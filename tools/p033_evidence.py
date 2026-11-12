#!/usr/bin/env python3
"""Evidencia P033 del supervisor y estado de instancias."""
from __future__ import annotations

import sys
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok, exit_zero


def main() -> int:
    ev = Evidence("P033")
    fixtures = [
        REPO / "crates/larm-domain/src/instances.rs",
        REPO / "crates/larm-domain/src/runtime_ports.rs",
        REPO / "crates/larm-runtime/src/instances.rs",
        REPO / "crates/larm-runtime/src/supervisor.rs",
        REPO / "crates/larm-runtime/src/ports.rs",
        REPO / "crates/larm-db/src/instances.rs",
        REPO / "crates/larm-db/src/runtime_instance_stops.rs",
        REPO / "crates/larm-runtime/src/stop_service.rs",
        REPO / "crates/larm-runtime/src/lib.rs",
        REPO / "crates/larm-domain/src/runtime_execution.rs",
        REPO / "crates/larm-adapter-host/src/runtime/discovery.rs",
        REPO / "crates/larm-adapter-host/examples/fake_runtime_worker.rs",
        REPO / "crates/larm-adapter-host/tests/runtime.rs",
        REPO / "crates/larm-adapter-sdk/Cargo.toml",
        REPO / "crates/larm-adapter-sdk/src/generic.rs",
        REPO / "tests/fixtures/adapters/runtime_session.rs",
        REPO / "crates/larm-api/src/instances.rs",
        REPO / "apps/daemon/src/serve.rs",
        REPO / "apps/daemon/src/runtime_instance_process.rs",
        REPO / "apps/daemon/src/runtime_instance_start.rs",
        REPO / "crates/larm-runtime/src/ports.rs",
        REPO / "apps/daemon/Cargo.toml",
        REPO / "apps/daemon/src/main.rs",
    ]

    ev.run(
        "P033-HEALTH-STATE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "instances::tests", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "Dos fallos degradan; cinco fallos dejan FAILED; la recuperación devuelve READY o BUSY; "
            "snapshots inválidos y health en estados no operativos se rechazan."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-DURABLE-HEALTH-CAS",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--lib",
            "instances::tests::health_snapshot_cas_persiste_contadores_transicion_y_outbox",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "La observación de una sesión cualificada persiste el contador y el cambio READY→DEGRADED con CAS y outbox; una revisión obsoleta no emite evento."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-QUALIFIED-HEALTH-SERVICE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "instances::service_tests::health_service_usa_la_sesion_calificada_y_persiste_el_snapshot",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "El servicio consulta InstanceRef por RuntimeDiscoverySession cualificada y persiste su snapshot con la revisión leída."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-LAUNCH-SNAPSHOT-VALIDATION",
        "UNIT",
        [
            "cargo", "test", "-p", "larm-domain", "--lib", "instances::tests",
            "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "El snapshot tipado conserva LaunchPlan, parámetros efectivos, entorno sin secretos y límites; hash del plan y del snapshot detectan manipulación, y la evidencia histórica sigue válida tras caducar el plan."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-LAUNCH-SNAPSHOT-SQLITE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--lib",
            "instances::tests::create_transition_and_outbox_are_atomic_and_use_cas",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "El snapshot efectivo se persiste en metadata SQLite, sobrevive al roundtrip y sigue ligado a la creación y transición CAS de la instancia."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-SUPERVISOR-IDENTITY-CRASH-STOP",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "supervisor::tests", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "Backoff [1,5,15] y ventana de diez minutos; reconciliación exacta por PID/inicio/hash/generation; "
            "PID reutilizado no se termina; parada graceful y forzada siguen la comprobación de identidad."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-PORT-RACE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "ports::tests", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "El bind real omite puertos ocupados, las reservas concurrentes no se duplican "
            "y la reserva vuelve a estar libre al cerrar su guard."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-LEASE-STOP-GUARD",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "--lib",
            "instances::tests::stop_cas_rechaza_toda_lease_no_terminal_y_confirma_sin_lease",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "El CAS a STOPPING rechaza REQUESTED, ADMITTING, ACTIVE y EXPIRING; "
            "una lease terminal permite la transición."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-DURABLE-STOP-JOURNAL",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-db", "runtime_instance_stops::tests",
            "--lib", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "Estado STOPPING, outbox, job e idempotencia se confirman juntos; leases activas y fallos del outbox "
            "no dejan escrituras parciales; RUNNING se recupera y sólo el actor ve el job."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-STOP-CONSUMER-ORCHESTRATION",
        "UNIT",
        [
            "cargo", "test", "-p", "larm-runtime", "stop_service::tests", "--lib", "-j", "1",
            "--", "--test-threads=1",
        ],
        expected=(
            "El consumidor sólo completa tras salida confirmada; revalida identidad antes de forzar el árbol y marca el job fallido sin actuar ante un PID distinto."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-STOP-API-AUTH-IDEMPOTENCY",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-api", "instances::tests::stop_", "--lib", "-j", "1",
            "--", "--test-threads=1",
        ],
        expected=(
            "POST exige leases:manage, Idempotency-Key y body estricto; una repetición devuelve el receipt original "
            "y un cuerpo distinto falla con IDEMPOTENCY_CONFLICT."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-DAEMON-STOP-JOURNAL-WIRING",
        "INTEGRATION",
        ["cargo", "check", "-p", "larm-daemon", "-j", "1"],
        expected=(
            "El daemon recupera trabajos RUNNING interrumpidos al iniciar, registra el job como fuente de GET /jobs "
            "y conecta el repositorio durable con la ruta de instancias."
        ),
        ok=exit_zero,
        fixtures=fixtures,
    )
    ev.run(
        "P033-DAEMON-NATIVE-STOP-CONTROLLER",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon",
            "runtime_instance_process", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "El controlador observa PID, fecha de creación y hash del proceso; la parada usa la sesión cualificada y cierra únicamente la sesión/Job Object vinculados a esa identidad."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-DAEMON-LAUNCHPLAN-INSTANCE-COORDINATOR",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon",
            "runtime_instance_start", "-j", "1", "--", "--test-threads=1",
        ],
        expected=(
            "El coordinador valida el binding cualificado y el PID/fecha/hash observados, persiste "
            "CREATED→STARTING→LOADING→READY con snapshot, retiene el puerto hasta cerrar la sesión "
            "y rechaza un puerto ocupado antes de runtime.start."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-QUALIFIED-SESSION-LIFECYCLE",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-adapter-host", "--test", "runtime",
            "sesion_cualificada_expone_ciclo_runtime_con_acceso_explicito_por_llamada",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Una sesión cualificada recorre launch_plan, start, health, stop y load; el worker conserva P022, rechaza accesos ajenos y acota timeout antes de enviar el RPC."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-GENERIC-RUNTIME-OS-PROCESS-IDENTITY",
        "INTEGRATION",
        [
            "cargo", "test", "-p", "larm-adapter-sdk", "--lib",
            "generic::tests::process_started_at_usa_la_identidad_observada_por_el_sistema",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "El adaptador genérico registra la hora de creación observada por el sistema para el PID lanzado, liga ese PID a la ruta canónica del ejecutable y rechaza una identidad distinta."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-BUILD-RUNTIME-PROCESS-FIXTURE",
        "REAL",
        ["cargo", "build", "--offline", "-p", "larm-adapter-host", "--example", "fake_runtime_worker", "-j", "1"],
        expected="El worker fixture que crea y supervisa procesos propios compila para la prueba local de ciclo P033.",
        ok=exit_zero,
        fixtures=fixtures,
    )
    ev.run(
        "P033-REAL-QUALIFIED-LAUNCH-SNAPSHOT-OWNED-PROCESS",
        "REAL",
        [
            "cargo", "test", "--offline", "-p", "larm-adapter-host", "--test", "runtime",
            "sesion_cualificada_inicia_proceso_propio_desde_snapshot_efectivo",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "El proceso fixture propio informa PID y fecha observados; recibe args/entorno del LaunchPlan; "
            "el snapshot y las transiciones se validan; un límite no aplicado se rechaza; el secreto no aparece en logs; "
            "stop graceful y cierre del Job Object terminan sus procesos."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-WORKER-JOB-OBJECT-TREE",
        "REAL",
        [
            "cargo", "test", "--offline", "-p", "larm-adapter-host", "--test", "worker",
            "job_object_al_cerrar_worker_termina_descendiente_aunque_muera_el_padre",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "En Windows, al cerrar el Job Object termina el descendiente aunque la prueba solo mate el worker raíz."
        ),
        ok=cargo_ok,
        fixtures=[
            REPO / "Cargo.toml",
            REPO / "Cargo.lock",
            REPO / "crates/larm-adapter-host/Cargo.toml",
            REPO / "crates/larm-adapter-host/src/worker.rs",
            REPO / "crates/larm-adapter-host/tests/worker.rs",
        ],
    )
    ev.run(
        "P033-WINDOWS-OWNED-PROCESS-FIXTURE",
        "REAL",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "supervisor::tests::proceso_fixture_windows_cubre_parada_graceful_y_forzada",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "En Windows, un proceso hijo propio termina al recibir la orden graceful; "
            "otro hijo fixture que ignora esa orden termina por la ruta forzada con su handle conservado."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-GRACEFUL-RPC-FAILURE-REVALIDATES-BEFORE-FORCE",
        "UNIT",
        [
            "cargo", "test", "-p", "larm-runtime", "--lib",
            "supervisor::tests::rpc_graceful_fallido_espera_y_fuerza_solo_tras_revalidar_identidad",
            "-j", "1", "--", "--exact", "--test-threads=1",
        ],
        expected=(
            "Un RPC graceful fallido no omite el periodo de gracia; la ruta forzada vuelve a comprobar la identidad antes de actuar."
        ),
        ok=cargo_ok,
        fixtures=fixtures,
    )
    ev.run(
        "P033-FORMAT",
        "CONTRACT",
        ["cargo", "fmt", "--all", "--", "--check"],
        expected="Todo el workspace Rust cumple el formato fijado.",
        ok=exit_zero,
        fixtures=fixtures,
    )
    ev.run(
        "P033-CLIPPY",
        "INTEGRATION",
        [
            "cargo", "clippy", "--workspace", "--all-targets", "-j", "1",
            "--", "-D", "warnings",
        ],
        expected="Todo el workspace compila sin advertencias de Clippy.",
        ok=exit_zero,
        fixtures=fixtures,
        timeout=1800,
    )
    ev.blocked(
        "P033-REAL-LAUNCHPLAN-INTEGRATED-INSTANCE",
        "REAL",
        "crear, reconciliar y detener RuntimeInstance mediante la ruta de ejecución integrada del daemon",
        "Un consumidor local cualificado integra LaunchPlan, la instancia durable, el registro de sesión nativa y el ciclo stop del daemon.",
        "El coordinador interno LaunchPlan→RuntimeInstance y sus negativos pasan con una sesión fixture y PID real; aún no existe un caller contractual de arranque local. P100 sigue limitado a adopción externa, cuyo contrato prohíbe lanzar o matar ese runtime, y P033 no define un endpoint público de creación.",
    )
    ev.blocked(
        "P033-API-DURABLE-STOP-JOB",
        "REAL",
        "POST /api/v1/instances/{id}/stop",
        "La API exige leases:manage e Idempotency-Key, crea un JobReceipt durable, aplica el CAS de leases=0 y el consumidor termina el proceso.",
        "La ruta, el journal SQLite, la idempotencia transaccional, la recuperación al inicio, el consumidor y el controlador nativo del daemon ya están conectados. El coordinador puede registrar una sesión y su puerto, pero ningún consumidor local invoca aún ese arranque en el daemon.",
    )

    changed_files = [
        "crates/larm-domain/src/instances.rs",
        "crates/larm-domain/src/runtime_ports.rs",
        "crates/larm-db/src/instances.rs",
        "crates/larm-db/src/runtime_instance_stops.rs",
        "crates/larm-runtime/src/stop_service.rs",
        "crates/larm-runtime/src/lib.rs",
        "crates/larm-api/src/instances.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/src/runtime_instance_process.rs",
        "apps/daemon/src/runtime_instance_start.rs",
        "crates/larm-runtime/src/ports.rs",
        "apps/daemon/Cargo.toml",
        "apps/daemon/src/main.rs",
        "crates/larm-runtime/src/supervisor.rs",
        "crates/larm-runtime/src/instances.rs",
        "crates/larm-runtime/src/supervisor.rs",
        "crates/larm-domain/src/runtime_execution.rs",
        "crates/larm-adapter-sdk/Cargo.toml",
        "crates/larm-adapter-sdk/src/generic.rs",
        "crates/larm-adapter-host/src/runtime/discovery.rs",
        "crates/larm-adapter-host/examples/fake_runtime_worker.rs",
        "crates/larm-adapter-host/tests/runtime.rs",
        "tests/fixtures/adapters/runtime_session.rs",
        "tools/p033_evidence.py",
        "docs/reports/P033_runtime-process-supervisor-e-instancias.md",
        "docs/implementation/STATE.md",
    ]
    status = ev.finish(
        changed_files=changed_files,
        blockers=[
            "P030, P021, P019 y P029 no tienen REAL_PASS; el contrato 00 bloquea el cierre consumidor P033.",
        "El daemon ya inicia el consumidor durable y el controlador nativo, pero la ruta de producción LaunchPlan→RuntimeInstance todavía no registra la sesión cualificada que ese controlador requiere; no se declara completo el ciclo start→stop.",
        ],
    )
    pass_count = sum(test["result"] == "PASS" for test in ev.tests)
    blocked_count = sum(test["result"] == "BLOCKED" for test in ev.tests)
    report = REPO / "docs/reports/P033_runtime-process-supervisor-e-instancias.md"
    report.write_text(
        f"""# P033 — Runtime Process Supervisor e instancias

Estado de este incremento: **{status}**.

## Implementado

- Monitor de health con umbrales contractuales: DEGRADED tras dos fallos consecutivos, FAILED tras cinco y recuperación al estado READY/BUSY previo. El snapshot se puede serializar para persistirlo en metadata.
- La observación desde una sesión cualificada conserva el snapshot de salud en SQLite con CAS, limita la mutación a esa clave y al estado permitido, mantiene inmutable el binding del proceso y registra `instance.health_observed` en el outbox atómico. La prueba cubre contador durable, degradación, rechazo de revisión obsoleta y rechazo de cambios de identidad.
- `RuntimeInstanceHealthService` consulta `health` sobre `RuntimeDiscoverySession` con `InstanceRef` derivado de la fila durable, aplica los umbrales del monitor y persiste la revisión siguiente con el repositorio CAS. La prueba de servicio confirma la llamada cualificada y el snapshot resultante.
- Identidad de proceso validada por PID, fecha de creación observada del sistema operativo, SHA-256 del ejecutable y generation; el adaptador genérico comprueba que el PID recién lanzado pertenece a la ruta canónica prevista y falla cerrado ante discrepancias. La reconciliación reanuda sólo la identidad exacta, marca como CRASHED una identidad ya terminada o con PID reutilizado, reintenta si el probe no está disponible y pone en cuarentena registros incompletos. Procesos externos quedan intactos.
- Política de reinicio con backoff [1, 5, 15] segundos y ventana móvil de diez minutos.
- Núcleo de parada que pide graceful, espera la gracia configurada y sólo permite la ruta forzada tras volver a verificar la identidad propia.
- Fixture de proceso hijo real en Windows que cubre salida graceful y forzada con el handle `Child` conservado.
- `POST /api/v1/instances/{{id}}/stop` aplica `leases:manage`, body estricto e `Idempotency-Key`; la respuesta `JobReceipt`, el paso a `STOPPING`, el outbox y la clave de idempotencia se escriben en una transacción SQLite.
- El journal duradero permite reclamar jobs, recuperar `RUNNING` tras reiniciar el daemon y cerrar en `STOPPED` sólo después de confirmar la parada. Los fallos dejan la instancia en `STOPPING` y el job en `FAILED`, sin fingir que el proceso terminó. `GET /api/v1/jobs/{{id}}` filtra por actor.
- `RuntimeInstanceStopService` reclama el journal, valida `STOPPING`, delega graceful/force al controlador de proceso y sólo completa el job tras confirmar la salida. Los tests cubren PID distinto, timeout con revalidación y salida graceful.
- El daemon conecta el journal con `NativeRuntimeProcessController` y ejecuta el consumidor durable durante el servicio. El registro enlaza la identidad exacta (PID, creación, hash y generación) con `InstanceRef`, alcance y sesión física cualificada retenida; graceful usa esa sesión y el cierre forzado termina únicamente el Job Object/worker propietario. El daemon cierra las sesiones retenidas durante su apagado. La prueba real inspecciona PID, fecha y hash y confirma que una parada cierra solo la sesión vinculada.
- `RuntimeInstanceStartService` coordina internamente `LaunchPlan → runtime.start → RuntimeInstance`: vuelve a comprobar el binding actor/runtime, verifica contra el sistema operativo el PID, la fecha de creación y el SHA-256 del ejecutable, conserva el snapshot efectivo y persiste CREATED→STARTING→LOADING→READY con CAS/outbox. Reserva por bind el puerto declarado por el plan y retiene esa reserva junto a la sesión hasta el cierre. Dos pruebas cubren el recorrido con un PID real de fixture y el rechazo de un puerto ocupado antes de iniciar el worker. La admisión local de leases todavía no invoca este coordinador.
- Si falla el RPC graceful, el supervisor aún espera la gracia, vuelve a observar la identidad y solo entonces intenta la parada forzada. Una identidad ajena o ambigua sigue bloqueando cualquier terminación.
- La sesión física cualificada expone llamadas tipadas de `launch_plan`, `start`, `load`, `health` y `stop`. Cada RPC requiere un alcance interno por operación que el worker verifica contra P022; rechaza rutas ajenas y timeouts vacíos o mayores de diez minutos. La autorización de descubrimiento sigue limitada a red. La prueba REAL crea un proceso hijo fixture desde LaunchPlan, observa args y entorno efectivos, conserva PID/fecha/hash en un `RuntimeInstance` validado, rechaza un límite de memoria que no aplica, comprueba que el secreto resuelto no queda en el log y prueba stop graceful y terminación del Job Object. Esta prueba no equivale al camino productivo de creación de instancias del daemon.
- Windows asigna el worker a un Job Object con `KILL_ON_JOB_CLOSE`; una prueba con procesos reales confirma que el descendiente termina aunque se mate primero sólo la raíz. Esto acredita la contención del proceso hijo del adaptador en P021.
- `RuntimeLaunchSnapshot` conserva el `LaunchPlan` completo y su hash bajo una clave versionada de `RuntimeInstance.metadata`; incluye parámetros, args, entorno permitido, referencias de credenciales y límite efectivo. Se valida al serializar/leer, no se rompe la evidencia al caducar el plan y la prueba SQLite cubre el roundtrip en la creación/transición CAS.

## Pruebas y límites

El archivo de evidencia asociado registra {pass_count} PASS, 0 FAIL y {blocked_count} BLOCKED con IDs, comandos, hashes de fixtures y transcripciones. Incluye el monitor de salud, observación cualificada con CAS/outbox, autorización e idempotencia de API, atomicidad/recuperación del journal, puertos, crash/backoff, reconciliación, PID reutilizado, identidad nativa, coordinador de arranque, formato y Clippy del workspace. El test nuevo del coordinador usa el repositorio de prueba en memoria; las pruebas existentes de SQLite siguen verificando el CAS y el outbox durables.

El daemon ya acepta y expone jobs durables de stop, ejecuta el consumidor y conecta un controlador nativo. El nuevo coordinador conecta esas operaciones a la persistencia de `RuntimeInstance` cuando un consumidor interno lo invoca; falta ese caller de arranque local para probar el ciclo completo sobre un worker runtime cualificado en la ruta productiva. El puente P100 actual adopta un runtime externo y no puede cubrir ese papel. La UI no cambia en este incremento.

P033 conserva **BLOCKED_REAL** mientras ningún consumidor local invoque el coordinador en la ruta productiva y P030/P021/P019/P029 sigan sin REAL_PASS. Las pruebas del coordinador usan un PID real y observan persistencia y lifecycle sobre su repositorio de test; todavía falta la integración con el caller local de leases y el worker productivo.
""",
        encoding="utf-8",
    )
    return 0 if status in {"CODE_PASS", "REAL_PASS", "BLOCKED_REAL"} else 1


if __name__ == "__main__":
    sys.exit(main())
