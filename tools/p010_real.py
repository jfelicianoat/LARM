#!/usr/bin/env python3
"""P010 - Pruebas reales del Hardware Profiler (OS, CPU y memoria) en el PC IA.

  python tools/p010_real.py windows   el sondeo del daemon frente a fuentes independientes
  python tools/p010_real.py serve     sondeo al arrancar, persistencia y GET /api/v1/hardware

`windows` compara `larm-daemon hardware-probe` con lo que Windows informa por otras
vías: CIM (Win32_ComputerSystem, Win32_Processor, Win32_PhysicalMemory,
Win32_PageFileUsage, Win32_OperatingSystem), el registro leído con reg.exe y
IsProcessorFeaturePresent (kernel32) para las extensiones AVX. Además valida el
snapshot contra domain.schema.json y que la huella se repita en dos sondeos.

`serve` arranca el daemon real sobre una raíz de prueba (docs/reports/P010_runs/real),
espera el perfil registrado al arrancar, lo lee por la API con un principal `read`,
valida la respuesta contra larm.schema.json#/$defs/hardware_profileList y comprueba
que un segundo arranque con el mismo hardware actualiza ese perfil (row_version 2)
en vez de crear otro.
"""
import ctypes
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P010_runs"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def validator(ref: str) -> Draft202012Validator:
    registry = Registry()
    for name in ("larm.schema.json", "domain.schema.json"):
        doc = json.loads((REPO / "docs/contracts/schemas" / name).read_text(encoding="utf-8"))
        registry = registry.with_resource(name, Resource.from_contents(doc))
    return Draft202012Validator({"$ref": ref}, registry=registry)


def schema_errors(v: Draft202012Validator, doc) -> list[str]:
    return [f"{'/'.join(map(str, e.path))}: {e.message}" for e in v.iter_errors(doc)]


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def daemon(*args: str, root: Path | None = None) -> dict:
    cmd = [str(EXE)] + (["--data-root", str(root)] if root else []) + list(args)
    p = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", env=clean_env(), timeout=120, check=False)
    if p.returncode != 0:
        raise AssertionError(f"{args[0]}: exit {p.returncode}: {p.stderr[-400:]}")
    return json.loads(p.stdout)


def cim() -> dict:
    script = (
        "$cs = Get-CimInstance Win32_ComputerSystem; $os = Get-CimInstance Win32_OperatingSystem; "
        "$cpu = @(Get-CimInstance Win32_Processor); "
        "[ordered]@{ logical = $cs.NumberOfLogicalProcessors; "
        "cores = ($cpu | Measure-Object NumberOfCores -Sum).Sum; "
        "vendor = $cpu[0].Manufacturer; name = $cpu[0].Name; "
        "total_physical = [string]$cs.TotalPhysicalMemory; "
        "installed = [string](Get-CimInstance Win32_PhysicalMemory | Measure-Object Capacity -Sum).Sum; "
        "pagefile_mb = (Get-CimInstance Win32_PageFileUsage | Measure-Object AllocatedBaseSize -Sum).Sum; "
        "free_kb = [string]$os.FreePhysicalMemory; build = $os.BuildNumber } | ConvertTo-Json"
    )
    out = subprocess.run(["powershell", "-NoProfile", "-Command", script], capture_output=True, text=True,
                         encoding="utf-8", check=True, timeout=60).stdout
    return json.loads(out)


def reg(key: str, name: str) -> str:
    out = subprocess.run(["reg", "query", key, "/v", name], capture_output=True, text=True, check=True,
                         encoding="cp1252", errors="replace").stdout
    for line in out.splitlines():
        parts = line.split()
        if parts and parts[0] == name:
            value = parts[-1]
            return str(int(value, 16)) if parts[1] == "REG_DWORD" else " ".join(parts[2:])
    raise AssertionError(f"{key}\\{name} no encontrado")


# IsProcessorFeaturePresent: PF_SSSE3=36, PF_SSE4_1=37, PF_SSE4_2=38, PF_AVX=39, PF_AVX2=40, PF_AVX512F=41.
PF = {"ssse3": 36, "sse4.1": 37, "sse4.2": 38, "avx": 39, "avx2": 40, "avx512f": 41}


def windows() -> str:
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    first = daemon("hardware-probe")
    (RUNS / "probe.json").write_text(json.dumps(first, indent=2, ensure_ascii=False), encoding="utf-8")
    snap = first["snapshot"]
    errors = schema_errors(validator("domain.schema.json#/$defs/hardware_profile/properties/snapshot"), snap)
    step(log, "schema-snapshot", not errors, errors[0] if errors else "snapshot conforme a domain.schema.json")

    ref = cim()
    (RUNS / "windows.json").write_text(json.dumps(ref, indent=2, ensure_ascii=False), encoding="utf-8")
    cpu, ram, os_ = snap["cpu"], snap["ram"], snap["os"]
    step(log, "cpu-nucleos", (cpu["logical_cores"], cpu["physical_cores"]) == (ref["logical"], ref["cores"]),
         f"lógicos {cpu['logical_cores']} / físicos {cpu['physical_cores']}; CIM {ref['logical']} / {ref['cores']}")
    step(log, "cpu-identidad", cpu["vendor"] == ref["vendor"] and cpu["model"] == ref["name"].strip(),
         f"{cpu['vendor']} «{cpu['model']}»")

    kernel32 = ctypes.WinDLL("kernel32")
    expected = {f for f, pf in PF.items() if kernel32.IsProcessorFeaturePresent(pf)}
    got = {f for f in PF if f in cpu["instruction_sets"]}
    step(log, "isa-frente-a-windows", got == expected,
         f"IsProcessorFeaturePresent {sorted(expected)}; sondeo {sorted(got)}")

    step(log, "ram-visible", ram["usable_bytes"] == ref["total_physical"],
         f"usable {ram['usable_bytes']} = Win32_ComputerSystem.TotalPhysicalMemory {ref['total_physical']}")
    step(log, "ram-instalada", ram["physical_bytes"] == ref["installed"],
         f"physical {ram['physical_bytes']} = suma de Win32_PhysicalMemory {ref['installed']}")
    usable, physical = int(ram["usable_bytes"]), int(ram["physical_bytes"])
    available = int(snap["observed_metrics"]["available_ram_bytes"])
    step(log, "carve-out-no-sumado", available <= usable < physical and ram["pool_id"] == "system-ram",
         f"disponible {available / 2**30:.1f} GiB ≤ visible {usable / 2**30:.1f} GiB < instalada "
         f"{physical / 2**30:.0f} GiB: la VRAM reservada no se cuenta como RAM")
    free = int(ref["free_kb"]) * 1024
    step(log, "ram-disponible-plausible", abs(available - free) < 2 * 2**30,
         f"sondeo {available / 2**30:.2f} GiB vs FreePhysicalMemory {free / 2**30:.2f} GiB (memoria viva, ±2 GiB)")
    pagefile = int(first["metadata"]["pagefile"]["total_bytes"])
    step(log, "pagefile", pagefile == int(ref["pagefile_mb"]) * 2**20,
         f"{pagefile} B = Win32_PageFileUsage {ref['pagefile_mb']} MiB")

    cv = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion"
    build = f"{reg(cv, 'CurrentBuild')}.{reg(cv, 'UBR')}"
    arch = reg(r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "PROCESSOR_ARCHITECTURE")
    step(log, "os", os_ == {"edition": reg(cv, "EditionID"), "build": build, "architecture": arch}
         and build.startswith(str(ref["build"]) + "."),
         f"{os_['edition']} {os_['build']} {os_['architecture']} (reg.exe y Win32_OperatingSystem {ref['build']})")
    step(log, "sin-desconocidos", first["metadata"]["unknown"] == [],
         "todos los componentes de OS/CPU/RAM observados en este equipo")

    second = daemon("hardware-probe")
    step(log, "huella-reproducible", second["hardware_fingerprint"] == first["hardware_fingerprint"]
         and len(first["hardware_fingerprint"]) == 64,
         f"dos sondeos -> {first['hardware_fingerprint'][:16]}… aunque cambien RAM libre y uso de CPU")
    (RUNS / "windows-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} comprobaciones frente a Windows PASS"


def http(url: str, token: str | None) -> tuple[int, dict | None]:
    req = urllib.request.Request(url)
    if token:
        req.add_header("authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        raw = e.read()
        return e.code, json.loads(raw) if raw else None


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def run_daemon(root: Path, port: int, reader: str, wanted_version: int) -> dict:
    proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"],
                            env=clean_env() | {"LARM_LOG_FORMAT": "json"},
                            creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    base = f"http://127.0.0.1:{port}/api/v1/hardware"
    try:
        deadline, body = time.monotonic() + 60, None
        while time.monotonic() < deadline:
            try:
                status, body = http(base, reader)
                if status == 200 and body["items"] and body["items"][-1]["row_version"] >= wanted_version:
                    break
            except OSError:
                pass
            time.sleep(0.3)
        return {"body": body, "unauth": http(base, None)[0]}
    finally:
        os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()


def serve() -> str:
    root = RUNS / "real"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    log: list = []
    try:
        reader = daemon("enroll-principal", "--name", "p010-reader", "--kind", "OPERATOR", "--scopes", "read",
                        root=root)["token"]
        writer = daemon("enroll-principal", "--name", "p010-writer", "--kind", "OPERATOR",
                        "--scopes", "models:write", root=root)["token"]
        port = free_port()
        first = run_daemon(root, port, reader, 1)
        body = first["body"]
        errors = schema_errors(validator("larm.schema.json#/$defs/hardware_profileList"), body)
        step(log, "get-hardware", body is not None and len(body["items"]) == 1 and not errors,
             errors[0] if errors else "200 con un perfil, conforme a hardware_profileList")
        step(log, "sin-token", first["unauth"] == 401, f"sin Bearer -> {first['unauth']}")
        profile = body["items"][0]
        meta = profile["metadata"]
        step(log, "procedencia", meta["profiler"]["name"] == "larm-hardware" and "ram.physical_bytes" in meta["sources"]
             and profile["evidence_refs"], f"profiler {meta['profiler']}; {len(meta['sources'])} fuentes")

        second = run_daemon(root, free_port(), reader, 2)
        items = second["body"]["items"]
        step(log, "rearranque-mismo-perfil", len(items) == 1 and items[0]["id"] == profile["id"]
             and items[0]["row_version"] == 2 and items[0]["hardware_fingerprint"] == profile["hardware_fingerprint"]
             and items[0]["probed_at"] > profile["probed_at"],
             f"mismo id, row_version {items[0]['row_version']}, probed_at {profile['probed_at']} -> {items[0]['probed_at']}")

        port = free_port()
        proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"],
                                env=clean_env(), creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            base = f"http://127.0.0.1:{port}/api/v1/hardware"
            for _ in range(100):
                try:
                    http(base, reader)
                    break
                except OSError:
                    time.sleep(0.3)
            forbidden = http(base, writer)
            bad = http(base + "?limit=0", reader)
            filt = http(base + "?q=x", reader)
        finally:
            os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
            proc.wait(timeout=20)
        step(log, "rechazos", forbidden[0] == 403 and bad[0] == 400 and filt[0] == 400
             and filt[1]["details"]["reason"] == "filter_not_allowed",
             f"sin scope read {forbidden[0]}; limit=0 {bad[0]}; filtro q {filt[0]}")
    finally:
        for name in ("p010-reader", "p010-writer"):
            try:
                daemon("revoke-principal", "--name", name, root=root)
            except AssertionError:
                pass
        (root / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"windows": windows, "serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, subprocess.SubprocessError) as e:
        print(f"FAIL {e}")
        raise SystemExit(1)
