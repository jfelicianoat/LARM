#!/usr/bin/env python3
"""Worker de adaptador falso (fixture común, P007) que habla el protocolo del contrato 03.

NDJSON JSON-RPC 2.0 por stdio: una línea UTF-8 por frame; stdout solo frames, stderr logs.
Implementa core.handshake, core.cancel, core.heartbeat y un método de prueba `test.work`
({"steps": n, "step_ms": t}) que emite core.progress y puede cancelarse. Los modos de fallo
sirven a las pruebas del adapter host (P020/P021) con un proceso real:

  --mode normal            protocolo correcto
  --mode bad-nonce         el handshake devuelve otro nonce
  --mode bad-version       el handshake devuelve protocol_version 2.0
  --mode crash-after N     termina con código 3 tras N frames recibidos
  --mode hang              nunca responde (timeout)
  --mode no-heartbeat      no responde a core.heartbeat
  --mode garbage           escribe una línea que no es JSON en stdout
  --mode oversize          responde un frame de más de 1 MiB
  --mode stderr-flood      escribe más de 10 MiB en stderr
--methods amplía la lista de métodos anunciados (separados por comas).
"""
import argparse
import json
import sys
import threading
import time

FRAME_LIMIT = 1024 * 1024
lock = threading.Lock()
running: dict[str, threading.Event] = {}


def send(frame: dict) -> None:
    line = json.dumps(frame, separators=(",", ":"), ensure_ascii=False)
    with lock:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()


def error(rid, code: int, message: str, data=None) -> None:
    err = {"code": code, "message": message}
    if data is not None:
        err["data"] = data
    send({"jsonrpc": "2.0", "id": rid, "error": err})


def larm_error(code: str, message: str, reason: str) -> dict:
    return {"code": code, "message": message, "retryable": False,
            "correlation_id": "01a12900-0000-7000-8000-000000000000",
            "details": {"reason": reason, "evidence_refs": [], "resource_ids": [], "original_error": None}}


def work(rid, params: dict) -> None:
    cancel = running.setdefault(rid, threading.Event())
    steps, step_ms = int(params.get("steps", 3)), int(params.get("step_ms", 50))
    for i in range(steps):
        if cancel.wait(step_ms / 1000):
            running.pop(rid, None)
            error(rid, -32000, "cancelado", larm_error("CANCELLED", "Operación cancelada", "cancelled"))
            return
        send({"jsonrpc": "2.0", "method": "core.progress",
              "params": {"request_id": rid, "phase": "work", "completed_bytes": str(i + 1),
                         "total_bytes": str(steps), "message_code": "step"}})
    running.pop(rid, None)
    send({"jsonrpc": "2.0", "id": rid, "result": {"done": steps}})


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="normal")
    ap.add_argument("--n", type=int, default=1)
    ap.add_argument("--adapter-version", default="0.0.1")
    ap.add_argument("--methods", default="")
    args = ap.parse_args()
    methods = ["core.handshake", "core.cancel", "core.heartbeat", "test.work"] + [m for m in args.methods.split(",") if m]
    if args.mode == "stderr-flood":
        chunk = "x" * 65536 + "\n"
        for _ in range(170):
            sys.stderr.write(chunk)
        sys.stderr.flush()
    received = 0
    for raw in sys.stdin:
        received += 1
        if args.mode == "crash-after" and received > args.n:
            return 3
        if args.mode == "hang":
            continue
        if args.mode == "garbage":
            sys.stdout.write("esto no es JSON\n")
            sys.stdout.flush()
            continue
        if len(raw.encode("utf-8")) > FRAME_LIMIT:
            error(None, -32700, "frame mayor de 1 MiB")
            continue
        try:
            frame = json.loads(raw)
        except json.JSONDecodeError:
            error(None, -32700, "JSON mal formado")
            continue
        rid, method, params = frame.get("id"), frame.get("method"), frame.get("params") or {}
        if frame.get("jsonrpc") != "2.0" or not isinstance(method, str):
            error(rid, -32600, "petición inválida")
            continue
        if method == "core.handshake":
            if params.get("protocol_version") != "1.0" or not params.get("nonce"):
                error(rid, -32602, "parámetros de handshake inválidos")
                continue
            nonce = "otro-nonce" if args.mode == "bad-nonce" else params["nonce"]
            version = "2.0" if args.mode == "bad-version" else "1.0"
            send({"jsonrpc": "2.0", "id": rid, "result": {"protocol_version": version, "adapter_version": args.adapter_version,
                                                          "methods": methods, "capabilities": ["test"], "nonce": nonce}})
        elif method == "core.heartbeat":
            if args.mode != "no-heartbeat":
                send({"jsonrpc": "2.0", "id": rid, "result": {"alive": True, "running_request_ids": sorted(running)}})
        elif method == "core.cancel":
            target = params.get("request_id")
            event = running.get(target)
            if event:
                event.set()
            send({"jsonrpc": "2.0", "id": rid, "result": {"cancelled": event is not None}})
        elif method == "test.work":
            if args.mode == "oversize":
                send({"jsonrpc": "2.0", "id": rid, "result": {"blob": "x" * (FRAME_LIMIT + 10)}})
                continue
            threading.Thread(target=work, args=(rid, params), daemon=True).start()
        elif method in methods:
            error(rid, -32000, "no implementado en el fake", larm_error("UNSUPPORTED_CAPABILITY", "No soportado", "fake"))
        else:
            # Método no anunciado: -32601 (contrato 03), nunca éxito vacío.
            error(rid, -32601, f"método desconocido {method}")
    time.sleep(0.05)
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8", newline="\n")
    raise SystemExit(main())
