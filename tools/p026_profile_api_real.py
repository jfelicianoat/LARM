#!/usr/bin/env python3
"""Perfil persistente + aprobación/API/CLI/daemon/TCP/worker runtime propios.

La instalación inicial es una fila de fixture INSTALLED/UNTRUSTED explícita.
La conformidad sólo procede de la receta compilada y del worker fuera de proceso.
"""
import copy
import hashlib
import json
import os
import platform
import signal
import sqlite3
import subprocess
import sys
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path

import rfc8785
from p005_real import REPO, EXE, cli, clean_env, free_port, step
from p023_real import launch_request, start
from p024_api_real import fetch
from p025_runtime_real import load_factory


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    out = REPO / "docs/reports/P026_runs"
    run = out / "profile-api-real" / uuid.uuid4().hex[:12]
    run.mkdir(parents=True)
    data = run / "data"
    db = data / "state/larm.sqlite"
    names = {role: f"p026-profile-{run.name}-{role}" for role in ("writer", "other")}
    daemon, steps, tokens, principals, success = None, [], {}, {}, False
    digest = lambda value: hashlib.sha256(rfc8785.dumps(value)).hexdigest()

    def shutdown():
        nonlocal daemon
        if daemon is not None and daemon.poll() is None:
            os.kill(daemon.pid, signal.CTRL_BREAK_EVENT)
            daemon.wait(timeout=45)
            assert daemon.returncode == 0
        daemon = None

    def register(file, actor, approval, expected=None):
        command = [str(EXE), "--data-root", str(data), "register-adapter-probe-profile",
                   "--installation", installation, "--actor-id", actor, "--approval-id", approval, "--file", str(file)]
        if expected:
            command += ["--expected-profile", expected]
        result = subprocess.run(command, capture_output=True, env=clean_env(), timeout=45)
        value = json.loads(result.stdout if result.returncode == 0 else result.stderr)
        return result.returncode, value

    def stored():
        with sqlite3.connect(db) as conn:
            a = conn.execute("SELECT row_version,state,trust,metadata FROM adapter_installation WHERE id=?", (installation,)).fetchone()
            profiles = conn.execute("SELECT id,phase,plan_hash,payload FROM operation_journal WHERE kind='adapter.conformance.profile' ORDER BY id").fetchall()
            approvals = conn.execute("SELECT id,phase,payload FROM operation_journal WHERE kind='approval' ORDER BY id").fetchall()
            conformance = conn.execute("SELECT id,phase FROM operation_journal WHERE kind='adapter.conformance' ORDER BY id").fetchall()
            counts = conn.execute("SELECT (SELECT count(*) FROM event),(SELECT count(*) FROM event_outbox)").fetchone()
            selection = conn.execute("SELECT * FROM adapter_selection ORDER BY adapter_id").fetchall()
            return {"installation": a, "profiles": profiles, "approvals": approvals, "conformance": conformance, "counts": counts, "selection": selection}

    def selection(*args):
        result = subprocess.run([str(EXE), "--data-root", str(data), "adapter-selection", "--adapter-id", "reference.echo", "--actor-id", principals["writer"], *args], capture_output=True, env=clean_env(), timeout=45)
        return result.returncode, json.loads(result.stdout if result.returncode == 0 else result.stderr)

    def poll(base, receipt, role="writer"):
        url = base.removesuffix("/api/v1") + receipt["status_url"]
        deadline = time.monotonic() + 180
        while True:
            status, state = fetch("GET", url, tokens[role])
            assert status == 200
            if state["state"] in ("COMPLETED", "FAILED"):
                return state
            assert time.monotonic() < deadline, "el sondeo no terminó"
            time.sleep(0.05)

    try:
        for role, name in names.items():
            enrolled = cli(data, "enroll-principal", "--name", name, "--kind", "OPERATOR", "--scopes",
                           "admin" if role == "writer" else "read,adapters:write")
            tokens[role], principals[role] = enrolled["token"], enrolled["principal"]["id"]
        package, manifest, model = load_factory().create(REPO, data / "adapters/reference.echo/1.0.0", version_delay_ms=3000)
        installation = str(uuid.uuid7())
        at = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        with sqlite3.connect(db) as conn:
            conn.execute("INSERT INTO adapter_installation(id,created_at,updated_at,row_version,metadata,adapter_id,version,family,package_hash,publisher,trust,manifest,state) VALUES(?,?,?,1,?,?,?,?,?,?,'UNTRUSTED',?,'INSTALLED')",
                         (installation, at, at, json.dumps({"fixture": "controlled initial row; not imported qualification"}),
                          manifest["id"], manifest["version"], manifest["family"], manifest["package_sha256"], manifest["publisher"], json.dumps(manifest)))
        forbidden = run / "oracle/forbidden.bin"
        forbidden.parent.mkdir()
        forbidden.write_bytes(b"owned sentinel, outside worker grant")
        evidence = run / "native-reports"
        runtime_id = str(uuid.uuid7())
        request = launch_request(free_port(), str(model), {"allowlist": [], "variables": []})
        request["installation_id"] = request["variant_id"] = runtime_id
        config = {"schema_version": "1.0", "package_dir": str(package), "work_dir": str(run / "work"), "roots": [],
                  "custom": False, "capability_profile": "reference-capabilities.json", "immutable_inputs": [], "timeout_seconds": 120,
                  "suite": {"recipe": "reference.runtime.echo.v1", "parameters": {"runtime_id": runtime_id, "launch_request": request,
                            "expected_version": "reference-echo 1.0.0", "forbidden_model": str(forbidden), "evidence_root": str(evidence)}}}
        file = run / "profile.json"
        file.write_text(json.dumps(config, ensure_ascii=False, indent=2), encoding="utf-8")
        daemon, base = start(data)
        operation = {"operation": "adapter.execute", **{key: manifest[key] for key in ("version", "package_sha256", "publisher", "permissions")}, "adapter_id": manifest["id"]}
        approval_request = {"operation_digest": digest(operation), "resource_ids": [f"adapter:{manifest['id']}@{manifest['version']}"],
                            "code_hash": manifest["package_sha256"], "permissions": {"publisher": manifest["publisher"], "permissions": manifest["permissions"],
                            "expires_at": datetime.fromtimestamp(time.time()+3600, timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z"), "secret_refs": []}, "approved": True}
        status, approved = fetch("POST", base + "/approvals", tokens["writer"], approval_request, "approve-profile")
        assert status == 201
        approval = approved["resource_id"]
        before = stored()
        step(steps, "initial-unqualified", before["installation"][1:3] == ("INSTALLED", "UNTRUSTED") and not before["profiles"] and not before["conformance"], "fila inicial de fixture explícita, sin PASS")
        mutations = (
            ("unknown-recipe", lambda value: value["suite"].update(recipe="external.suite")),
            ("external-pass", lambda value: value.update(passed=True)),
            ("other-package", lambda value: value.update(package_dir=str(run))),
            ("missing-input", lambda value: value["suite"]["parameters"].update(forbidden_model=str(run / "missing.bin"))),
        )
        for name, mutate in mutations:
            value = copy.deepcopy(config)
            mutate(value)
            invalid_file = run / f"{name}.json"
            invalid_file.write_text(json.dumps(value), encoding="utf-8")
            code, _ = register(invalid_file, principals["writer"], approval)
            step(steps, name, code != 0 and stored() == before, "rechazo sin consumir aprobación ni guardar perfil")
        code, _ = register(file, principals["other"], approval)
        step(steps, "approval-actor-bound", code != 0 and stored() == before, "la aprobación no se transfiere a otro actor")
        code, registered = register(file, principals["writer"], approval)
        after = stored()
        step(steps, "profile-and-approval-atomic", code == 0 and len(after["profiles"]) == 1 and after["profiles"][0][1] == "READY" and after["approvals"][0][1] == "CONSUMED" and after["installation"] == before["installation"] and after["counts"][0] == after["counts"][1], "registro no ejecuta ni cambia INSTALLED/UNTRUSTED; concesión emitida una vez")
        profile = json.loads(after["profiles"][0][3])
        step(steps, "profile-binding-independent-jcs", digest(profile) == after["profiles"][0][2] and profile["configuration"] == config and profile["manifest_sha256"] == digest(manifest), "hash JCS independiente del registro/manifiesto completo")
        code, _ = register(file, principals["writer"], approval, registered["profile_id"])
        step(steps, "consumed-approval-not-reused", code != 0 and stored() == after, "otra emisión requiere una aprobación nueva")
        shutdown()
        daemon, base = start(data)
        step(steps, "profile-survives-daemon-restart", stored() == after, "nuevo proceso sin catálogo en memoria conserva el perfil y su consumo")
        body = {"resource_id": None, "profile_id": None}
        status, accepted = fetch("POST", base + f"/adapters/{installation}/probe", tokens["writer"], body, "persistent-profile-probe")
        step(steps, "probe-accepted", status == 202 and accepted["state"] == "QUEUED", "JobReceipt contractual tras reconstruir el perfil")
        state = poll(base, accepted)
        step(steps, "actual-native-runtime-conformance", state["state"] == "COMPLETED" and state["result"]["conformance"]["passed"] is True, "suite compilada: frames, quince firmas, versión, planes, procesos, HTTP, permisos, cancel/crash y cierre")
        receipt = state["result"]["conformance"]
        report_path = evidence / receipt["run_id"] / "runtime.json"
        report = json.loads(report_path.read_bytes())
        step(steps, "native-observations-retained", len(report["observations"]["Ok"]) == 17 and all(case["passed"] for case in report["cases"]) and report["run_sha256"] == receipt["run_sha256"], "17 observaciones reales y expected/observed durables")
        completed = stored()
        metadata = json.loads(completed["installation"][3])
        step(steps, "kernel-commit-and-package-unchanged", completed["installation"][1:3] == ("VALIDATED", "UNTRUSTED") and metadata["larm_conformance_v1"] == receipt and len(completed["conformance"]) == 1 and load_factory().package_hash(package) == manifest["package_sha256"] and completed["counts"][0] == completed["counts"][1], "Core/SQLite y paquete original; sin promoción ni confianza heredada")
        status, replay = fetch("POST", base + f"/adapters/{installation}/probe", tokens["writer"], body, "persistent-profile-probe")
        step(steps, "probe-replay-no-second-worker", status == 202 and replay == accepted and stored() == completed, "mismo recibo sin otra conformidad/eventos")
        code, promoted = selection("--promote", installation, "--expected-revision", "0")
        step(steps, "cli-promotion-physical-profile", code == 0 and promoted["selection"]["active_installation_id"] == installation and promoted["selection"]["enabled"] is False, "otra instancia del daemon reconstruye perfil/concesión y mide el contexto antes del COMMIT")
        code, enabled = selection("--enabled", "true", "--expected-revision", "1")
        step(steps, "cli-enable-physical-profile", code == 0 and enabled["selection"]["enabled"] is True and enabled["selection"]["row_version"] == 2, "activación mediante el gate físico compuesto, sin ejecutar otro worker")
        before_noop = stored()
        code, repeated = selection("--enabled", "true", "--expected-revision", "2")
        step(steps, "cli-enable-noop-rechecks", code == 0 and repeated == enabled and stored() == before_noop, "no-op conserva revisión y eventos, tras volver a comprobar elegibilidad")
        original = forbidden.read_bytes()
        forbidden.write_bytes(b"owned input changed after qualification")
        code, denied_selection = selection("--enabled", "true", "--expected-revision", "2")
        step(steps, "cli-changed-input-no-mutation", code != 0 and denied_selection["details"]["reason"] == "conformance_context_changed" and stored() == before_noop, "input modificado invalida incluso enable no-op, sin journal/eventos/selección nuevos")
        code, disabled = selection("--enabled", "false", "--expected-revision", "2")
        step(steps, "cli-disable-despite-stale-input", code == 0 and disabled["selection"]["enabled"] is False and disabled["selection"]["row_version"] == 3, "desactivar no necesita una concesión de ejecución vigente")
        forbidden.write_bytes(original)
        code, restored = selection("--enabled", "true", "--expected-revision", "3")
        step(steps, "cli-restored-input-valid-again", code == 0 and restored["selection"]["enabled"] is True and restored["selection"]["row_version"] == 4, "restaurar los bytes medidos permite repetir la operación sin importar un PASS")
        code, final_selection = selection("--enabled", "false", "--expected-revision", "4")
        step(steps, "cli-final-selection-disabled", code == 0 and final_selection["selection"]["enabled"] is False, "fixture termina deshabilitada y sin runtimes activos")
        status, denied = fetch("POST", base + f"/adapters/{installation}/probe", tokens["other"], body, "other-actor")
        assert status == 202
        failure = poll(base, denied, "other")
        current = stored()
        step(steps, "persisted-grant-not-transferable", failure["state"] == "FAILED" and failure["error"]["code"] == "FORBIDDEN" and failure["result"] is None and current["installation"] == completed["installation"] and current["conformance"] == completed["conformance"], "actor ajeno no ejecuta ni invalida el recibo histórico confirmado")
        shutdown()
        daemon, base = start(data)
        status, reopened = fetch("GET", base + f"/jobs/{accepted['job_id']}", tokens["writer"])
        step(steps, "completed-job-survives-second-restart", status == 200 and reopened == state and stored() == current, "recibo y perfil sobreviven sin reejecución ni mutación")
        shutdown()
        step(steps, "owned-processes-graceful-shutdown", daemon is None, "daemon y workers propios drenados; sin tocar procesos del operador")
        success = True
        return 0
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                shutdown()
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=15)
        for name in names.values():
            subprocess.run([str(EXE), "--data-root", str(data), "revoke-principal", "--name", name], capture_output=True, env=clean_env(), timeout=30)
        record = {"status": "PASS" if success and steps and all(v["result"] == "PASS" for v in steps) else "FAIL", "scope": "real daemon/TCP/Windows temporary credentials/SQLite/local compiled runtime profile, real worker and owned echo; fixture initial installation, no P028 import or global P026 closure", "steps": steps,
                  "run": run.relative_to(REPO).as_posix(), "daemon_sha256": sha(EXE), "source_sha256": sha(Path(__file__)), "host": platform.platform(), "python": platform.python_version(), "generated_at": datetime.now(timezone.utc).isoformat()}
        (run / "receipt.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        (out / "profile-api-real.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
