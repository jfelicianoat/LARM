#!/usr/bin/env python3
"""Oráculos comunes reutilizables. P025 añade oráculos de IO de cada referencia.

CLI: runner.py --package <paquete> --cases <JSON de requests de prueba>
No promueve instalaciones ni se presenta como suite completa de P026.
"""
import argparse
import hashlib
import json
from pathlib import Path

from session import Session


def common(session, *, valid_method, valid_params, cancel_method, cancel_params):
    outcomes = []
    def passed(name, condition):
        assert condition, name
        outcomes.append({"id": name, "result": "PASS"})
    early = session.call(valid_method, valid_params)
    passed("handshake-required", early.get("error", {}).get("data", {}).get("code") == "PROTOCOL_ERROR")
    result = session.handshake()
    passed("handshake-nonce-capabilities", result["adapter_version"] == session.manifest["version"])
    unknown = session.call("conformance.method_not_declared", {}, validate=False)
    passed("unannounced-method", unknown.get("error", {}).get("code") == -32601 and unknown["error"]["data"]["code"] == "UNSUPPORTED_CAPABILITY")
    invalid = session.call(valid_method, None, validate=False)
    passed("invalid-params", invalid.get("error", {}).get("code") == -32602)
    valid = session.call(valid_method, valid_params)
    passed("valid-response-schema", "result" in valid)  # Session valida el DTO contra la tabla normativa.
    request = session.send(cancel_method, cancel_params)
    progress = session.wait_progress(request)
    beat = session.result("core.heartbeat", {})
    passed("progress-heartbeat", progress["request_id"] == request and request in beat["running_request_ids"])
    cancelled = session.result("core.cancel", {"request_id": request})
    reply = session.response(request)
    reason = reply.get("error", {}).get("data", {}).get("details", {}).get("reason", "")
    passed("cooperative-cancel", cancelled["cancelled"] and "cancel" in reason)
    return outcomes


def executable(package, manifest):
    root = Path(package).resolve(strict=True)
    relative = Path(manifest["executable"]["relative_path"])
    candidate = (root / relative).resolve(strict=True)
    if relative.is_absolute() or not candidate.is_relative_to(root) or not candidate.is_file():
        raise ValueError("Ejecutable fuera del paquete")
    return [str(candidate), *manifest["executable"]["args"]]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--cases", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--workdir", type=Path)
    args = parser.parse_args()
    manifest = json.loads((args.package / "manifest.json").read_text(encoding="utf-8"))
    cases = json.loads(args.cases.read_text(encoding="utf-8"))
    session = Session(executable(args.package, manifest), manifest, cwd=args.workdir or args.package)
    try:
        outcomes = common(session, **cases)
    finally:
        session.close()
    outcomes.append({"id": "shutdown-stdout-contract", "result": "PASS"})
    record = {"scope": "common protocol only; not full family qualification P026", "result": "PASS", "tests": outcomes, "cases_sha256": hashlib.sha256(args.cases.read_bytes()).hexdigest()}
    args.out.write_text(json.dumps(record, indent=2), encoding="utf-8")
    print(f"PASS: {len(outcomes)} oráculos comunes; no promoción ni cualificación oficial")


if __name__ == "__main__":
    main()
