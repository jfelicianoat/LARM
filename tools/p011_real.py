#!/usr/bin/env python3
"""P011 - Pruebas reales de GPU, NPU, backends y refresco en el PC IA.

  python tools/p011_real.py windows    sondeo del daemon frente a oráculos independientes
  python tools/p011_real.py sin-dll    con SystemRoot y PATH vacíos: lo buscado es ABSENT, lo probado sigue VERIFIED
  python tools/p011_real.py serve      POST /hardware/refresh, job, idempotencia y recuperación tras matar el daemon

Oráculos de `windows`, implementados aquí sin usar código de LARM:
  - Get-PnpDevice -PresentOnly (clases Display y ComputeAccelerator) y DEVPKEY_Device_DriverVersion;
  - registro HardwareInformation.qwMemorySize (memoria dedicada que declara el driver);
  - Vulkan por ctypes (vkCreateInstance + vkEnumeratePhysicalDevices + vkGetPhysicalDeviceProperties);
  - OpenCL por ctypes (clGetPlatformIDs + clGetDeviceIDs con CL_DEVICE_TYPE_GPU);
  - HIP por ctypes (hipGetDeviceCount);
  - sha256 con hashlib y versión de fichero con PowerShell de cada DLL declarada como dependencia.
"""
import ctypes
import hashlib
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P011_runs"
SYSTEM32 = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def validator(ref: str) -> Draft202012Validator:
    registry = Registry()
    for name in ("larm.schema.json", "domain.schema.json"):
        doc = json.loads((REPO / "docs/contracts/schemas" / name).read_text(encoding="utf-8"))
        registry = registry.with_resource(name, Resource.from_contents(doc))
    return Draft202012Validator({"$ref": ref}, registry=registry)


def schema_errors(ref: str, doc) -> list[str]:
    return [f"{'/'.join(map(str, e.path))}: {e.message}" for e in validator(ref).iter_errors(doc)]


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def daemon(*args: str, root: Path | None = None, env: dict | None = None) -> dict:
    cmd = [str(EXE)] + (["--data-root", str(root)] if root else []) + list(args)
    p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", env=env or clean_env(), timeout=180, check=False)
    if p.returncode != 0:
        raise AssertionError(f"{args[0]}: exit {p.returncode}: {p.stderr[-400:]}")
    return json.loads(p.stdout)


def powershell(script: str) -> str:
    return subprocess.run(["powershell", "-NoProfile", "-Command", script], capture_output=True, text=True,
                          encoding="utf-8", errors="replace", check=True, timeout=120).stdout


# --- Oráculos por ctypes -------------------------------------------------------------------------

class VkApplicationInfo(ctypes.Structure):
    _fields_ = [("sType", ctypes.c_int), ("pNext", ctypes.c_void_p), ("pApplicationName", ctypes.c_char_p),
                ("applicationVersion", ctypes.c_uint32), ("pEngineName", ctypes.c_char_p),
                ("engineVersion", ctypes.c_uint32), ("apiVersion", ctypes.c_uint32)]


class VkInstanceCreateInfo(ctypes.Structure):
    _fields_ = [("sType", ctypes.c_int), ("pNext", ctypes.c_void_p), ("flags", ctypes.c_uint32),
                ("pApplicationInfo", ctypes.POINTER(VkApplicationInfo)), ("enabledLayerCount", ctypes.c_uint32),
                ("ppEnabledLayerNames", ctypes.c_void_p), ("enabledExtensionCount", ctypes.c_uint32),
                ("ppEnabledExtensionNames", ctypes.c_void_p)]


def vulkan_devices() -> list[tuple[int, int, str]]:
    """(vendorID, deviceID, deviceName) de cada dispositivo físico, sin capas."""
    vk = ctypes.WinDLL(str(SYSTEM32 / "vulkan-1.dll"))
    app = VkApplicationInfo(0, None, b"p011", 1, b"p011", 1, (1 << 22) | (1 << 12))
    info = VkInstanceCreateInfo(1, None, 0, ctypes.pointer(app), 0, None, 0, None)
    instance = ctypes.c_void_p()
    if vk.vkCreateInstance(ctypes.byref(info), None, ctypes.byref(instance)) != 0:
        raise AssertionError("vkCreateInstance falló")
    try:
        count = ctypes.c_uint32()
        vk.vkEnumeratePhysicalDevices(instance, ctypes.byref(count), None)
        handles = (ctypes.c_void_p * count.value)()
        vk.vkEnumeratePhysicalDevices(instance, ctypes.byref(count), handles)
        out = []
        for h in handles:
            props = (ctypes.c_ubyte * 1024)()  # VkPhysicalDeviceProperties < 1 KiB en su cabecera
            vk.vkGetPhysicalDeviceProperties(ctypes.c_void_p(h), props)
            raw = bytes(props)
            vendor, device = int.from_bytes(raw[8:12], "little"), int.from_bytes(raw[12:16], "little")
            name = raw[20:276].split(b"\0", 1)[0].decode("utf-8", "replace")
            out.append((vendor, device, name))
        return out
    finally:
        vk.vkDestroyInstance(instance, None)


def opencl_gpus() -> int:
    cl = ctypes.WinDLL(str(SYSTEM32 / "OpenCL.dll"))
    n = ctypes.c_uint32()
    if cl.clGetPlatformIDs(0, None, ctypes.byref(n)) != 0 or n.value == 0:
        return 0
    platforms = (ctypes.c_void_p * n.value)()
    cl.clGetPlatformIDs(n.value, platforms, None)
    total = 0
    for p in platforms:
        d = ctypes.c_uint32()
        if cl.clGetDeviceIDs(ctypes.c_void_p(p), ctypes.c_uint64(1 << 2), 0, None, ctypes.byref(d)) == 0:
            total += d.value
    return total


def hip_devices(dll: Path) -> int:
    hip = ctypes.CDLL(str(dll))
    n = ctypes.c_int()
    code = hip.hipGetDeviceCount(ctypes.byref(n))
    return n.value if code == 0 else -code


def fingerprint(snapshot: dict) -> str:
    """Huella del contrato 04 implementada aparte: orden canónico, fuera
    observed_metrics y, por dispositivo, luid y evidence_refs; RFC 8785 (claves
    ASCII ordenadas, sin espacios, escape mínimo) y sha256."""
    s = json.loads(json.dumps(snapshot))
    s.pop("observed_metrics")
    s["cpu"]["instruction_sets"] = sorted(set(s["cpu"]["instruction_sets"]))
    for d in s["devices"]:
        d.pop("luid")
        d.pop("evidence_refs")
        d["backends"] = sorted(set(d["backends"]))
    key = lambda v: (v is not None, v or "")  # noqa: E731 - None antes que cualquier texto (Option de Rust)
    s["devices"].sort(key=lambda d: (d["kind"], key(d["device_id"]), key(d["vendor"])))
    s["dependencies"].sort(key=lambda d: (d["name"], key(d["version"]), key(d["hash"])))
    text = json.dumps(s, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def registry_memory_sizes() -> list[str]:
    """`HardwareInformation.qwMemorySize` de cada driver de la clase Display (winreg)."""
    import winreg
    out = []
    base = r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}"
    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, base) as cls:
        for i in range(winreg.QueryInfoKey(cls)[0]):
            name = winreg.EnumKey(cls, i)
            if not (len(name) == 4 and name.isdigit()):
                continue
            try:
                with winreg.OpenKey(cls, name) as key:
                    value, kind = winreg.QueryValueEx(key, "HardwareInformation.qwMemorySize")
            except OSError:
                continue
            out.append(str(int.from_bytes(value, "little") if kind == winreg.REG_BINARY else int(value)))
    return out


# --- Pruebas ---------------------------------------------------------------------------------------

def windows() -> str:
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    first = daemon("hardware-probe")
    (RUNS / "probe.json").write_text(json.dumps(first, indent=1, ensure_ascii=False), encoding="utf-8")
    snap, meta = first["snapshot"], first["metadata"]
    errors = schema_errors("domain.schema.json#/$defs/hardware_profile/properties/snapshot", snap)
    step(log, "schema-snapshot", not errors, errors[0] if errors else "snapshot con dispositivos conforme a domain.schema.json")

    pnp = json.loads(powershell(
        "@(Get-PnpDevice -PresentOnly | Where-Object { $_.Class -in 'Display','ComputeAccelerator' -and $_.InstanceId -like 'PCI\\*' } | "
        "ForEach-Object { [ordered]@{ class = $_.Class; id = $_.InstanceId; "
        "driver = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_DriverVersion).Data } }) | ConvertTo-Json"))
    pnp = pnp if isinstance(pnp, list) else [pnp]
    (RUNS / "pnp.json").write_text(json.dumps(pnp, indent=1), encoding="utf-8")
    expected = {(("GPU" if d["class"] == "Display" else "NPU"), d["id"].rsplit("\\", 1)[0].upper(), d["driver"]) for d in pnp}
    got = {(d["kind"], d["device_id"], d["driver_version"]) for d in snap["devices"]}
    step(log, "dispositivos-y-drivers", got == expected,
         f"{len(got)} dispositivos = Get-PnpDevice -PresentOnly: {sorted(expected)}")

    gpu = next(d for d in snap["devices"] if d["kind"] == "GPU")
    qw = registry_memory_sizes()
    step(log, "memoria-dedicada", gpu["dedicated_memory_bytes"] in qw,
         f"DXGI {gpu['dedicated_memory_bytes']} B = registro HardwareInformation.qwMemorySize {qw}")
    step(log, "memoria-unificada", gpu["unified_pool_id"] == "system-ram" and gpu["shared_memory_bytes"] is not None,
         f"UMA verificada por D3D12: memoria compartida {int(gpu['shared_memory_bytes']) / 2**30:.1f} GiB del pool system-ram")

    rows = {a["api"]: a for a in meta["accelerators"]}
    step(log, "matriz-12-apis", len(rows) == 12 and all(r["status"] in ("VERIFIED", "UNKNOWN", "ABSENT") for r in rows.values()),
         ", ".join(f"{k}={v['status']}" for k, v in rows.items()))

    vk = vulkan_devices()
    vk_match = [d for d in vk if f"VEN_{d[0]:04X}&DEV_{d[1]:04X}" in gpu["device_id"]]
    step(log, "vulkan-frente-a-ctypes", (rows["vulkan"]["status"] == "VERIFIED") == bool(vk_match) and ("vulkan" in gpu["backends"]) == bool(vk_match),
         f"vkEnumeratePhysicalDevices: {vk}; LARM {rows['vulkan']['status']}")
    cl = opencl_gpus()
    step(log, "opencl-frente-a-ctypes", (rows["opencl"]["status"] == "VERIFIED") == (cl > 0),
         f"clGetDeviceIDs(GPU) = {cl}; LARM {rows['opencl']['status']}")
    hip_dll = next((SYSTEM32 / n for n in ("amdhip64_7.dll", "amdhip64_6.dll", "amdhip64.dll") if (SYSTEM32 / n).is_file()), None)
    hip = hip_devices(hip_dll) if hip_dll else 0
    step(log, "hip-frente-a-ctypes", (rows["rocm"]["status"] == "VERIFIED") == (hip > 0) and ("rocm" in gpu["backends"]) == (hip == 1),
         f"hipGetDeviceCount({hip_dll.name if hip_dll else '-'}) = {hip}; LARM {rows['rocm']['status']} {rows['rocm']['version']}")
    step(log, "directml-y-winml", rows["directml"]["status"] == "VERIFIED" and rows["winml"]["status"] == "VERIFIED",
         f"{rows['directml']['evidence']}; {rows['winml']['evidence']}")

    path_dirs = [SYSTEM32] + [Path(p) for p in os.environ.get("PATH", "").split(os.pathsep) if p]
    for api, names in (("cuda", ["nvcuda.dll"]), ("qnn", ["QnnHtp.dll"]), ("openvino", ["openvino.dll"])):
        present = any((d / n).is_file() for d in path_dirs for n in names)
        step(log, f"{api}-ausencia", (rows[api]["status"] == "ABSENT") == (not present),
             f"{names} en System32/PATH: {present}; LARM {rows[api]['status']}")

    deps = snap["dependencies"]
    bad = []
    for comp in [c for r in rows.values() for c in r["components"]]:
        path = Path(comp["path"])
        sha = hashlib.sha256(path.read_bytes()).hexdigest()
        ver = powershell(f"$v = (Get-Item '{path}').VersionInfo; '{{0}}.{{1}}.{{2}}.{{3}}' -f $v.FileMajorPart, $v.FileMinorPart, $v.FileBuildPart, $v.FilePrivatePart").strip()
        dep = next((d for d in deps if d["name"] == path.name.lower()), None)
        if not dep or dep["hash"] != sha or dep["version"] != ver or comp["sha256"] != sha:
            bad.append(f"{path.name}: LARM {dep} / hashlib {sha[:12]} / VersionInfo {ver}")
    step(log, "dependencias-hash-y-version", not bad and len(deps) >= 1,
         bad[0] if bad else f"{len(deps)} DLL: sha256 (hashlib) y versión (VersionInfo) idénticos")

    second = daemon("hardware-probe")
    step(log, "huella-reproducible", second["hardware_fingerprint"] == first["hardware_fingerprint"],
         f"dos sondeos con dispositivos -> {first['hardware_fingerprint'][:16]}…")
    ours = fingerprint(snap)
    moved = json.loads(json.dumps(snap))
    for d in moved["devices"]:
        d["luid"] = "ffffffff-ffffffff"
        d["evidence_refs"] = ["otra-sesion"]
    moved["devices"].reverse()
    moved["observed_metrics"] = {"available_ram_bytes": "1", "cpu_usage_percent": 99.0}
    newer = json.loads(json.dumps(snap))
    newer["devices"][0]["driver_version"] = "99.0.0.0"
    step(log, "huella-recalculada", ours == first["hardware_fingerprint"] and fingerprint(moved) == ours and fingerprint(newer) != ours,
         "RFC 8785 + sha256 recalculado en Python = huella de LARM; otro LUID, otro orden o métricas no la cambian; otro driver sí")
    step(log, "sin-desconocidos", meta["unknown"] == [], "ningún componente quedó desconocido en este equipo")
    (RUNS / "windows-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} comprobaciones frente a oráculos independientes PASS"


def sin_dll() -> str:
    """Búsqueda sin carpetas: SystemRoot y PATH apuntan a carpetas vacías."""
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    with tempfile.TemporaryDirectory() as empty:
        (Path(empty) / "System32").mkdir()
        env = clean_env() | {"SystemRoot": empty, "PATH": empty}
        probe = daemon("hardware-probe", env=env)
    (RUNS / "sin-dll.json").write_text(json.dumps(probe, indent=1, ensure_ascii=False), encoding="utf-8")
    rows = {a["api"]: a for a in probe["metadata"]["accelerators"]}
    searched = ["vulkan", "opencl", "cuda", "sycl", "vitisai", "migraphx", "openvino", "qnn", "tensorrt"]
    step(log, "buscadas-absent", all(rows[a]["status"] == "ABSENT" for a in searched),
         ", ".join(f"{a}={rows[a]['status']}" for a in searched))
    step(log, "probadas-siguen-verified", all(rows[a]["status"] == "VERIFIED" for a in ("directml", "winml", "rocm")),
         "DirectML, Windows ML y HIP se cargan desde el directorio de sistema real: la prueba manda sobre la búsqueda")
    gpu = next(d for d in probe["snapshot"]["devices"] if d["kind"] == "GPU")
    step(log, "backends-sin-vulkan-ni-opencl", "vulkan" not in gpu["backends"] and "opencl" not in gpu["backends"]
         and probe["snapshot"]["dependencies"] == [], f"backends {gpu['backends']}; dependencias []")
    step(log, "huella-distinta", probe["hardware_fingerprint"] != daemon("hardware-probe")["hardware_fingerprint"],
         "sin componentes de runtime la huella cambia (contrato 04: dependencias en la huella)")
    (RUNS / "sin-dll-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


def http(method: str, url: str, token: str | None, body: dict | None = None, key: str | None = None) -> tuple[int, dict | None]:
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if token:
        req.add_header("authorization", f"Bearer {token}")
    if data is not None:
        req.add_header("content-type", "application/json")
    if key:
        req.add_header("idempotency-key", key)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            raw = r.read()
            return r.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, json.loads(raw) if raw else None


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def start(root: Path, port: int) -> subprocess.Popen:
    proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"], env=clean_env(),
                            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            if http("GET", f"http://127.0.0.1:{port}/api/v1/health/live", None)[0] == 200:
                return proc
        except OSError:
            time.sleep(0.2)
    raise AssertionError("el daemon no arrancó")


def stop(proc: subprocess.Popen) -> None:
    os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
    try:
        proc.wait(timeout=20)
    except subprocess.TimeoutExpired:
        proc.kill()


def wait_job(base: str, token: str, job_id: str, timeout: float = 60) -> dict:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        status, body = http("GET", f"{base}/jobs/{job_id}", token)
        if status == 200 and body["state"] in ("COMPLETED", "FAILED"):
            return body
        time.sleep(0.3)
    raise AssertionError(f"el job {job_id} no terminó")


def serve() -> str:
    root = RUNS / "real"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    log: list = []
    proc = None
    nulls = {"resource_id": None, "profile_id": None}
    try:
        admin = daemon("enroll-principal", "--name", "p011-admin", "--kind", "OPERATOR", "--scopes", "admin", root=root)["token"]
        reader = daemon("enroll-principal", "--name", "p011-reader", "--kind", "OPERATOR", "--scopes", "read", root=root)["token"]
        port = free_port()
        base = f"http://127.0.0.1:{port}/api/v1"
        proc = start(root, port)

        status, receipt = http("POST", f"{base}/hardware/refresh", admin, nulls, "p011-a")
        errors = schema_errors("larm.schema.json#/$defs/JobReceipt", receipt) if status == 202 else ["no 202"]
        step(log, "refresh-202", status == 202 and not errors, errors[0] if errors else f"202 JobReceipt {receipt['state']}")
        status2, other = http("POST", f"{base}/hardware/refresh", admin, nulls, "p011-b")
        step(log, "un-solo-sondeo-a-la-vez", status2 == 202 and other["job_id"] == receipt["job_id"],
             f"segunda petición con otra clave mientras sondea -> el mismo job {other['job_id'][:13]}…")
        replay = http("POST", f"{base}/hardware/refresh", admin, nulls, "p011-a")
        step(log, "idempotencia", replay == (202, receipt), "misma clave y cuerpo -> misma respuesta")
        # Cuerpo válido y equivalente, pero con otros bytes (claves en otro orden).
        conflict = http("POST", f"{base}/hardware/refresh", admin, {"profile_id": None, "resource_id": None}, "p011-a")
        step(log, "idempotencia-conflicto", conflict[0] == 409 and conflict[1]["code"] == "IDEMPOTENCY_CONFLICT",
             f"misma clave, otros bytes -> {conflict[0]} {conflict[1]['code']}")

        job = wait_job(base, admin, receipt["job_id"])
        errors = schema_errors("larm.schema.json#/$defs/JobState", job)
        step(log, "job-completado", job["state"] == "COMPLETED" and not errors and job["result"]["hardware_profile_id"] == job["resource_id"],
             errors[0] if errors else f"COMPLETED -> perfil {job['resource_id'][:13]}… row_version {job['result']['row_version']}")
        status, page = http("GET", f"{base}/hardware", reader)
        latest = page["items"][-1]
        step(log, "perfil-con-dispositivos", latest["id"] == job["resource_id"] and len(latest["snapshot"]["devices"]) >= 1
             and len(latest["metadata"]["accelerators"]) == 12 and not schema_errors("larm.schema.json#/$defs/hardware_profileList", page),
             f"{len(latest['snapshot']['devices'])} dispositivos y 12 APIs en GET /hardware, conforme a hardware_profileList")
        forbidden = http("POST", f"{base}/hardware/refresh", reader, nulls, "p011-c")
        hidden = http("GET", f"{base}/jobs/{receipt['job_id']}", reader)
        step(log, "permisos", forbidden[0] == 403 and hidden[0] == 404, f"lector: refresh {forbidden[0]}, job ajeno {hidden[0]}")

        # Un refresco en curso cuando el daemon muere queda FAILED (job_interrupted) al volver.
        status, interrupted = http("POST", f"{base}/hardware/refresh", admin, nulls, "p011-d")
        proc.kill()
        proc.wait(timeout=20)
        proc = start(root, port)
        job = wait_job(base, admin, interrupted["job_id"], timeout=10)
        step(log, "recuperacion-tras-matar", interrupted["state"] in ("QUEUED", "RUNNING") and job["state"] == "FAILED"
             and job["error"]["details"]["reason"] == "job_interrupted" and job["error"]["retryable"] is True,
             f"daemon matado con el job {interrupted['state']} -> al volver {job['state']} "
             f"{(job['error'] or {}).get('details', {}).get('reason', '')}")
        stop(proc)
        proc = None
    finally:
        if proc is not None and proc.poll() is None:
            proc.kill()
        for name in ("p011-admin", "p011-reader"):
            try:
                daemon("revoke-principal", "--name", name, root=root)
            except AssertionError:
                pass
        (root / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"windows": windows, "sin-dll": sin_dll, "serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, StopIteration, subprocess.SubprocessError) as e:
        print(f"FAIL {e}")
        raise SystemExit(1)
