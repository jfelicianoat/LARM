#!/usr/bin/env python3
"""Evidencia incremental del motor nativo; no cierra P026 ni cualifica plugins."""
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


def main():
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    baseline_path = OUT / "preparation.checks.json"
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    baseline_sha = sha(baseline_path)
    snapshot = OUT / f"native-engine-baseline-{baseline_sha[:12]}.checks.json"
    if snapshot.exists():
        assert sha(snapshot) == baseline_sha, "baseline snapshot changed"
    else:
        snapshot.write_bytes(baseline_path.read_bytes())
    sources = set(baseline["source_sha256"])
    sources.update((
        "tools/p026_native_engine_prepare.py", "tools/p026_prepare_evidence.py",
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
        "tools/p026_source_api_real.py", "tools/p043_sources_real.py", "tools/p026_evidence.py", "tools/evidence.py",
        "tools/p005_real.py", "tools/p023_real.py", "tools/p024_api_real.py", "tools/p024_update_real.py",

        "crates/larm-adapter-host/src/conformance.rs", "crates/larm-adapter-host/src/policy.rs",
        "crates/larm-adapter-host/src/conformance/raw.rs", "crates/larm-adapter-host/src/conformance/wire.rs",
        "crates/larm-adapter-host/src/conformance/runtime.rs",
        "crates/larm-adapter-host/src/conformance/source.rs",
        "crates/larm-adapter-host/src/conformance/protocol.rs",
        "crates/larm-adapter-host/src/conformance/transformation.rs",
        "crates/larm-adapter-host/src/conformance/benchmark.rs", "tools/p026_native_audit.py",
        "crates/larm-adapter-host/src/conformance/qualification.rs",
        "tests/fixtures/adapters/conformance_persistence.rs",
        "crates/larm-db/src/sources.rs", "crates/larm-db/src/adapter_selection.rs",
        "crates/larm-db/tests/sources.rs", "crates/larm-db/tests/adapter_selection.rs",
        "apps/daemon/src/sources.rs", "apps/daemon/src/adapters.rs",
        "apps/daemon/tests/adapter_coordinator.rs",
        "crates/larm-adapter-host/src/conformance/fixtures/reference-runtime.requests.json",
        "crates/larm-adapter-host/Cargo.toml", "adapters/runtime/reference/echo.rs",
        "crates/larm-adapter-host/tests/conformance.rs", "docs/adapters/host.md",
        "docs/adapters/conformance.md",
    ))
    before = {p: sha(REPO / p) for p in sorted(sources)}
    checks = []
    for name, command in (
        ("build", ["cargo", "build", "-p", "larm-daemon", "-p", "larm-adapter-sdk", "-p", "larm-adapter-host", "--bins", "--examples"]),
        ("fmt", ["cargo", "fmt", "--all", "--check"]),
        ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        ("workspace-tests", ["cargo", "test", "--workspace"]),
        ("probe-api", [sys.executable, "tools/p026_probe_api_real.py"]),
        ("profile-api", [sys.executable, "tools/p026_profile_api_real.py"]),
        ("source-api", [sys.executable, "tools/p026_source_api_real.py"]),
        ("openapi", [str(REPO / "target/debug/larm-daemon.exe"), "openapi", "--out", "docs/api/openapi.json", "--check"]),
        ("five-family-wire", [sys.executable, "tools/p026_wire_real.py"]),
        ("architecture", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check_architecture.ps1"]),
        ("package-copy", [sys.executable, "tools/package_copy.py", "check"]),
        ("lock", [sys.executable, "tools/verify_p000.py", "lock-coherence"]),
    ):
        result = subprocess.run(command, cwd=REPO, capture_output=True, timeout=600,
                                env=os.environ | {"PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"})
        log = OUT / f"native-engine-{name}.log"
        log.write_bytes(result.stdout + b"\n" + result.stderr)
        checks.append({"id": name, "command": command, "exit_code": result.returncode,
                       "result": "PASS" if result.returncode == 0 else "FAIL",
                       "log": log.relative_to(REPO).as_posix(), "sha256": sha(log)})
        print(f"P026-NATIVE-{name}: {checks[-1]['result']}", flush=True)
        if result.returncode:
            print(log.read_text(encoding="utf-8", errors="replace")[-4000:])
    after = {p: sha(REPO / p) for p in sorted(sources)}
    assert before == after, "sources changed during native-engine checks"
    changed = [p for p, digest in baseline["source_sha256"].items() if after[p] != digest]
    result = {
        "phase": "P026", "phase_status": "IN_PROGRESS",
        "implemented_checks_status": "PASS" if all(c["result"] == "PASS" for c in checks) else "FAIL",
        "scope": "NativeConformanceRunner/P022/binding/ownership including registered oracle IO cleanup; 80 original-package native wire cases and nine real common checks per family, explicitly rejected as incomplete qualification. Native reference runtime/echo 17 observations/15 signatures, source 21/9/private ledger HTTP transfer/recovery, protocol 17/4/actual Rust echo backend transformation 17/4/actual Base64 files/private attempts/recovery and benchmark 17/3/actual HTTP samples/statistics/private ledger/recovery, all through persistent compiled profiles/consumed approvals, authenticated HTTP router/real credential logic with ephemeral store, durable probe service, Core/SQLite and physical promotion gate. Real daemon/TCP additionally validates runtime from a local registered profile across restart and actual CLI promotion/enable/no-op/stale input/disable. Native qualification retains file handles/admission, rechecks package/suite/profile/inputs/effective context/P022 before selection commit; actual mutation/retry/enable/disable probes per reference. Runtime failed revalidation removes eligibility. Engine lifecycle fixtures still use synthetic CODE_PASS observations. Current Rust workspace, lock and independent five-family wire/host mutant regression; unchanged frontend and standalone functional SDK runners are historical baseline.",
        "checks": checks, "source_sha256": after,
        "previous_preparation": {"snapshot": snapshot.relative_to(REPO).as_posix(), "sha256": baseline_sha,
                                 "scope": "historical, before native engine; not current full preparation",
                                 "changed_source_paths": changed,
                                 "unchanged_source_count": len(baseline["source_sha256"]) - len(changed)},
        "environment_lock_sha256": sha(REPO / "environment.lock.json"),
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "host": platform.platform(), "python": platform.python_version(),
        "pending": ["persistent local profiles and approval consumption for five compiled reference recipes implemented; automatic owned oracle resources and general product profiles pending",
                    "real daemon/TCP runtime profile/restart and local CLI physical selection tested; source HTTP composition receives the same runner; actual enabled-source CRUD/input/CAS/outbox/restart scenario tested",
                    "direct execution consumers belong to following phases; formal closure requires current -Real/PromptEvidence and P025 REAL_PASS"],
    }
    (OUT / "native-engine.checks.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
    return 0 if result["implemented_checks_status"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
