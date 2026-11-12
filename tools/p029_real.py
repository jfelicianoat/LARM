#!/usr/bin/env python3
"""Prueba REAL P029: restart del daemon con escrituras concurrentes y replay SSE."""
from __future__ import annotations

import concurrent.futures
import json
import os
import secrets
import socket
import sqlite3
import subprocess
import sys
import threading
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

REPO = Path(__file__).resolve().parent.parent
EXE = REPO / "target/debug/larm-daemon.exe"
RUNS = REPO / "docs/reports/P029_runs/real"
DEFAULTS = json.loads((REPO / "docs/contracts/defaults.json").read_text(encoding="utf-8"))
DB_RELATIVE = Path(DEFAULTS["paths"]["database_relative_path"])


def clean_env() -> dict[str, str]:
    return {key: value for key, value in os.environ.items() if not key.startswith("LARM_")}


def cli(root: Path, *args: str) -> dict:
    result = subprocess.run(
        [str(EXE), "--data-root", str(root), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        env=clean_env(),
        timeout=60,
        check=False,
    )
    if result.returncode:
        raise RuntimeError(f"daemon CLI falló para {args[0]} (exit={result.returncode})")
    return json.loads(result.stdout)


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def http(
    method: str,
    url: str,
    token: str | None = None,
    body: bytes | None = None,
    headers: dict[str, str] | None = None,
    timeout: float = 8,
) -> tuple[int, dict[str, str], dict | None]:
    request = Request(url, data=body, method=method)
    if token:
        request.add_header("authorization", f"Bearer {token}")
    for key, value in (headers or {}).items():
        request.add_header(key, value)
    try:
        with urlopen(request, timeout=timeout) as response:
            raw = response.read()
            return response.status, dict(response.headers), json.loads(raw) if raw else None
    except HTTPError as error:
        raw = error.read()
        return error.code, dict(error.headers), json.loads(raw) if raw else None


def launch(root: Path, port: int) -> subprocess.Popen:
    return subprocess.Popen(
        [str(EXE), "--data-root", str(root), "--port", str(port), "--serve-api"],
        env=clean_env() | {"LARM_LOG_FORMAT": "json"},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=subprocess.CREATE_NEW_PROCESS_GROUP,
    )


def wait_live(base: str, process: subprocess.Popen) -> None:
    for _ in range(100):
        if process.poll() is not None:
            raise RuntimeError(f"daemon terminó durante el arranque (exit={process.returncode})")
        try:
            if http("GET", f"{base}/api/v1/health/live", timeout=1)[0] == 200:
                return
        except (OSError, URLError):
            time.sleep(0.2)
    raise TimeoutError("daemon no abrió la API en 20 segundos")


class SseClient:
    def __init__(self, port: int, token: str, last_event_id: str | None = None):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=8)
        self.socket.settimeout(0.5)
        headers = [
            "GET /api/v1/events HTTP/1.1",
            "Host: 127.0.0.1",
            f"Authorization: Bearer {token}",
            "Accept: text/event-stream",
        ]
        if last_event_id is not None:
            headers.append(f"Last-Event-ID: {last_event_id}")
        self.socket.sendall(("\r\n".join(headers) + "\r\n\r\n").encode("ascii"))
        self.buffer = bytearray()
        head = self._until(b"\r\n\r\n", time.monotonic() + 8).decode("utf-8", "replace")
        if " 200 " not in head or "text/event-stream" not in head.lower():
            raise RuntimeError(f"SSE no abrió stream: {head.splitlines()[0] if head else 'sin cabeceras'}")

    def _until(self, marker: bytes, deadline: float) -> bytes:
        while marker not in self.buffer:
            if time.monotonic() >= deadline:
                raise TimeoutError("timeout leyendo SSE")
            try:
                chunk = self.socket.recv(4096)
            except socket.timeout:
                continue
            if not chunk:
                raise ConnectionError("el stream SSE terminó antes de completar el frame")
            self.buffer.extend(chunk)
        index = self.buffer.index(marker) + len(marker)
        found = bytes(self.buffer[:index])
        del self.buffer[:index]
        return found

    def next_event(self, timeout: float = 12) -> dict[str, str]:
        deadline = time.monotonic() + timeout
        while True:
            while True:
                matches = [
                    (self.buffer.find(marker), marker)
                    for marker in (b"\r\n\r\n", b"\n\n")
                    if self.buffer.find(marker) >= 0
                ]
                if matches:
                    offset, marker = min(matches, key=lambda item: item[0])
                    end = offset + len(marker)
                    frame_bytes = bytes(self.buffer[:end])
                    del self.buffer[:end]
                    break
                if time.monotonic() >= deadline:
                    raise TimeoutError("timeout leyendo SSE")
                try:
                    chunk = self.socket.recv(4096)
                except socket.timeout:
                    continue
                if not chunk:
                    raise ConnectionError("el stream SSE terminó antes de completar el frame")
                self.buffer.extend(chunk)
            frame = frame_bytes.decode("utf-8", "replace")
            parsed: dict[str, str] = {}
            data: list[str] = []
            for line in frame.splitlines():
                if not line or line.startswith(":" ):
                    continue
                key, _, value = line.partition(":")
                value = value[1:] if value.startswith(" ") else value
                if key == "data":
                    data.append(value)
                elif key in {"id", "event"}:
                    parsed[key] = value
            if data:
                parsed["data"] = "\n".join(data)
                return parsed

    def close(self) -> None:
        self.socket.close()


def create_credential(base: str, writer: str, run_id: str, suffix: str) -> tuple[int, dict | None]:
    body = json.dumps(
        {
            "service": f"larm/test/p029/{run_id}/{suffix}",
            "username": f"fixture-{run_id}",
            "secret_type": "TOKEN",
            "secret": "p029-fixture-" + secrets.token_hex(20),
            "scopes": [],
        }
    ).encode("utf-8")
    status, _, response = http(
        "POST",
        f"{base}/api/v1/credentials",
        writer,
        body,
        {"content-type": "application/json", "idempotency-key": f"p029-{uuid.uuid4()}"},
        timeout=4,
    )
    return status, response


def durable_test_records(database: Path) -> list[tuple[str, int]]:
    with sqlite3.connect(database, timeout=3) as connection:
        return connection.execute(
            "SELECT id,row_version FROM credential_reference WHERE service LIKE 'larm/test/p029/%' ORDER BY created_at,id"
        ).fetchall()


def delete_test_records(base: str, writer: str, database: Path) -> int:
    deleted = 0
    for resource_id, row_version in durable_test_records(database):
        status, _, _ = http(
            "DELETE",
            f"{base}/api/v1/credentials/{resource_id}",
            writer,
            headers={"if-match": str(row_version)},
        )
        if status not in (200, 204):
            raise RuntimeError(f"no se pudo limpiar credential fixture: HTTP {status}")
        deleted += 1
    return deleted


def main() -> int:
    RUNS.mkdir(parents=True, exist_ok=True)
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    root = RUNS / run_id
    root.mkdir()
    database = root / DB_RELATIVE
    database.parent.mkdir(parents=True, exist_ok=True)
    port = free_port()
    base = f"http://127.0.0.1:{port}"
    process: subprocess.Popen | None = None
    stream: SseClient | None = None
    reader: str | None = None
    writer: str | None = None
    cleaned = False
    steps: list[dict[str, object]] = []
    try:
        reader = cli(root, "enroll-principal", "--name", "p029-reader", "--kind", "OPERATOR", "--scopes", "read")["token"]
        writer = cli(
            root,
            "enroll-principal",
            "--name",
            "p029-writer",
            "--kind",
            "OPERATOR",
            "--scopes",
            "credentials:write,credentials:read",
        )["token"]
        process = launch(root, port)
        wait_live(base, process)
        stream = SseClient(port, reader)
        status, baseline = create_credential(base, writer, run_id, "baseline")
        if status != 201 or not baseline or "id" not in baseline:
            code = (baseline or {}).get("code", "sin código")
            reason = ((baseline or {}).get("details") or {}).get("reason", "sin razón")
            raise RuntimeError(f"no se pudo crear el evento base: HTTP {status} ({code}, {reason})")
        baseline_event = stream.next_event()
        baseline_envelope = json.loads(baseline_event["data"])
        cursor = baseline_event.get("id", "")
        if (
            baseline_event.get("event") != "credential.created"
            or baseline_envelope.get("resource_id") != baseline["id"]
            or not cursor.isdecimal()
        ):
            raise AssertionError("el consumidor no recibió el evento base durable esperado")
        steps.append({"step": "baseline", "result": "PASS", "event_id": cursor})

        start = threading.Event()

        def write(index: int) -> tuple[int, dict | None]:
            start.wait(timeout=5)
            return create_credential(base, writer or "", run_id, f"concurrent/{index:02d}")

        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
            writes = [executor.submit(write, index) for index in range(40)]
            start.set()
            first_success: tuple[int, dict | None] | None = None
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline and first_success is None:
                done, _ = concurrent.futures.wait(
                    writes,
                    timeout=0.2,
                    return_when=concurrent.futures.FIRST_COMPLETED,
                )
                for completed in done:
                    result = completed.result()
                    if result[0] == 201 and result[1] is not None:
                        first_success = result
                        break
            if first_success is None:
                raise RuntimeError("ninguna escritura concurrente confirmó mientras el daemon seguía activo")
            in_flight = sum(not future.done() for future in writes)
            process.kill()
            process.wait(timeout=10)
            if in_flight == 0:
                raise AssertionError("el daemon se detuvo después de terminar todas las escrituras concurrentes")
            if stream:
                stream.close()
                stream = None
            outcomes: list[tuple[int, dict | None]] = []
            for future in writes:
                try:
                    outcomes.append(future.result(timeout=5))
                except (concurrent.futures.TimeoutError, OSError, URLError):
                    outcomes.append((0, None))
        committed = durable_test_records(database)
        concurrent_records = [
            resource_id
            for resource_id, _ in committed
            if resource_id != baseline["id"]
        ]
        if not concurrent_records:
            raise AssertionError("no quedó ninguna escritura concurrente confirmada en SQLite")
        steps.append(
            {
                "step": "restart-during-concurrent-writes",
                "result": "PASS",
                "in_flight_at_kill": in_flight,
                "http_successes": sum(status == 201 for status, _ in outcomes),
                "durable_records": len(concurrent_records),
            }
        )

        process = launch(root, port)
        wait_live(base, process)
        stream = SseClient(port, reader, cursor)
        expected = set(concurrent_records)
        seen_event_ids: set[str] = set()
        seen_resources: set[str] = set()
        while not expected.issubset(seen_resources):
            event = stream.next_event(timeout=15)
            event_id = event.get("id", "")
            envelope = json.loads(event["data"])
            if not event_id.isdecimal() or event_id in seen_event_ids:
                raise AssertionError(f"replay visible duplicado o inválido: {event_id}")
            seen_event_ids.add(event_id)
            if envelope.get("resource_id"):
                seen_resources.add(envelope["resource_id"])
        if not expected.issubset(seen_resources):
            raise AssertionError("hay filas confirmadas sin evento reproducido")
        steps.append(
            {
                "step": "replay-after-process-restart",
                "result": "PASS",
                "last_event_id": cursor,
                "expected_durable_resources": len(expected),
                "replayed_events": len(seen_event_ids),
            }
        )

        status, sentinel = create_credential(base, writer, run_id, "sentinel")
        if status != 201 or not sentinel:
            raise RuntimeError(f"no se pudo crear sentinel live: HTTP {status}")
        sentinel_seen = False
        while not sentinel_seen:
            event = stream.next_event(timeout=10)
            event_id = event.get("id", "")
            envelope = json.loads(event["data"])
            if not event_id.isdecimal() or event_id in seen_event_ids:
                raise AssertionError(f"evento live duplicado o inválido después del replay: {event_id}")
            seen_event_ids.add(event_id)
            if envelope.get("resource_id") == sentinel["id"]:
                sentinel_seen = True
        steps.append({"step": "replay-live-boundary-no-duplicate", "result": "PASS"})

        deleted = delete_test_records(base, writer, database)
        cleaned = True
        steps.append({"step": "fixture-cleanup", "result": "PASS", "records_deleted": deleted})
        print(f"PASS: proceso reiniciado con {in_flight} escrituras en curso; {len(expected)} filas durables replayadas sin duplicados")
        return 0
    except Exception as error:
        steps.append({"step": "P029-REAL", "result": "FAIL", "reason": type(error).__name__})
        print(f"FAIL: P029 real: {type(error).__name__}: {error}", file=sys.stderr)
        return 1
    finally:
        if stream:
            stream.close()
            stream = None
        if writer and database.exists() and not cleaned:
            try:
                if process is None or process.poll() is not None:
                    process = launch(root, port)
                    wait_live(base, process)
                deleted = delete_test_records(base, writer, database)
                steps.append({"step": "fixture-cleanup", "result": "PASS", "records_deleted": deleted})
            except Exception as cleanup_error:
                steps.append(
                    {
                        "step": "fixture-cleanup",
                        "result": "BLOCKED",
                        "reason": type(cleanup_error).__name__,
                    }
                )
        if process and process.poll() is None:
            process.kill()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
        if reader:
            try:
                cli(root, "revoke-principal", "--name", "p029-reader")
            except Exception:
                pass
        if writer:
            try:
                cli(root, "revoke-principal", "--name", "p029-writer")
            except Exception:
                pass
        (root / "steps.json").write_text(json.dumps(steps, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(main())
