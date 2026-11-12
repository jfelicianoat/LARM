#!/usr/bin/env python3
"""Referencia Rust con proceso echo HTTP real y runner común reutilizable."""
import copy
import hashlib
import importlib.util
import json
import os
import platform
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "tools/adapter-conformance"))
from runner import common, executable  # noqa: E402
from session import Session  # noqa: E402
from p023_real import free_port, launch_request, port_open, start, stop, http  # noqa: E402
from p043_sources_real import cli, EXE  # noqa: E402


def load_factory():
    spec = importlib.util.spec_from_file_location("reference_runtime_package", REPO / "adapters/runtime/reference/package.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def echo(port, value):
    data = json.dumps(value).encode()
    request = urllib.request.Request(f"http://127.0.0.1:{port}/echo", data=data, headers={"content-type": "application/json"})
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.status, json.loads(response.read())


def children(pid):
    # Sólo PID y nombre de los hijos del worker creado por esta prueba.
    command = f"@(Get-CimInstance Win32_Process -Filter 'ParentProcessId = {int(pid)}' | Select-Object ProcessId,Name) | ConvertTo-Json -Compress"
    result = subprocess.run(["powershell", "-NoProfile", "-Command", command], capture_output=True, timeout=15, check=True)
    data = json.loads(result.stdout) if result.stdout.strip() else []
    return data if isinstance(data, list) else [data]


def main():
    run = REPO / "docs/reports/P025_runs/runtime-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    factory = load_factory()
    package, manifest, model = factory.create(REPO, run / "package", version_delay_ms=3000)
    initial = {p.relative_to(package).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in package.rglob("*") if p.is_file()}
    steps, session, success, daemon = [], None, False, None
    principal_name = f"p025-runtime-{run.name}"
    enrolled = False
    def step(name, condition, detail):
        assert condition, name
        steps.append({"id": name, "result": "PASS", "detail": detail})
        print(f"PASS {name}", flush=True)
    try:
        observed = subprocess.run([str(package / "bin/echo.exe"), "--version"], capture_output=True, timeout=5, check=True).stdout.decode().strip()
        session = Session(executable(package, manifest), manifest, cwd=package)
        params = {"runtime_id": str(uuid.uuid7()), "channel": "custom", "cursor": None}
        session.call("runtime.list_versions", params)  # antes del handshake: ningún probe
        baseline_children = children(session.process.pid)
        assert all(child["Name"] == "conhost.exe" for child in baseline_children), "hijo inesperado antes del probe"
        cases = {"valid_method": "runtime.list_versions", "valid_params": params, "cancel_method": "runtime.list_versions", "cancel_params": params}
        (run / "common-cases.json").write_text(json.dumps(cases, indent=2), encoding="utf-8")
        steps.extend(common(session, **cases))
        remaining = children(session.process.pid)
        assert remaining == baseline_children, f"sondeo cambió hijos: antes={baseline_children}; después={remaining}"
        step("version-probe-no-surviving-child", remaining == baseline_children, "los hijos vuelven al estado previo: sólo permanece la consola propia del worker, que ya existía antes del sondeo")
        versions = session.result("runtime.list_versions", params)
        version = versions["items"][0]
        step("version-and-binary-hash", version["version_string"] == observed and version["artifact_hash"] == initial["bin/echo.exe"] and version["channel"] == "custom", "versión observada sin SDK y SHA256 independiente de los bytes; no infiere canal stable")
        unsupported = session.call("runtime.detect_backends", {"installation_id": str(uuid.uuid7())})
        step("unimplemented-is-not-success", unsupported.get("error", {}).get("data", {}).get("code") == "UNSUPPORTED_CAPABILITY", "no convierte una capacidad ausente en un backend inventado")
        port = free_port()
        request = launch_request(port, str(model), {"allowlist": [], "variables": []})
        invalid = copy.deepcopy(request)
        invalid["launch_profile"]["adapter_options"]["model_path"] = str(package / "models/missing.bin")
        failed = session.call("runtime.launch_plan", invalid)
        step("invalid-model-no-process", failed.get("error", {}).get("data", {}).get("details", {}).get("reason") == "path_missing" and not port_open(port), "archivo inexistente rechazado antes de abrir puerto/proceso")
        limited = copy.deepcopy(request)
        limited["launch_profile"]["memory_limit_bytes"] = "1024"
        failed = session.call("runtime.launch_plan", limited)
        step("unsupported-memory-limit", failed.get("error", {}).get("data", {}).get("details", {}).get("reason") == "memory_limit_unsupported" and not port_open(port), "no afirma aplicar un límite que el runtime no implementa")
        plan = session.result("runtime.launch_plan", request)
        unsigned = {key: value for key, value in plan.items() if key != "plan_hash"}
        calculated = hashlib.sha256(json.dumps(unsigned, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()).hexdigest()
        step("launch-plan-real-hash", calculated == plan["plan_hash"] and plan["memory_estimate"]["confidence"] == "UNKNOWN", "hash independiente, configuración efectiva y memoria desconocida preservada")
        bad = session.call("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": "f" * 64, "approval_id": None})
        step("changed-plan-no-process", bad.get("error", {}).get("data", {}).get("details", {}).get("reason") == "plan_hash_mismatch" and not port_open(port), "plan alterado no inicia un proceso")
        handle = session.result("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        ref = {"instance_id": handle["instance_id"], "generation": handle["generation"]}
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            health = session.result("runtime.health", ref)
            if health["state"] == "READY":
                break
            time.sleep(0.05)
        step("actual-http-echo-and-health", health["state"] == "READY" and echo(port, {"fixture": ["á", 42, None]}) == (200, {"echo": {"fixture": ["á", 42, None]}}), "POST /echo directo confirma el proceso real; health READY sólo después de responder")
        reused = session.call("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        wrong = session.call("runtime.health", ref | {"generation": ref["generation"] + 1})
        step("single-use-and-generation", reused.get("error", {}).get("data", {}).get("details", {}).get("reason") == "plan_already_used" and wrong.get("error", {}).get("data", {}).get("details", {}).get("reason") == "generation_mismatch", "plan de un uso e instancia/generación ligados")
        result = session.result("runtime.stop", ref)
        step("http-stop-closes-port", result == {"stopped": True} and not port_open(port), "parada HTTP y proceso terminado")
        plan = session.result("runtime.launch_plan", request)
        handle = session.result("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        owned = [child for child in children(session.process.pid) if child["Name"].lower() == "echo.exe"]
        assert len(owned) == 1
        subprocess.run(["taskkill", "/PID", str(owned[0]["ProcessId"]), "/F"], capture_output=True, check=True, timeout=10)
        ref = {"instance_id": handle["instance_id"], "generation": handle["generation"]}
        health = session.result("runtime.health", ref)
        step("actual-backend-crash-observed", health["state"] == "FAILED" and health["reason"] == "process_exited" and not port_open(port), "sólo se mata el hijo echo inventariado del worker propio; health observa la salida real")
        session.result("runtime.stop", ref)
        plan = session.result("runtime.launch_plan", request)
        session.result("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        deadline = time.monotonic() + 6
        while not port_open(port) and time.monotonic() < deadline:
            time.sleep(0.05)
        step("new-plan-recovers-after-backend-crash", echo(port, {"restarted": True}) == (200, {"echo": {"restarted": True}}), "plan nuevo arranca otro proceso real tras retirar la instancia fallida")
        session.close()
        session = None
        step("eof-stops-active-instance", not port_open(port), "cerrar stdin termina también la instancia activa; no quedan pipes retenidos")
        after = {p.relative_to(package).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest() for p in package.rglob("*") if p.is_file()}
        step("package-and-input-preserved", initial == after and factory.package_hash(package) == manifest["package_sha256"], "paquete, fixture y documento conservados; ningún estado externo modificado")
        flood_package, flood_manifest, _ = factory.create(REPO, run / "flood-package", version_flood=True)
        session = Session(executable(flood_package, flood_manifest), flood_manifest, cwd=flood_package)
        session.handshake()
        failed = session.call("runtime.list_versions", params)
        step("version-output-bounded", failed.get("error", {}).get("data", {}).get("details", {}).get("reason") == "version_probe_output_limit", "2 MiB reales de stdout se rechazan tras leer 1 MiB+1; proceso termina sin crecimiento ilimitado")
        session.close()
        session = None
        data = run / "data"
        principal = cli(data, "enroll-principal", "--name", principal_name, "--kind", "OPERATOR", "--scopes", "read,adapters:write")
        enrolled = True
        daemon, api = start(data)
        document = json.loads((package / "declarative.json").read_text(encoding="utf-8"))
        status, installed = http("POST", api + "/adapters/declarative", principal["token"], document, {"Idempotency-Key": "reference-runtime"})
        assert status == 201
        status, listing = http("GET", api + "/adapters?limit=200", principal["token"])
        step("core-registers-and-discovers-reference", status == 200 and installed in listing["items"] and installed["state"] == "INSTALLED" and installed["trust"] == "UNTRUSTED", "API existente registra y descubre la referencia sin promoción ni casos especiales en Core")
        installed_package = data / "adapters/reference.echo/1.0.0"
        installed_manifest = json.loads((installed_package / "manifest.json").read_text(encoding="utf-8"))
        session = Session(executable(installed_package, installed_manifest), installed_manifest, cwd=installed_package)
        refused = session.call("core.handshake", {"protocol_version":"1.0","adapter_id":installed_manifest["id"],"nonce":"uncontracted-import"})
        step("installed-uncontracted-capability-denied", refused.get("error",{}).get("data",{}).get("details",{}).get("reason")=="capability_descriptor_missing", "el importador materializa el launcher genérico: no hereda el contrato echo del wrapper de referencia")
        session.close()
        session = None
        launcher = copy.deepcopy(document)
        launcher["manifest"]["id"] = "reference.launcher"
        launcher["manifest"]["capabilities"] = launcher["capabilities"] = []
        status, installed_launcher = http("POST", api + "/adapters/declarative", principal["token"], launcher, {"Idempotency-Key":"reference-launcher"})
        assert status == 201 and installed_launcher["state"] == "INSTALLED" and installed_launcher["trust"] == "UNTRUSTED"
        installed_package = data / "adapters/reference.launcher/1.0.0"
        installed_manifest = json.loads((installed_package / "manifest.json").read_text(encoding="utf-8"))
        session = Session(executable(installed_package, installed_manifest), installed_manifest, cwd=installed_package)
        assert session.handshake()["capabilities"] == []
        plan = session.result("runtime.launch_plan", request)
        handle = session.result("runtime.start", {"plan_id": plan["plan_id"], "plan_hash": plan["plan_hash"], "approval_id": None})
        deadline = time.monotonic() + 6
        while not port_open(port) and time.monotonic() < deadline:
            time.sleep(0.05)
        step("installed-launcher-executes-actual-echo", echo(port, {"installed": True}) == (200, {"echo": {"installed": True}}) and factory.package_hash(installed_package) == installed_launcher["package_hash"], "launcher sin capacidades de inferencia ejecuta el echo real; hash contrastado, no cualificación P026")
        session.close()
        session = None
        step("installed-worker-eof-cleanup", not port_open(port), "el worker instalado también termina su instancia al cerrar sesión")
        success = True
        return 0
    finally:
        if session is not None:
            session.close()
        if daemon is not None and daemon.poll() is None:
            stop(daemon)
            daemon.wait(timeout=15)
        if enrolled:
            cli(run / "data", "revoke-principal", "--name", principal_name)
        record = {"result": "PASS" if success else "FAIL", "scope": "SDK Rust y worker genérico con proceso HTTP echo real; no inferencia ni cualificación P026", "steps": steps, "run": str(run.relative_to(REPO)), "package_hash": manifest["package_sha256"], "host": platform.platform(), "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "receipt.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")
        (REPO / "docs/reports/P025_runs/runtime-real.json").write_text(json.dumps(record, indent=2, ensure_ascii=False), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
