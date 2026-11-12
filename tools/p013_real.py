#!/usr/bin/env python3
"""P013 - Huellas reproducibles: oráculos independientes en el PC IA.

  python tools/p013_real.py launch     profile_hash de LARM = RFC 8785 + sha256 implementado aquí
  python tools/p013_real.py hardware   huella de hardware reproducible entre procesos y recalculada aquí
  python tools/p013_real.py serve      versión y diff persistidos por el daemon real

`launch` genera variantes del fixture tests/fixtures/launch/balanced.json (solo
argumentos, opciones, entorno, límite o generación cambian; o solo el orden de las
propiedades) y compara el hash de `larm-daemon launch-profile-hash` con el de este
script, que implementa JCS por su cuenta: claves por unidades UTF-16 y números con el
algoritmo Number::toString de ECMAScript a partir de repr() (dígitos mínimos).
"""
import copy
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
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P013_runs"
FIXTURE = REPO / "tests/fixtures/launch/balanced.json"


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("LARM_")}


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


# --- JCS independiente -----------------------------------------------------------------------------

def es_number(x: float) -> str:
    if x != x or x in (float("inf"), float("-inf")):
        raise ValueError("no finito")
    if x == 0:
        return "0"
    if x < 0:
        return "-" + es_number(-x)
    mantissa, _, exp = f"{x!r}".lower().partition("e")
    digits_point = mantissa
    if "." in digits_point:
        int_part, frac_part = digits_point.split(".")
    else:
        int_part, frac_part = digits_point, ""
    exponent = int(exp) if exp else 0
    digits = (int_part + frac_part).lstrip("0")
    # posición del punto decimal respecto a los dígitos significativos
    lead_zeros = len(int_part + frac_part) - len((int_part + frac_part).lstrip("0"))
    n = len(int_part) - lead_zeros + exponent
    digits = digits.rstrip("0") or "0"
    k = len(digits)
    if k <= n <= 21:
        return digits + "0" * (n - k)
    if 0 < n <= 21:
        return digits[:n] + "." + digits[n:]
    if -6 < n <= 0:
        return "0." + "0" * (-n) + digits
    e = n - 1
    sign = "+" if e >= 0 else "-"
    return (digits[0] + ("." + digits[1:] if k > 1 else "")) + "e" + sign + str(abs(e))


def jcs(value) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        if abs(value) > 2**53:
            raise ValueError("entero fuera del rango exacto de un doble")
        return str(value)
    if isinstance(value, float):
        return es_number(value)
    if isinstance(value, str):
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, list):
        return "[" + ",".join(jcs(v) for v in value) + "]"
    if isinstance(value, dict):
        keys = sorted(value, key=lambda k: k.encode("utf-16-be"))
        return "{" + ",".join(json.dumps(k, ensure_ascii=False) + ":" + jcs(value[k]) for k in keys) + "}"
    raise TypeError(type(value))


def sha(value) -> str:
    return hashlib.sha256(jcs(value).encode("utf-8")).hexdigest()


def larm_hash(params: dict) -> dict:
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
        json.dump(params, f, ensure_ascii=False)
        path = f.name
    try:
        p = subprocess.run([str(EXE), "launch-profile-hash", "--file", path], capture_output=True, env=clean_env(), timeout=60)
        return json.loads(p.stdout.decode("utf-8"))
    finally:
        os.unlink(path)


def reordered(value):
    if isinstance(value, dict):
        return {k: reordered(value[k]) for k in reversed(list(value))}
    if isinstance(value, list):
        return [reordered(v) for v in value]
    return value


def launch() -> str:
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    base = json.loads(FIXTURE.read_text(encoding="utf-8"))
    ours = sha(base)
    theirs = larm_hash(base)
    step(log, "misma-huella-que-el-oraculo", theirs == {"valid": True, "profile_hash": ours},
         f"{ours[:16]}… = JCS de Python (números ECMAScript: 0.7, 1 por 1.0, 0.000001, 1e+21; claves UTF-16 con ñ y €)")
    step(log, "orden-de-propiedades", larm_hash(reordered(base))["profile_hash"] == ours, "todas las claves en orden inverso: mismo hash")
    variants = {
        "argumento": lambda p: p["runtime_arguments"].append("--verbose"),
        "orden-de-argumentos": lambda p: p["runtime_arguments"].reverse(),
        "opcion": lambda p: p["adapter_options"].__setitem__("mlock", True),
        "opcion-anidada": lambda p: p["adapter_options"]["rope"].__setitem__("scale", 1.5),
        "entorno-valor": lambda p: p["environment"]["variables"][0].__setitem__("value", "11.0.0"),
        "entorno-referencia": lambda p: p["environment"]["variables"][1].__setitem__("credential_reference", "larm/source/hf2"),
        "limite": lambda p: p.__setitem__("memory_limit_bytes", "1073741824"),
        "sin-limite": lambda p: p.__setitem__("memory_limit_bytes", None),
        "temperatura": lambda p: p["generation"].__setitem__("temperature", 0.71),
    }
    seen = {ours}
    for name, edit in variants.items():
        p = copy.deepcopy(base)
        edit(p)
        got = larm_hash(p)
        step(log, f"cambio-{name}", got.get("profile_hash") == sha(p) and got["profile_hash"] not in seen,
             f"{got.get('profile_hash', got)[:16]}… distinto y = oráculo")
        seen.add(got["profile_hash"])
    rejects = {
        "memory_limit_zero": lambda p: p.__setitem__("memory_limit_bytes", "0"),
        "env_not_allowed": lambda p: p["environment"]["variables"].append({"name": "OTRA", "value": "x"}),
        "env_duplicate": lambda p: (p["environment"]["allowlist"].append("Hf_Token2"),
                                    p["environment"]["variables"].append({"name": "hf_token", "value": "x"})),
        "credential_reference_invalid": lambda p: p["environment"]["variables"][1].__setitem__("credential_reference", "otro/x"),
        "schema_violation": lambda p: p.__setitem__("extra", 1),
    }
    for reason, edit in rejects.items():
        p = copy.deepcopy(base)
        edit(p)
        got = larm_hash(p)
        step(log, f"rechazo-{reason}", got.get("valid") is False and got.get("reason") in (reason, "schema_violation", "env_not_allowed"),
             f"{got.get('reason')}: {got.get('detail', '')[:80]}")
    (RUNS / "launch-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} comprobaciones PASS"


def hardware_fingerprint(snapshot: dict) -> str:
    s = copy.deepcopy(snapshot)
    s.pop("observed_metrics")
    s["cpu"]["instruction_sets"] = sorted(set(s["cpu"]["instruction_sets"]))
    for d in s["devices"]:
        d.pop("luid")
        d.pop("evidence_refs")
        d["backends"] = sorted(set(d["backends"]))
    key = lambda v: (v is not None, v or "")  # noqa: E731
    s["devices"].sort(key=lambda d: (d["kind"], key(d["device_id"]), key(d["vendor"])))
    s["dependencies"].sort(key=lambda d: (d["name"], key(d["version"]), key(d["hash"])))
    return sha(s)


def probe() -> dict:
    p = subprocess.run([str(EXE), "hardware-probe"], capture_output=True, env=clean_env(), timeout=180, check=True)
    return json.loads(p.stdout.decode("utf-8"))


def hardware() -> str:
    RUNS.mkdir(parents=True, exist_ok=True)
    log: list = []
    a, b = probe(), probe()
    step(log, "reproducible-entre-procesos", a["hardware_fingerprint"] == b["hardware_fingerprint"],
         f"dos procesos -> {a['hardware_fingerprint'][:16]}…; RAM libre {a['snapshot']['observed_metrics']['available_ram_bytes']} vs "
         f"{b['snapshot']['observed_metrics']['available_ram_bytes']}")
    step(log, "oraculo-python", hardware_fingerprint(a["snapshot"]) == a["hardware_fingerprint"], "JCS + sha256 de Python = huella de LARM")
    edited = copy.deepcopy(a["snapshot"])
    gpu = next(d for d in edited["devices"] if d["kind"] == "GPU")
    gpu["driver_version"] = "99.0.0.1"
    step(log, "driver-cambia-la-huella", hardware_fingerprint(edited) != a["hardware_fingerprint"], "otro driver -> otra huella")
    edited = copy.deepcopy(a["snapshot"])
    edited["observed_metrics"]["available_ram_bytes"] = "1"
    for d in edited["devices"]:
        d["luid"] = "ffffffff-ffffffff"
    edited["devices"].reverse()
    step(log, "volatiles-no-cuentan", hardware_fingerprint(edited) == a["hardware_fingerprint"], "RAM libre, LUID y orden: misma huella")
    (RUNS / "hardware-steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} comprobaciones PASS"


def serve() -> str:
    root = RUNS / "real"
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)
    log: list = []
    token = json.loads(subprocess.run([str(EXE), "--data-root", str(root), "enroll-principal", "--name", "p013", "--kind", "OPERATOR",
                                       "--scopes", "read"], capture_output=True, env=clean_env(), check=True).stdout)["token"]
    profiles = []
    try:
        for _ in range(2):
            with socket.socket() as s:
                s.bind(("127.0.0.1", 0))
                port = s.getsockname()[1]
            proc = subprocess.Popen([str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"], env=clean_env(),
                                    creationflags=subprocess.CREATE_NEW_PROCESS_GROUP, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                deadline, items = time.monotonic() + 90, []
                while time.monotonic() < deadline:
                    try:
                        req = urllib.request.Request(f"http://127.0.0.1:{port}/api/v1/hardware", headers={"authorization": f"Bearer {token}"})
                        with urllib.request.urlopen(req, timeout=10) as r:
                            items = json.loads(r.read())["items"]
                        if items and items[-1]["row_version"] >= len(profiles) + 1:
                            break
                    except OSError:
                        pass
                    time.sleep(0.5)
                profiles.append(items)
            finally:
                os.kill(proc.pid, signal.CTRL_BREAK_EVENT)
                proc.wait(timeout=30)
        first, second = profiles[0][-1], profiles[1][-1]
        f = first["metadata"]["fingerprint"]
        step(log, "version-y-primer-perfil", f["version"] == 1 and f["previous_profile_id"] is None and f["changes"] == [],
             "fingerprint_version 1; primer perfil sin anterior ni cambios")
        step(log, "rearranque-misma-huella", len(profiles[1]) == 1 and second["id"] == first["id"] and second["row_version"] == 2
             and second["metadata"]["fingerprint"] == f and second["hardware_fingerprint"] == first["hardware_fingerprint"],
             "rearranque del daemon: mismo perfil, row_version 2 y diff conservado")
    finally:
        subprocess.run([str(EXE), "--data-root", str(root), "revoke-principal", "--name", "p013"], capture_output=True, env=clean_env())
        (root / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"launch": launch, "hardware": hardware, "serve": serve}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, TypeError, subprocess.SubprocessError) as e:
        print(f"FAIL {e}")
        raise SystemExit(1)
