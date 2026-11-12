#!/usr/bin/env python3
"""Ejecuta las regresiones de la auditoría y conserva salidas, hashes y estado real."""
import hashlib
import json
import os
import platform
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
OUT = REPO / "docs/reports/AUDIT_FIXES_2026-10-16_runs"


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    commands = [
        ("quality-gates", ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "tools/check.ps1"]),
        ("runtime-hardware-regression", ["cargo", "test", "-p", "larm-runtime", "catalogo_y_lease_usan_hardware_actual"]),
        ("api-method-regression", ["cargo", "test", "-p", "larm-api", "--test", "app"]),
        ("storage-real", [sys.executable, "tools/p014_real.py", "serve"]),
        ("api-hardware-desktop-real", [sys.executable, "tools/audit_fixes_real.py", "--desktop-smoke"]),
        ("bundle-integrity-real", [sys.executable, "tools/ui_bundle_real.py", "--fault"]),
        ("desktop-offline-real", [sys.executable, "tools/p006_real.py", "offline"]),
        ("desktop-online-real", [sys.executable, "tools/p006_real.py", "online"]),
        ("credentials-real", [sys.executable, "tools/check_integration_credentials.py"]),
        ("secret-hygiene", [sys.executable, "tools/check_integration_credentials.py", "--scan-only"]),
        ("lock-coherence", [sys.executable, "tools/verify_p000.py", "lock-coherence"]),
        ("evidence-schema", [sys.executable, "tools/validate_evidence.py"]),
    ]
    checks = []
    for name, command in commands:
        print(f"==> {name}", flush=True)
        completed = subprocess.run(command, cwd=REPO, capture_output=True, timeout=1800,
                                   env=os.environ | {"PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"})
        output = completed.stdout + b"\n" + completed.stderr
        log = OUT / f"{name}.log"
        log.write_bytes(output)
        checks.append({"id": name, "command": command, "result": "PASS" if completed.returncode == 0 else "FAIL",
                       "exit_code": completed.returncode, "log": str(log.relative_to(REPO)), "sha256": hashlib.sha256(output).hexdigest()})
        print(f"    {checks[-1]['result']}", flush=True)
        if completed.returncode:
            print(output.decode("utf-8", errors="replace")[-1800:])
    result = {"status": "PASS" if all(c["result"] == "PASS" for c in checks) else "FAIL", "checks": checks,
              "host": platform.node(), "generated_at": datetime.now(timezone.utc).isoformat(),
              "scope": "Correcciones INC-01 a INC-07 sobre la implementación actual; no acredita fases futuras.",
              "binaries": {str(p.relative_to(REPO)): hashlib.sha256(p.read_bytes()).hexdigest() for p in
                           (REPO / "target/debug/larm-daemon.exe", REPO / "apps/desktop/src-tauri/target/debug/larm-desktop.exe")}}
    (REPO / "docs/reports/AUDIT_FIXES_2026-10-16.checks.json").write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"STATUS: {result['status']} ({len(checks)} grupos)")
    return 0 if result["status"] == "PASS" else 1


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    raise SystemExit(main())
