#!/usr/bin/env python3
"""P021 - Adapter host, workers, timeout, crash y lifecycle: pruebas y evidencia."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from evidence import Evidence, cargo_ok  # noqa: E402

PY = sys.executable


def main() -> int:
    ev = Evidence("P021")
    ev.run("P021-REAL-build-fake", "REAL", ["cargo", "build", "-p", "larm-adapter-host", "--example", "fake_adapter"],
           "adaptador falso compilado")
    tests = [
        ("P021-REAL-handshake-ok", "handshake_correcto_y_llamadas", "READY, llamadas, método no anunciado sin enviar, parada STOPPED"),
        ("P021-REAL-handshake-fallido", "handshake_fallido", "handshake sin respuesta (TIMEOUT) y con otro nonce; ejecutable ausente; variable inyectable"),
        ("P021-REAL-timeout", "timeout_con_cancelacion_atendida_y_sin_atender", "timeout con cancel atendido (sigue READY) y desatendido (gracia y kill, FAILED)"),
        ("P021-REAL-cancelacion", "cancelacion_pedida_por_el_host", "cancel por id: request_cancelled y cancelled=false después"),
        ("P021-REAL-crash", "crash_del_proceso", "proceso que sale: ADAPTER_CRASHED y FAILED process_exited"),
        ("P021-REAL-stdout-corrupto", "stdout_corrupto", "tres líneas que no son frames: FAILED stdout_corrupt"),
        ("P021-REAL-stderr-enorme", "stderr_enorme_no_bloquea_y_se_limita", "12 MiB en stderr: no bloquea, se descarta lo que pasa de 10 MiB, secreto redactado"),
        ("P021-REAL-heartbeat", "heartbeat_ausente", "3 heartbeats sin respuesta: FAILED heartbeat_missed"),
        ("P021-REAL-entorno", "entorno_minimo_explicito", "entorno vaciado: ni PATH ni USERPROFILE ni APPDATA; sí lo del perfil y TEMP"),
        ("P021-REAL-cola", "cola_acotada", "cola llena reintentable; mutaciones del mismo recurso en serie"),
        ("P021-REAL-arbol", "matar_es_el_ultimo_recurso_y_mata_el_arbol", "kill como último recurso termina worker y nieto (tasklist)"),
        ("P021-REAL-job-object-kill-on-close", "job_object_al_cerrar_worker_termina_descendiente_aunque_muera_el_padre", "Job Object kill-on-close termina al nieto cuando solo muere el worker raíz"),
    ]
    for test_id, filt, expected in tests:
        ev.run(test_id, "REAL", ["cargo", "test", "-p", "larm-adapter-host", "--test", "worker", "--", filt, "--exact"],
               f"PC IA, procesos reales: {expected}", ok=cargo_ok)
    ev.run("P021-UNIT-cargo-fmt", "UNIT", ["cargo", "fmt", "--all", "--check"], "sin diferencias de formato")
    ev.run("P021-UNIT-cargo-clippy", "UNIT", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], "clippy sin avisos")
    ev.run("P021-UNIT-cargo-test", "UNIT", ["cargo", "test", "--workspace"], "pruebas del workspace en verde", ok=cargo_ok)
    ev.run("P021-REAL-architecture", "REAL",
           ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"], "grafo de crates intacto")
    ev.run("P021-REAL-regresion-p020", "REAL", [PY, "tools/p020_real.py", "stdio"], "protocolo v1 (P020) sigue en verde", timeout=600)
    changed = [
        "Cargo.toml", "Cargo.lock", "crates/larm-adapter-host/src/lib.rs", "crates/larm-adapter-host/src/worker.rs", "crates/larm-adapter-host/Cargo.toml",
        "crates/larm-adapter-host/examples/fake_adapter.rs", "crates/larm-adapter-host/tests/worker.rs",
        "tests/fixtures/fake_adapter/manifest.json", "tests/fixtures/fake_adapter/README.md", "docs/adapters/host.md",
        "docs/adapters/security.md", "docs/reports/P021_adapter-host-workers-timeout-crash-y-lifecycle.md",
        "docs/implementation/STATE.md", "tools/p021_evidence.py",
    ]
    ev.finish(changed, [])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
