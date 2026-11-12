#!/usr/bin/env python3
"""Cola SDK Rust por proceso Windows real; fixture de transporte, no benchmark."""
import hashlib
import json
import platform
import shutil
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "tools/adapter-conformance"))
from session import Session  # noqa: E402


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    run = REPO / "docs/reports/P026_runs/server-real" / uuid.uuid4().hex[:12]
    package = run / "package"
    (package / "bin").mkdir(parents=True)
    binary = package / "bin/demo_adapter.exe"
    shutil.copyfile(REPO / "target/debug/examples/demo_adapter.exe", binary)
    manifest = json.loads((REPO / "tests/fixtures/adapters/demo-benchmark.manifest.json").read_text(encoding="utf-8"))
    manifest["capabilities"] = []
    manifest["executable"] = {"relative_path": "bin/demo_adapter.exe", "args": ["--manifest", str(package / "manifest.json")]}
    manifest["package_sha256"] = hashlib.sha256(f"bin/demo_adapter.exe\0{sha(binary)}\n".encode()).hexdigest()
    (package / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    initial = {p.relative_to(package).as_posix(): sha(p) for p in package.rglob("*") if p.is_file()}
    session, steps, success = None, [], False

    def step(name, condition, detail):
        assert condition, name
        steps.append({"id": name, "result": "PASS", "detail": detail})
        print(f"PASS {name}", flush=True)

    identity = str(uuid.uuid7())
    params = {"variant_id":identity,"runtime_version_id":identity,"backend_id":identity,"launch_profile_id":identity,"suite_id":"fixture.transport:10000","fixture_hash":"a"*64,"settings_hash":"b"*64}
    try:
        session = Session([str(binary), *manifest["executable"]["args"]], manifest, cwd=package)
        session.handshake()
        step("actual-rust-worker", session.process.poll() is None, "ejecutable propio con SDK/validadores embebidos; manifiesto sin capacidades funcionales, sólo transporte")
        calls = [session.send("benchmark.run", params) for _ in range(68)]
        with session.condition:
            assert session.condition.wait_for(lambda: len(session.progress) >= 4 or session.failure or session.closed, timeout=10), "cuatro callbacks no emitieron progreso"
        heartbeat = session.result("core.heartbeat", {})
        with session.condition:
            started = set(session.progress)
        step("four-active-sixty-four-queued", len(started) == 4 and set(heartbeat["running_request_ids"]) == set(calls), "68 IDs admitidos y sólo cuatro emiten progreso real; heartbeat responde bajo saturación")
        rejected = session.call("benchmark.run", params)
        step("overflow-without-losing-accepted-calls", rejected["error"]["data"]["details"]["reason"] == "adapter_queue_full" and rejected["error"]["data"]["retryable"], "request 69 rechazada sin sustituir ni cancelar una de las 68 admitidas")
        queued = next(request_id for request_id in calls if request_id not in started)
        cancel = session.result("core.cancel", {"request_id": queued})
        cancelled = session.response(queued)
        with session.condition:
            started_after_cancel = set(session.progress)
        step("queued-cancel-without-callback", cancel["cancelled"] and cancelled["error"]["data"]["details"]["reason"] == "operation_cancelled" and started_after_cancel == started, "cancel responde y retira la llamada en espera mientras las cuatro activas continúan")
        replacement = session.send("benchmark.run", params)
        heartbeat = session.result("core.heartbeat", {})
        expected = (set(calls) - {queued}) | {replacement}
        step("capacity-recovered-after-reply", set(heartbeat["running_request_ids"]) == expected, "la respuesta cancelada escrita libera una plaza; la siguiente petición se admite con ID nuevo")
        process = session.process
        session.close()
        with session.condition:
            replies = dict(session.replies)
            progressed = set(session.progress)
        step("eof-finishes-all-accepted-calls", process.returncode == 0 and all(request_id in replies and "error" in replies[request_id] for request_id in set(calls) | {replacement}) and progressed == started, "EOF cancela 4 callbacks y 64 en espera, todas las llamadas tienen respuesta única y ninguna llamada de cola se ejecuta")
        session = None
        current = {p.relative_to(package).as_posix(): sha(p) for p in package.rglob("*") if p.is_file()}
        step("package-preserved-and-pipes-closed", current == initial, "SDK no modifica paquete ni manifiesto; cierre observado por el cliente sin pipes retenidos")
        success = True
        return 0
    finally:
        if session:
            session.close()
        record = {"result":"PASS" if success else "FAIL","scope":"SDK Rust bounded callbacks/queue over actual Windows process; not family functional qualification","steps":steps,"run":run.relative_to(REPO).as_posix(),"package_sha256":manifest["package_sha256"],"binary_sha256":sha(binary),"server_sha256":sha(REPO / "crates/larm-adapter-sdk/src/server.rs"),"script_sha256":sha(Path(__file__)),"host":platform.platform(),"generated_at":datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00","Z")}
        (run / "receipt.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
        (REPO / "docs/reports/P026_runs/server-real.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
