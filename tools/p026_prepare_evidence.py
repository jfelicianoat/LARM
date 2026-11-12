#!/usr/bin/env python3
"""Recibo de ejecución P026; el JSON no es autoridad de cualificación de un plugin."""
import hashlib
import json
import os
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "docs/reports/P026_runs"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def preparation_commands():
    return (
        ("build", ["cargo", "build", "-p", "larm-adapter-sdk", "-p", "larm-adapter-host", "-p", "larm-daemon", "--bins", "--examples"]),
        ("fmt", ["cargo", "fmt", "--all", "--check"]),
        ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        ("workspace-tests", ["cargo", "test", "--workspace"]),
        ("generated-schemas", [sys.executable, "tools/p026_schemas.py", "check"]),
        ("capability-schemas", [sys.executable, "tools/p026_capability_schemas.py", "check"]),
        ("capability-fixtures", [sys.executable, "tools/p026_capability_fixtures.py", "check"]),
        ("reference-capability-descriptors", [sys.executable, "tools/p026_reference_capabilities.py", "check"]),
        ("shared-corpus", [sys.executable, "tools/p026_schema_corpus.py", "check"]),
        ("differential", [sys.executable, "tools/test_p026_schemas.py"]),
        ("python-contracts", [sys.executable, "tools/test_python_contracts.py"]),
        ("python-capabilities", [sys.executable, "tools/test_capability_registry.py"]),
        ("python-negotiation", [sys.executable, "tools/test_capability_negotiation.py"]),
        ("rust-sdk", ["cargo", "test", "-p", "larm-adapter-sdk"]),
        ("domain-conformance", ["cargo", "test", "-p", "larm-domain"]),
        ("core-conformance", ["cargo", "test", "-p", "larm-core", "--test", "conformance"]),
        ("probe-api-contract", ["cargo", "test", "-p", "larm-api", "--test", "adapter_probes"]),
        ("profile-contract", ["cargo", "test", "-p", "larm-adapter-host", "--test", "conformance_profiles"]),
        ("profile-persistence", ["cargo", "test", "-p", "larm-db", "--test", "conformance_profiles"]),
        ("profile-api-real", [sys.executable, "tools/p026_profile_api_real.py"]),
        ("source-api-real", [sys.executable, "tools/p026_source_api_real.py"]),
        ("probe-persistence", ["cargo", "test", "-p", "larm-db", "--test", "adapter_probes"]),
        ("probe-service-lifecycle", ["cargo", "test", "-p", "larm-daemon", "--test", "adapter_probe_service"]),
        ("probe-api-real", [sys.executable, "tools/p026_probe_api_real.py"]),
        ("probe-openapi", [str(REPO / "target/debug/larm-daemon.exe"), "openapi", "--out", "docs/api/openapi.json", "--check"]),
        ("persistence-conformance", ["cargo", "test", "-p", "larm-db", "--test", "conformance"]),
        ("selection-conformance", ["cargo", "test", "-p", "larm-db", "--test", "adapter_selection"]),
        ("source-conformance", ["cargo", "test", "-p", "larm-db", "--test", "sources"]),
        ("update-coordinator-conformance", ["cargo", "test", "-p", "larm-daemon", "--test", "adapter_coordinator"]),
        ("python-sdk", [sys.executable, "tools/test_python_adapter_sdk.py"]),
        ("host-regressions", ["cargo", "test", "-p", "larm-adapter-host", "--lib", "--test", "declarative", "--test", "worker", "--test", "policy", "--test", "source"]),
        ("native-conformance-engine", ["cargo", "test", "-p", "larm-adapter-host", "--test", "conformance"]),
        ("protocol-process", [sys.executable, "tools/p020_real.py", "stdio"]),
        ("server-process-limits", [sys.executable, "tools/p026_server_real.py"]),
        ("protected-references", ["cargo", "test", "-p", "larm-daemon", "--test", "reference_adapters"]),
        ("five-family-negotiation", [sys.executable, "tools/p026_negotiation_real.py"]),
        ("five-family-wire", [sys.executable, "tools/p026_wire_real.py"]),
        ("declarative-regression", [sys.executable, "tools/p023_real.py", "serve"]),
        ("runtime-reference", [sys.executable, "tools/p025_runtime_real.py"]),
        ("source-reference", [sys.executable, "tools/p025_source_real.py"]),
        ("source-transfer", [sys.executable, "tools/test_reference_transfer.py"]),
        ("protocol-reference", [sys.executable, "tools/p026_protocol_real.py"]),
        ("transformation-files", [sys.executable, "tools/test_reference_transformation.py"]),
        ("transformation-reference", [sys.executable, "tools/p026_transformation_real.py"]),
        ("benchmark-statistics", [sys.executable, "tools/test_reference_benchmark.py"]),
        ("benchmark-reference", [sys.executable, "tools/p026_benchmark_real.py"]),
        ("package-copy", [sys.executable, "tools/package_copy.py", "check"]),
        ("architecture", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"]),
        ("lock", [sys.executable, "tools/verify_p000.py", "lock-coherence"]),
        ("secret-scan-contract", [sys.executable, "tools/test_secret_scan.py"]),
        ("secret-storage", [sys.executable, "tools/check_integration_credentials.py", "--scan-only"]),
    )


def preparation_sources():
    sources = [REPO / f"tools/{name}" for name in ("p026_schemas.py", "p026_oracle.py", "p026_schema_corpus.py", "test_p026_schemas.py", "test_python_contracts.py", "p026_prepare_evidence.py", "p020_real.py")]
    sources += [REPO / name for name in ("Cargo.toml", "Cargo.lock", "crates/larm-adapter-sdk/Cargo.toml", "crates/larm-adapter-sdk/src/lib.rs", "crates/larm-adapter-sdk/src/server.rs", "crates/larm-adapter-sdk/src/contracts/mod.rs", "crates/larm-adapter-sdk/src/contracts/generated.rs", "crates/larm-adapter-sdk/tests/contracts.rs", "crates/larm-adapter-sdk/tests/contracts_session.rs", "crates/larm-adapter-sdk/tests/session.rs", "crates/larm-adapter-sdk/examples/demo_adapter.rs", "crates/larm-adapter-host/examples/fake_adapter.rs", "crates/larm-adapter-host/tests/worker.rs", "crates/larm-adapter-host/tests/declarative.rs", "adapters/sdk/python/larm_adapter_sdk/contracts.py", "adapters/source/reference/package.py", "tests/fixtures/adapters/benchmark_support.rs", "tests/fixtures/adapters/method-schema-corpus.json")]
    sources += sorted((REPO / "docs/adapters/method-schemas").glob("*.json"))
    sources += [REPO / "tools/p026_server_real.py", REPO / "crates/larm-adapter-sdk/tests/server_limits.rs"]
    sources += [REPO / name for name in (
        "tools/p026_capability_schemas.py", "tools/p026_capability_fixtures.py",
        "tools/test_capability_registry.py", "tools/test_reference_transfer.py",
        "tools/p026_protocol_real.py", "tools/p025_host_fixture.py",
        "tools/requirements.txt", "tools/requirements-rfc8785.txt",
        "adapters/sdk/python/larm_adapter_sdk/__init__.py",
        "adapters/sdk/python/larm_adapter_sdk/access.py",
        "adapters/sdk/python/larm_adapter_sdk/capabilities.py",
        "adapters/source/reference/transfer.py",
        "adapters/protocol/reference/adapter.py", "adapters/protocol/reference/package.py",
        "adapters/protocol/reference/main.py",
        "apps/daemon/tests/reference_adapters.rs",
        "crates/larm-adapter-sdk/src/capabilities/mod.rs",
        "crates/larm-adapter-sdk/src/capabilities/builtin.rs",
        "crates/larm-adapter-sdk/tests/capabilities.rs",
        "tests/fixtures/adapters/capability-registry.json",
        "docs/contracts/capability-contracts.json",
        "apps/daemon/Cargo.toml",
        "tools/test_reference_transformation.py", "tools/p026_transformation_real.py",
        "adapters/transformation/reference/adapter.py", "adapters/transformation/reference/files.py",
        "adapters/transformation/reference/state.py", "adapters/transformation/reference/main.py",
        "adapters/transformation/reference/package.py",
        "tools/test_reference_benchmark.py", "tools/p026_benchmark_real.py",
        "adapters/benchmark/reference/adapter.py", "adapters/benchmark/reference/config.py",
        "adapters/benchmark/reference/ledger.py", "adapters/benchmark/reference/main.py",
        "adapters/benchmark/reference/package.py", "adapters/benchmark/reference/stats.py",
        "adapters/benchmark/reference/transport.py",
        "tools/p026_reference_capabilities.py", "tools/p026_negotiation_real.py",
        "tools/test_capability_negotiation.py", "tools/test_python_adapter_sdk.py",
        "tools/p023_real.py", "tools/p025_runtime_real.py", "tools/p025_source_real.py",
        "adapters/runtime/reference/main.rs", "adapters/runtime/reference/package.py",
        "adapters/runtime/reference/capabilities.json", "adapters/source/reference/capabilities.json",
        "adapters/source/reference/adapter.py", "adapters/source/reference/state.py", "adapters/sdk/python/larm_adapter_sdk/server.py",
        "crates/larm-adapter-sdk/src/generic.rs", "crates/larm-adapter-sdk/src/source/handler.rs",
        "crates/larm-adapter-sdk/tests/capability_session.rs",
        "crates/larm-adapter-host/examples/fake_source.rs",
        "tests/fixtures/adapters/python_sdk_worker.py",
        "adapters/sdk/python/README.md", "docs/adapters/authoring.md",
        "docs/adapters/capabilities.md", "docs/adapters/declarative.md",
        "docs/adapters/host.md",
        "tools/p026_wire_real.py", "tools/adapter-conformance/runner.py",
        "tools/adapter-conformance/session.py",
        "crates/larm-adapter-host/src/lib.rs", "crates/larm-adapter-host/src/framing.rs",
        "crates/larm-adapter-host/src/worker.rs",
        "crates/larm-adapter-host/src/policy.rs", "crates/larm-adapter-host/src/conformance.rs",
        "crates/larm-adapter-host/src/conformance/raw.rs", "crates/larm-adapter-host/src/conformance/wire.rs",
        "crates/larm-adapter-host/src/conformance/runtime.rs",
        "crates/larm-adapter-host/src/conformance/source.rs",
        "crates/larm-adapter-host/src/conformance/protocol.rs",
        "crates/larm-adapter-host/src/conformance/transformation.rs",
        "crates/larm-adapter-host/src/conformance/benchmark.rs", "tools/p026_native_audit.py",
        "crates/larm-domain/src/conformance_profiles.rs",
        "crates/larm-db/src/conformance_profiles.rs",
        "crates/larm-db/tests/conformance_profiles.rs",
        "crates/larm-adapter-host/src/conformance/profiles.rs",
        "crates/larm-adapter-host/tests/conformance_profiles.rs",
        "apps/daemon/src/probe_profiles.rs",
        "tools/p026_profile_api_real.py",
        "tools/p025_runtime_real.py",
        "adapters/runtime/reference/package.py",
        "crates/larm-domain/src/adapter_probes.rs",
        "crates/larm-domain/src/hardware.rs",
        "crates/larm-db/src/adapter_probes.rs",
        "crates/larm-db/tests/adapter_probes.rs",
        "crates/larm-adapter-host/src/conformance/catalog.rs",
        "crates/larm-adapter-host/src/conformance/service.rs",
        "crates/larm-api/src/adapters.rs",
        "crates/larm-api/src/openapi.rs",
        "crates/larm-api/tests/adapter_probes.rs",
        "crates/larm-core/src/conformance.rs",
        "crates/larm-core/tests/conformance.rs",
        "apps/daemon/src/probes.rs",
        "apps/daemon/src/main.rs",
        "apps/daemon/src/serve.rs",
        "apps/daemon/Cargo.toml",
        "apps/daemon/tests/adapter_probe_service.rs",
        "docs/api/openapi.json",
        "tools/p026_probe_api_real.py",
        "tools/p005_real.py", "tools/p023_real.py", "tools/p024_api_real.py", "tools/p024_update_real.py",

        "crates/larm-adapter-host/src/conformance/qualification.rs",
        "tests/fixtures/adapters/conformance_persistence.rs",
        "crates/larm-db/src/sources.rs", "crates/larm-db/src/adapter_selection.rs",
        "crates/larm-db/tests/sources.rs", "crates/larm-db/tests/adapter_selection.rs",
        "apps/daemon/src/sources.rs", "apps/daemon/src/adapters.rs",
        "apps/daemon/tests/adapter_coordinator.rs",
        "crates/larm-adapter-host/src/conformance/fixtures/reference-runtime.requests.json",
        "crates/larm-adapter-host/Cargo.toml", "adapters/runtime/reference/echo.rs",
        "crates/larm-adapter-host/tests/conformance.rs", "tools/p026_native_engine_prepare.py",
        "crates/larm-adapter-host/examples/conformance_host.rs",
        "crates/larm-adapter-host/examples/wire_proxy.rs",
        "crates/larm-domain/src/conformance.rs", "crates/larm-domain/src/lib.rs",
        "crates/larm-domain/src/ports.rs", "crates/larm-domain/tests/conformance.rs",
        "crates/larm-db/src/conformance.rs", "crates/larm-db/src/lib.rs",
        "crates/larm-db/tests/conformance.rs",
        "crates/larm-db/src/adapter_selection.rs", "crates/larm-db/tests/adapter_selection.rs",
        "crates/larm-db/src/sources.rs", "crates/larm-db/tests/sources.rs",
        "apps/daemon/tests/adapter_coordinator.rs", "tests/fixtures/adapters/conformance_persistence.rs",
        "crates/larm-core/src/conformance.rs", "crates/larm-core/src/lib.rs",
        "crates/larm-core/tests/conformance.rs", "docs/adapters/conformance.md",
    )]
    sources += sorted((REPO / "docs/adapters/capability-schemas").glob("*.json"))
    sources += [REPO / "docs/contracts/adapter-methods.json", *sorted((REPO / "docs/contracts/schemas").glob("*.schema.json"))]
    sources += [REPO / name for name in ("tools/p026_evidence.py", "tools/p026_source_api_real.py", "tools/p043_sources_real.py", "tools/evidence.py", "tools/check.ps1", "tools/check_architecture.ps1", "tools/package_copy.py", "tools/verify_p000.py", "tools/check_integration_credentials.py", "tools/test_secret_scan.py")]
    return sorted(set(sources))


def source_hashes():
    return {p.relative_to(REPO).as_posix(): sha(p) for p in preparation_sources()}


def write_receipt(checks, before, started):
    after = source_hashes()
    record = {
        "phase": "P026", "phase_status": "IN_PROGRESS",
        "implemented_checks_status": "PASS" if all(c["result"] == "PASS" for c in checks) and before == after else "FAIL",
        "scope": "40 firmas SDK Rust/Python, 8903 casos, registro obligatorio de capacidades; negociación y matriz wire/host por cinco familias; kernel/gate transaccional y motor nativo con binding físico/P022/ownership/cierre incluido IO del oráculo. Recetas nativas runtime/echo 17 observaciones, source 21/ledger HTTP privado, protocol 17/backend Rust real transformation 17/Base64/archivos/intentos privados y benchmark 17/muestras HTTP/estadisticas/ledger privado, todas Core/SQLite y gate de promoción; runtime revalidación fallida. Cinco referencias funcionales SDK con IO real. Perfiles locales persistentes para cinco recetas compiladas y gate compuesto en selección/fuentes; faltan recursos automáticos/perfiles generales e integración en consumidores de ejecución; sin acreditar inferencia ni cerrar dependencias",
        "checks": checks,
        "source_sha256": after, "source_before_sha256": before, "sources_unchanged": before == after, "started_at": started,
        "environment_lock_sha256": sha(REPO / "environment.lock.json"),
        "python": platform.python_version(), "host": platform.platform(),
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "pending": [
            "P025 debe alcanzar REAL_PASS para cerrar el consumidor P026; este recibo no sustituye PromptEvidence",
            "motor nativo y nueve checks wire originales implementados; runtime/echo 17 checks, source 21, protocol 17, transformation 17 y benchmark 17 con Core/SQLite; resolver persistente y composición daemon implementados; faltan recursos automáticos/perfiles generales; no cualificar workers mediante fixtures sintéticas o cobertura parcial",
            "gate transaccional y gate físico de promoción/habilitación/rollback/fuentes implementados e inyectados en SQLite; composición de selección/fuentes implementada, consumidores de ejecución pendientes; no importar veredictos externos",
            "consumidores de ejecución de fases posteriores y recursos/perfiles generales de producto siguen pendientes",
        ],
    }
    (OUT / "preparation.checks.json").write_text(json.dumps(record, ensure_ascii=False, indent=2), encoding="utf-8")
    return record


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    OUT.mkdir(parents=True, exist_ok=True)
    before = source_hashes()
    started = datetime.now(timezone.utc).isoformat()
    checks = []
    for name, command in preparation_commands():
        try:
            result = subprocess.run(command, cwd=REPO, capture_output=True, timeout=1800 if name == "secret-storage" else 600,
                                    env=os.environ | {"PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"})
            code, output = result.returncode, result.stdout + b"\n" + result.stderr
        except subprocess.TimeoutExpired as error:
            code, output = -1, b"TIMEOUT\n" + (error.stdout or b"") + (error.stderr or b"")
        log = OUT / f"preparation-{name}.log"
        log.write_bytes(output)
        checks.append({"id": name, "command": command, "result": "PASS" if code == 0 else "FAIL", "exit_code": code, "log": log.relative_to(REPO).as_posix(), "sha256": sha(log)})
        print(f"P026-PREP-{name}: {checks[-1]['result']}", flush=True)
        if code:
            print(output.decode("utf-8", errors="replace")[-3000:])
    record = write_receipt(checks, before, started)
    return 0 if record["implemented_checks_status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
