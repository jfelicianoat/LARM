#!/usr/bin/env python3
"""P099: locks inmutables, adquisición determinista y límites reales pendientes."""
import sys
from datetime import datetime, timezone
from pathlib import Path

from evidence import Evidence, REPO, cargo_ok, exit_zero

TOOL_PYTHON = REPO / ".venv-tools" / "Scripts" / "python.exe"
PYTHON = str(TOOL_PYTHON) if TOOL_PYTHON.is_file() else sys.executable


def main() -> int:
    ev = Evidence("P099")
    ui_temp = REPO / "target" / "tmp"
    ui_temp.mkdir(parents=True, exist_ok=True)
    ui_env = {"TEMP": str(ui_temp), "TMP": str(ui_temp)}
    ev.run(
        "P099-CONTRACT-profile-lock-api",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-api", "--lib"],
        "PUT autenticado e idempotente y GET por hash; cuerpos, scope y promoción inválidos se rechazan",
        ok=cargo_ok,
    )
    ev.run(
        "P099-CONTRACT-profile-lock-schema-regenerate",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/p026_schemas.py", "write"],
        "índice de schemas de adaptadores regenerado para el hash actualizado de larm.schema.json",
    )
    ev.run(
        "P099-CONTRACT-profile-lock-schema",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p026_schemas.py"],
        "schemas JSON mantienen su validez y compatibilidad con los contratos existentes",
    )
    ev.run(
        "P099-CONTRACT-capability-schema-index",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/p026_capability_schemas.py", "check"],
        "el índice generado de capacidades conserva los hashes de sus fuentes, incluido larm.schema.json",
    )
    ev.run(
        "P099-CONTRACT-locks-sqlite",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-db", "--test", "runtime_profile_locks"],
        "nueve casos SQLite: adquisición y cualificación separadas, replay, binding de fixtures/estrategias, inmutabilidad e rollback del outbox",
        ok=cargo_ok,
    )
    ev.run(
        "P099-CONTRACT-install-binding",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-db", "--test", "runtime_installation"],
        "admisión P031 valida perfil persistido y rechaza ausencia o binding distinto sin escrituras",
        ok=cargo_ok,
    )
    ev.run(
        "P099-CONTRACT-acquisition-tool",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_runtime_profile_locks.py"],
        "ocho casos de catálogo, hash, captura de fuente, invariancia, estrategia y CLI",
        fixtures=[REPO / "tools/runtime_profile_locks.py"],
    )
    ev.run(
        "P099-CONTRACT-local-observation-capture",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p099_capture_ollama.py"],
        "seis casos verifican el comando allowlisted, hash de ejecutable/fuente, entorno sin credenciales, replay inmutable y estado siempre UNQUALIFIED",
        fixtures=[
            REPO / "tools/p099_capture_ollama.py",
            REPO / "docs/contracts/runtime-profiles.json",
            REPO / "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md",
        ],
    )
    ev.run(
        "P099-CONTRACT-local-api-observation-capture",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p099_capture_ollama_api.py"],
        "cinco casos verifican endpoint loopback exacto, hash de respuesta, entorno sin credenciales, propietario allowlisted y rechazo si cambia durante la consulta; ninguna observación cualifica el perfil",
        fixtures=[
            REPO / "tools/p099_capture_ollama_api.py",
            REPO / "tools/p099_ollama_listener.py",
            REPO / "docs/contracts/runtime-profiles.json",
            REPO / "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md",
        ],
    )
    ev.run(
        "P099-CONTRACT-listener-owner",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p099_ollama_listener.py"],
        "cuatro casos identifican un único dueño de puerto, resuelven ruta y hash del ejecutable, y fallan cerrados ante propietarios ambiguos o inaccesibles",
        fixtures=[REPO / "tools/p099_ollama_listener.py"],
    )
    ev.run(
        "P099-CONTRACT-model-smoke-capture",
        "CONTRACT",
        [PYTHON, "-X", "utf8", "tools/test_p099_smoke_ollama_model.py"],
        "seis casos verifican versión, catálogo y generación fija, hash de captura, listener estable, entorno sin credenciales y estado siempre UNQUALIFIED",
        fixtures=[
            REPO / "tools/p099_smoke_ollama_model.py",
            REPO / "tools/p099_capture_ollama_api.py",
            REPO / "tools/p099_ollama_listener.py",
        ],
    )
    capture_run_id = datetime.now(timezone.utc).strftime("ollama-%Y%m%dT%H%M%S%fZ")
    capture_dir = REPO / "docs" / "reports" / "P099_runs" / "observations" / capture_run_id
    ev.run(
        "P099-REAL-ollama-version-observation",
        "REAL",
        [
            PYTHON,
            "-X",
            "utf8",
            "tools/p099_capture_ollama.py",
            "--run-id",
            capture_run_id,
        ],
        "la sonda local allowlisted registra Ollama 0.40.6, el hash del ejecutable y la transcripción cruda sin crear lock ni cualificación",
        ok=lambda code, out: code == 0 and '"status": "PASS"' in out and '"upstream_version": "0.40.6"' in out,
        timeout=60,
        fixtures=[
            REPO / "tools/p099_capture_ollama.py",
            REPO / "docs/contracts/runtime-profiles.json",
            REPO / "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md",
            capture_dir / "capture.json",
            capture_dir / "probe.stdout.bin",
            capture_dir / "probe.stderr.bin",
        ],
    )
    api_capture_run_id = datetime.now(timezone.utc).strftime("ollama-api-%Y%m%dT%H%M%S%fZ")
    api_capture_dir = REPO / "docs" / "reports" / "P099_runs" / "api_observations" / api_capture_run_id
    ev.run(
        "P099-REAL-ollama-api-version-observation",
        "REAL",
        [
            PYTHON,
            "-X",
            "utf8",
            "tools/p099_capture_ollama_api.py",
            "--run-id",
            api_capture_run_id,
        ],
        "el capturador verifica PID y SHA-256 del Ollama preexistente, consulta sólo GET 127.0.0.1:11434/api/version y registra 0.40.6 sin iniciar ni detener el servicio, cargar modelos o crear lock",
        ok=lambda code, out: code == 0 and '"status": "PASS"' in out and '"upstream_version": "0.40.6"' in out,
        timeout=60,
        fixtures=[
            REPO / "tools/p099_capture_ollama_api.py",
            REPO / "tools/p099_capture_ollama.py",
            REPO / "tools/p099_ollama_listener.py",
            REPO / "docs/contracts/runtime-profiles.json",
            REPO / "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md",
            api_capture_dir / "capture.json",
            api_capture_dir / "api.response.bin",
        ],
    )
    model_smoke_run_id = datetime.now(timezone.utc).strftime(
        "ollama-gemma4-12b-smoke-%Y%m%dT%H%M%S%fZ"
    )
    model_smoke_dir = REPO / "docs" / "reports" / "P099_runs" / "model_smokes" / model_smoke_run_id
    ev.run(
        "P099-REAL-ollama-gemma4-model-smoke",
        "REAL",
        [
            PYTHON,
            "-X",
            "utf8",
            "tools/p099_smoke_ollama_model.py",
            "--run-id",
            model_smoke_run_id,
        ],
        "verifica el propietario allowlisted antes y después de consultar versión, listar modelos y generar LARM_OK con gemma4:12b; el servicio preexistente no se inicia ni detiene y el perfil queda UNQUALIFIED",
        ok=lambda code, out: code == 0 and '"status": "PASS"' in out and '"response_contains_expected_marker": true' in out,
        timeout=240,
        fixtures=[
            REPO / "tools/p099_smoke_ollama_model.py",
            REPO / "tools/p099_capture_ollama_api.py",
            REPO / "tools/p099_ollama_listener.py",
            REPO / "docs/contracts/runtime-profiles.json",
            REPO / "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md",
            model_smoke_dir / "capture.json",
            model_smoke_dir / "api.version.response.json",
            model_smoke_dir / "model.response.json",
        ],
    )
    ev.run(
        "P099-CONTRACT-format",
        "CONTRACT",
        ["cargo", "fmt", "--all", "--", "--check"],
        "formato Rust del workspace correcto",
    )
    ev.run(
        "P099-CONTRACT-clippy",
        "CONTRACT",
        ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
        "Clippy del workspace y todos sus targets sin warnings de Rust",
    )
    ev.run(
        "P099-CONTRACT-daemon-wiring",
        "CONTRACT",
        ["cargo", "test", "-p", "larm-daemon", "--no-run"],
        "el daemon de producción compila con las rutas P099 y el repositorio SQLite compartido",
        ok=exit_zero,
        env={"CARGO_TARGET_DIR": str(REPO / "target" / "verification-final")},
    )
    ev.run(
        "P099-CONTRACT-desktop-put-transport",
        "CONTRACT",
        ["cargo", "test", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml", "api::tests"],
        "el transporte nativo sólo admite PUT dentro de la API loopback y sigue protegiendo el token",
        ok=cargo_ok,
    )
    ev.run(
        "P099-CONTRACT-desktop-format",
        "CONTRACT",
        ["cargo", "fmt", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml", "--", "--check"],
        "formato del backend nativo Tauri correcto",
    )
    ev.run(
        "P099-CONTRACT-desktop-clippy",
        "CONTRACT",
        ["cargo", "clippy", "--manifest-path", "apps/desktop/src-tauri/Cargo.toml", "--all-targets", "--", "-D", "warnings"],
        "Clippy del backend nativo Tauri sin warnings",
    )
    ev.run(
        "P099-CONTRACT-profile-lock-ui",
        "CONTRACT",
        ["npm.cmd", "--prefix", "apps/desktop", "test"],
        "suite de interfaz valida adquisición, consulta por hash y bloqueo local de promoción",
        env=ui_env,
    )
    ev.run(
        "P099-CONTRACT-profile-lock-ui-build",
        "CONTRACT",
        ["npm.cmd", "--prefix", "apps/desktop", "run", "build"],
        "TypeScript y build de producción del escritorio pasan",
        env=ui_env,
    )
    ev.run(
        "P099-CONTRACT-profile-lock-ui-lint",
        "CONTRACT",
        ["npm.cmd", "--prefix", "apps/desktop", "run", "lint"],
        "ESLint del escritorio pasa",
        env=ui_env,
    )

    ev.blocked(
        "P099-REAL-profile-capture",
        "REAL",
        "captura de perfiles de los runtimes instalados en el PC IA",
        "captura completa por runtime con versión, documentación versionada, probes y fixtures de API/modelo/backend con hashes",
        "Ollama 0.40.6 ya tiene observaciones locales verificadas de versión, catálogo y generación con gemma4:12b; aún faltan fixtures y probes para completar su perfil, el resto del software instalado y la evidencia REAL_PASS de P000/P020/P043/P036",
    )
    ev.blocked(
        "P099-REAL-adapter-qualification",
        "REAL",
        "cualificación por adaptador y versión upstream",
        "fixtures positivos/negativos y pruebas de adquisición observadas por cada adaptador disponible",
        "requiere el inventario de P000 y las dependencias reales P020/P043/P036; ningún lock UNQUALIFIED habilita soporte",
    )

    changed = [
        "crates/larm-adapter-host/src/conformance.rs",
        "crates/larm-adapter-host/src/conformance/raw.rs",
        "crates/larm-adapter-host/src/policy.rs",
        "crates/larm-adapter-host/src/worker.rs",
        "crates/larm-db/src/migrate.rs",
        "crates/larm-db/src/runtime_profiles.rs",
        "crates/larm-db/tests/adapter_selection.rs",
        "crates/larm-db/tests/integration.rs",
        "crates/larm-db/tests/runtime_profile_locks.rs",
        "crates/larm-domain/src/runtime_profiles.rs",
        "crates/larm-domain/src/auth.rs",
        "crates/larm-runtime/src/install_service.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/src/runtime_install.rs",
        "crates/larm-db/src/runtimes/install.rs",
        "crates/larm-db/tests/runtime_installation.rs",
        "crates/larm-api/src/lib.rs",
        "crates/larm-api/src/openapi.rs",
        "crates/larm-api/src/runtime_profiles.rs",
        "apps/desktop/src/api/client.ts",
        "apps/desktop/src/api/runtimes.ts",
        "apps/desktop/src/runtimes.tsx",
        "apps/desktop/src/runtimes.test.tsx",
        "apps/desktop/src/styles.css",
        "apps/desktop/src-tauri/src/api.rs",
        "migrations/0004_runtime_profile_locks.sql",
        "migrations/0005_runtime_profile_qualifications.sql",
        "docs/contracts/api-operations.json",
        "docs/contracts/schemas/larm.schema.json",
        "docs/adapters/method-schemas/index.json",
        "docs/adapters/capability-schemas/index.json",
        "docs/operations/runtime-profile-locks.md",
        "docs/reports/P031_descubrimiento-e-instalacion-de-versiones-de-runtime.md",
        "docs/implementation/STATE.md",
        "docs/implementation/STATE.md",
        "tools/p099_evidence.py",
        "tools/runtime_profile_locks.py",
        "tools/test_runtime_profile_locks.py",
        "tools/p099_capture_ollama.py",
        "tools/p099_capture_ollama_api.py",
        "tools/p099_ollama_listener.py",
        "tools/p099_smoke_ollama_model.py",
        "tools/p099_evidence.py",
        "tools/test_p099_capture_ollama.py",
        "tools/test_p099_capture_ollama_api.py",
        "tools/test_p099_ollama_listener.py",
        "tools/test_p099_smoke_ollama_model.py",
        "docs/reports/P099_runs/sources/ollama/2476d5e/docs/api.md",
        "docs/reports/P099_perfiles-externos-por-version-y-fixtures-de-apis-reales.md",
    ]
    for filename in ("capture.json", "probe.stdout.bin", "probe.stderr.bin"):
        changed.append(
            (Path("docs/reports/P099_runs/observations") / capture_run_id / filename).as_posix()
        )
    for filename in ("capture.json", "api.response.bin"):
        changed.append(
            (Path("docs/reports/P099_runs/api_observations") / api_capture_run_id / filename).as_posix()
        )
    for filename in ("capture.json", "api.version.response.json", "model.response.json"):
        changed.append(
            (Path("docs/reports/P099_runs/model_smokes") / model_smoke_run_id / filename).as_posix()
        )
    blockers = [
        "P099 permanece BLOCKED_REAL: faltan fichas verificadas para las 20 entradas y capturas completas para los runtimes disponibles; Ollama ya tiene observaciones de versión, catálogo y generación de gemma4:12b con propietario de proceso verificado.",
        "P099 aún necesita pruebas de adquisición por cada adaptador disponible y la cualificación real de sus perfiles; el smoke Ollama observado no crea una cualificación.",
        "No se permite cualificar ni habilitar un lock sin los hechos y fixtures reales requeridos.",
    ]
    return int(ev.finish(sorted(set(changed)), blockers) == "BLOCKED")


if __name__ == "__main__":
    raise SystemExit(main())
