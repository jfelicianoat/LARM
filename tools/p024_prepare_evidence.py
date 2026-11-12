#!/usr/bin/env python3
"""Recibo auxiliar de P024. El runner canónico completo es p024_evidence.py."""
import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "docs/reports/P024_runs"


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    commands = [
        ("quality", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check.ps1", "-Prompt", "P024"]),
        ("packages", ["cargo", "test", "-p", "larm-adapter-host", "--test", "packages"]),
        ("catalog", ["cargo", "test", "-p", "larm-api", "-p", "larm-db", "--test", "adapters"]),
        ("selection", ["cargo", "test", "-p", "larm-db", "--test", "adapter_selection", "--test", "integration"]),
        ("update-dto", ["cargo", "test", "-p", "larm-domain", "adapter_updates"]),
        ("update-api", ["cargo", "test", "-p", "larm-api", "--test", "adapter_updates"]),
        ("update-journal", ["cargo", "test", "-p", "larm-db", "--test", "adapter_updates"]),
        ("update-coordinator", ["cargo", "test", "-p", "larm-daemon", "--test", "adapter_coordinator"]),
        ("package-space", ["cargo", "test", "-p", "larm-daemon", "--bin", "larm-daemon", "adapters::tests"]),
        ("daemon-build", ["cargo", "build", "-p", "larm-daemon"]),
        ("openapi", [str(REPO / "target/debug/larm-daemon.exe"), "openapi", "--out", "docs/api/openapi.json", "--check"]),
        ("real-preparation", [sys.executable, "tools/p024_real.py"]),
        ("real-selection", [sys.executable, "tools/p024_selection_real.py"]),
        ("real-update", [sys.executable, "tools/p024_update_real.py"]),
        ("real-api-update", [sys.executable, "tools/p024_api_real.py"]),
        ("real-migration-regression", [sys.executable, "tools/p004_real.py", "dbinfo"]),
        ("lock", [sys.executable, "tools/verify_p000.py", "lock-coherence"]),
        ("evidence-schema", [sys.executable, "tools/validate_evidence.py"]),
    ]
    checks = []
    for name, command in commands:
        print(f"==> P024 preparation {name}", flush=True)
        process = subprocess.run(command, capture_output=True, cwd=REPO, timeout=1800,
                                 env=os.environ | {"PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"})
        raw = process.stdout + b"\n" + process.stderr
        log = OUT / f"preparation-{name}.log"
        log.write_bytes(raw)
        checks.append({"id": name, "command": command, "result": "PASS" if process.returncode == 0 else "FAIL",
                       "exit_code": process.returncode, "log": str(log.relative_to(REPO)), "sha256": hashlib.sha256(raw).hexdigest()})
        print(f"    {checks[-1]['result']}", flush=True)
        if process.returncode:
            print(raw.decode("utf-8", errors="replace")[-1600:])
    result = {"phase": "P024", "phase_status": "AUXILIARY_CHECKS", "scope": "ZIP, registro local, selección, journal, CLI y API HTTP; el estado de fase lo calcula p024_evidence.py",
              "checks": checks, "implemented_checks_status": "PASS" if all(c["result"] == "PASS" for c in checks) else "FAIL",
              "pending": ["conformidad P026 previa a promoción de una candidata real",
                          "dependencias P020/P022 sin REAL_PASS; consultar PromptEvidence para el estado calculado"],
              "environment_lock_sha256": hashlib.sha256((REPO / "environment.lock.json").read_bytes()).hexdigest(),
              "source_sha256": {path: hashlib.sha256((REPO / path).read_bytes()).hexdigest() for path in [
                  "crates/larm-db/src/adapter_selection.rs", "crates/larm-db/tests/adapter_selection.rs",
                  "crates/larm-db/src/adapter_updates.rs", "crates/larm-db/tests/adapter_updates.rs",
                  "crates/larm-db/src/adapters.rs", "crates/larm-db/src/guard.rs", "crates/larm-db/src/reservations.rs",
                  "crates/larm-db/src/migrate.rs", "migrations/0002_adapter_selection.sql",
                  "crates/larm-domain/src/adapters.rs", "crates/larm-domain/src/ports.rs",
                  "crates/larm-domain/src/adapter_updates.rs",
                  "crates/larm-adapter-host/src/updates.rs", "crates/larm-adapter-host/src/package.rs",
                  "crates/larm-adapter-host/src/update_service.rs", "crates/larm-api/src/adapters.rs",
                  "crates/larm-api/src/openapi.rs", "crates/larm-api/tests/adapter_updates.rs",
                  "crates/larm-adapter-sdk/src/permissions.rs", "apps/daemon/tests/adapter_coordinator.rs", "apps/daemon/Cargo.toml", "Cargo.lock",
                  "apps/daemon/src/main.rs", "apps/daemon/src/adapters.rs", "apps/daemon/src/serve.rs",
                  "tools/p024_selection_real.py", "tools/p024_update_real.py", "tools/p024_api_real.py",
                  "tools/p024_prepare_evidence.py", "tools/p024_evidence.py"]},
              "generated_at": datetime.now(timezone.utc).isoformat()}
    (OUT / "preparation.checks.json").write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"P024 comprobaciones auxiliares: {result['implemented_checks_status']}; estado en PromptEvidence")
    return 0 if result["implemented_checks_status"] == "PASS" else 1


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
