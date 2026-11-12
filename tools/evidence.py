#!/usr/bin/env python3
"""Evidencia por prompt conforme a larm.schema.json#/$defs/PromptEvidence.

Uso desde un script de prompt (tools/pNNN_evidence.py):

    ev = Evidence("P004")
    ev.run("P004-UNIT-db", "UNIT", ["cargo", "test", "-p", "larm-db"],
           expected="cargo test -p larm-db sin fallos", ok=cargo_ok)
    ev.blocked("P004-REAL-x", "procedimiento", expected="...", reason="...")
    ev.finish(changed_files=[...], blockers=[...])

Reglas que aplica (contrato 00):
- cada prueba se ejecuta aquí; ``observed`` es la salida real resumida y la
  transcripción completa queda en docs/reports/<PNNN>_runs/<id>.log con su sha256;
- un FAIL deja el prompt en BLOCKED (implementación); sin FAIL, cualquier prueba
  BLOCKED, bloqueo declarado o dependencia sin REAL_PASS deja BLOCKED_REAL;
  REAL_PASS exige al menos una prueba REAL y todas en PASS/INAPPLICABLE;
- ``inputs_hash`` se calcula sobre las entradas obligatorias del paquete v3, cuyos
  hashes se comprueban antes contra manifest.json.
"""
from __future__ import annotations

import hashlib
import json
import os
import platform
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable

REPO = Path(__file__).resolve().parent.parent
PACKAGE = REPO.parent / "LARM_prompts_v3"
REPORTS = REPO / "docs/reports"
COMMON_INPUTS = [
    "LARM_SPEC_V1.md", "SHARED_CONTRACT.md", "manifest.json",
    "contracts/defaults.json", "contracts/persistence.sql", "contracts/api-operations.json",
    "contracts/adapter-methods.json", "contracts/capability-contracts.json",
    "schemas/domain.schema.json", "schemas/larm.schema.json",
    "fixtures/benchmark-suites.json", "contracts/acceptance.json",
]


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def now_utc() -> str:
    m = datetime.now(timezone.utc)
    return m.strftime("%Y-%m-%dT%H:%M:%S.") + f"{m.microsecond // 1000:03d}Z"


def manifest() -> dict:
    return json.loads((PACKAGE / "manifest.json").read_text(encoding="utf-8"))


def prompt_entry(code: str) -> dict:
    for p in manifest()["prompts"]:
        if p["code"] == code:
            return p
    raise SystemExit(f"prompt desconocido en el manifiesto: {code}")


def verify_package_integrity() -> None:
    for item in manifest()["normative_files"]:
        path = PACKAGE / item["file"]
        if sha256_file(path) != item["sha256"]:
            raise SystemExit(f"hash del paquete v3 no coincide: {item['file']}")


def dependency_status(code: str) -> str:
    entry = prompt_entry(code)
    path = REPO / f"{entry['report']}.evidence.json"
    if not path.is_file():
        return "NOT_STARTED"
    try:
        return json.loads(path.read_text(encoding="utf-8")).get("status", "UNKNOWN")
    except json.JSONDecodeError:
        return "INVALID_EVIDENCE"


def cargo_ok(code: int, out: str) -> bool:
    """cargo test terminó bien, ejecutó al menos una prueba y ninguna falló o se ignoró."""
    if code != 0:
        return False
    results = [ln for ln in out.splitlines() if ln.strip().startswith("test result:")]
    ran = sum(int(ln.split(" passed")[0].rsplit(" ", 1)[-1]) for ln in results if " passed" in ln)
    ignored = sum(int(ln.split(" ignored")[0].rsplit(" ", 1)[-1]) for ln in results if " ignored" in ln)
    return ran > 0 and ignored == 0


def exit_zero(code: int, _out: str) -> bool:
    return code == 0


class Evidence:
    def __init__(self, code: str, extra_inputs: list[str] | None = None):
        # La consola de Windows usa cp1252: sin esto, un carácter no representable tumba el informe.
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        verify_package_integrity()
        self.entry = prompt_entry(code)
        self.code = code
        self.stem = Path(self.entry["report"]).name
        self.runs = REPORTS / f"{code}_runs"
        self.runs.mkdir(parents=True, exist_ok=True)
        inputs = [self.entry["file"]] + COMMON_INPUTS + [f"contracts/{c}" for c in self.entry["contracts"]]
        inputs += extra_inputs or []
        self.inputs = {name: sha256_file(PACKAGE / name) for name in dict.fromkeys(inputs)}
        self.tests: list[dict] = []

    # -- registro de pruebas -------------------------------------------------
    def run(self, test_id: str, kind: str, command: list[str], expected: str,
            ok: Callable[[int, str], bool] = exit_zero, cwd: Path | None = None,
            fixtures: list[Path] | None = None, timeout: int = 1800,
            env: dict | None = None) -> bool:
        print(f"==> {test_id}: {' '.join(command)}", flush=True)
        started = time.monotonic()
        full_env = {**os.environ, "PYTHONIOENCODING": "utf-8", "PYTHONUTF8": "1", **(env or {})}
        try:
            proc = subprocess.run(command, cwd=cwd or REPO, capture_output=True, timeout=timeout,
                                  env=full_env, check=False)
            code = proc.returncode
            out = (proc.stdout + b"\n" + proc.stderr).decode("utf-8", errors="replace")
        except subprocess.TimeoutExpired as exc:
            code = -1
            out = f"TIMEOUT tras {timeout}s\n" + ((exc.stdout or b"") + (exc.stderr or b"")).decode("utf-8", "replace")
        except OSError as exc:
            code = -2
            out = f"no se pudo ejecutar: {exc}"
        elapsed = time.monotonic() - started
        log = self.runs / f"{test_id}.log"
        log.write_text(f"$ {' '.join(command)}\n# exit={code} elapsed={elapsed:.1f}s\n{out}", encoding="utf-8")
        passed = ok(code, out)
        tail = " | ".join(ln.strip() for ln in out.strip().splitlines()[-4:] if ln.strip())[:600]
        self._add(test_id, kind, " ".join(command), expected,
                  f"exit={code}; {tail or 'sin salida'}", "PASS" if passed else "FAIL",
                  [log], fixtures)
        print(f"    {'PASS' if passed else 'FAIL'} ({elapsed:.1f}s)", flush=True)
        return passed

    def blocked(self, test_id: str, kind: str, command: str, expected: str, reason: str) -> None:
        self._add(test_id, kind, command, expected, f"NO EJECUTADA: {reason}", "BLOCKED", [], None)

    def _add(self, test_id, kind, command, expected, observed, result, paths, fixtures) -> None:
        if any(t["id"] == test_id for t in self.tests):
            raise SystemExit(f"id de prueba duplicado: {test_id}")
        self.tests.append({
            "id": test_id, "kind": kind, "command": command,
            "fixture_hashes": [f"{p.relative_to(REPO).as_posix() if p.is_relative_to(REPO) else p.name}:{sha256_file(p)}"
                               for p in (fixtures or [])],
            "expected": expected, "observed": observed, "result": result,
            "evidence_paths": [f"{p.relative_to(REPO).as_posix()}#sha256={sha256_file(p)}" for p in paths],
        })

    # -- cierre -----------------------------------------------------------------
    def finish(self, changed_files: list[str], blockers: list[str]) -> str:
        blockers = list(blockers)
        for dep in self.entry["dependencies"]:
            st = dependency_status(dep)
            if st != "REAL_PASS":
                blockers.append(f"dependencia {dep} en {st} (contrato 00: el consumidor no se completa sin REAL_PASS)")
        results = [t["result"] for t in self.tests]
        has_real = any(t["kind"] == "REAL" for t in self.tests)
        if "FAIL" in results:
            status = "BLOCKED"
        elif "BLOCKED" in results or blockers or not has_real:
            status = "BLOCKED_REAL" if has_real or blockers else "CODE_PASS"
        else:
            status = "REAL_PASS"
        evidence = {
            "prompt_id": self.code,
            "status": status,
            "host_id": f"{platform.node()} ({platform.platform()})",
            "environment_lock_hash": sha256_file(REPO / "environment.lock.json"),
            "inputs_hash": sha256_bytes(json.dumps(self.inputs, sort_keys=True).encode()),
            "changed_files": sorted(changed_files),
            "tests": self.tests,
            "blockers": blockers,
            "generated_at": now_utc(),
        }
        self._validate(evidence)
        out = REPORTS / f"{self.stem}.evidence.json"
        out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        (self.runs / "inputs.json").write_text(json.dumps(self.inputs, indent=2) + "\n", encoding="utf-8")
        counts = {r: results.count(r) for r in ("PASS", "FAIL", "BLOCKED", "INAPPLICABLE")}
        print(f"{self.code}: {status} {counts} bloqueos={len(blockers)} -> {out.relative_to(REPO)}")
        for t in self.tests:
            if t["result"] != "PASS":
                print(f"   {t['result']:<8} {t['id']}: {t['observed'][:200]}")
        for b in blockers:
            print(f"   BLOQUEO  {b}")
        return status

    @staticmethod
    def _validate(evidence: dict) -> None:
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        registry = Registry()
        for name in ("larm.schema.json", "domain.schema.json"):
            doc = json.loads((PACKAGE / "schemas" / name).read_text(encoding="utf-8"))
            registry = registry.with_resource(name, Resource.from_contents(doc))
        v = Draft202012Validator({"$ref": "larm.schema.json#/$defs/PromptEvidence"}, registry=registry)
        errors = list(v.iter_errors(evidence))
        if errors:
            raise SystemExit(f"evidence no cumple PromptEvidence: {errors[0].message}")


if __name__ == "__main__":
    print(__doc__)
    sys.exit(0)
