#!/usr/bin/env python3
"""Loopback-only OpenAI text-generation subset for an approved runtime host."""
from __future__ import annotations

import hmac
import ipaddress
import json
import socket
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable

MAX_BODY_BYTES = 1_048_576
MAX_TOKENS = 256


class SidecarError(Exception):
    def __init__(self, status: int, code: str, message: str):
        super().__init__(message)
        self.status = status
        self.code = code
        self.message = message


def _duplicate_rejecting_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def _closed_object(value: Any, required: set[str], optional: set[str] = frozenset()) -> dict[str, Any]:
    if not isinstance(value, dict) or not required.issubset(value) or set(value) - required - optional:
        raise SidecarError(400, "invalid_request_error", "El cuerpo no coincide con el subset admitido.")
    return value


def _limit(value: Any) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= MAX_TOKENS:
        raise SidecarError(400, "invalid_request_error", "max_tokens debe estar entre 1 y 256.")
    return value


def completion_request(body: Any, expected_model: str) -> tuple[str, int, bool]:
    value = _closed_object(body, {"model", "prompt"}, {"max_tokens", "stream"})
    if value["model"] != expected_model:
        raise SidecarError(404, "model_not_found", "El modelo solicitado no está cargado en esta instancia.")
    if not isinstance(value["prompt"], str) or not value["prompt"].strip() or len(value["prompt"]) > 16_000:
        raise SidecarError(400, "invalid_request_error", "prompt debe ser texto no vacío de hasta 16000 caracteres.")
    stream = value.get("stream", False)
    if stream is not False:
        raise SidecarError(400, "unsupported_parameter", "El streaming no forma parte del subset disponible.")
    return value["prompt"], _limit(value.get("max_tokens", 64)), False


def chat_request(body: Any, expected_model: str) -> tuple[str, int, bool]:
    value = _closed_object(body, {"model", "messages"}, {"max_tokens", "stream"})
    if value["model"] != expected_model:
        raise SidecarError(404, "model_not_found", "El modelo solicitado no está cargado en esta instancia.")
    messages = value["messages"]
    if not isinstance(messages, list) or not messages or len(messages) > 64:
        raise SidecarError(400, "invalid_request_error", "messages debe contener entre 1 y 64 mensajes.")
    lines: list[str] = []
    for message in messages:
        item = _closed_object(message, {"role", "content"})
        if (
            not isinstance(item["role"], str)
            or item["role"] not in {"system", "user", "assistant"}
            or not isinstance(item["content"], str)
        ):
            raise SidecarError(400, "invalid_request_error", "Sólo se admiten mensajes system, user y assistant en texto.")
        lines.append(f"{item['role']}: {item['content']}")
    stream = value.get("stream", False)
    if stream is not False:
        raise SidecarError(400, "unsupported_parameter", "El streaming no forma parte del subset disponible.")
    lines.append("assistant:")
    return "\n".join(lines), _limit(value.get("max_tokens", 64)), False


def completion_response(
    *,
    text: str,
    model: str,
    prompt_tokens: int,
    completion_tokens: int,
    finish_reason: str,
    chat: bool,
) -> dict[str, Any]:
    if finish_reason not in {"stop", "length"}:
        raise SidecarError(500, "runtime_error", "El runtime devolvió una causa de fin desconocida.")
    choice: dict[str, Any]
    if chat:
        choice = {"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": finish_reason}
        object_type = "chat.completion"
        response_id = "chatcmpl-" + uuid.uuid4().hex
    else:
        choice = {"index": 0, "text": text, "finish_reason": finish_reason}
        object_type = "text_completion"
        response_id = "cmpl-" + uuid.uuid4().hex
    return {
        "id": response_id,
        "object": object_type,
        "created": int(time.time()),
        "model": model,
        "choices": [choice],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    }


def create_server(
    *,
    host: str,
    port: int,
    model: str,
    bearer_token: str,
    generate: Callable[[str, int], dict[str, Any]],
) -> ThreadingHTTPServer:
    try:
        address = ipaddress.ip_address(host)
    except ValueError as error:
        raise ValueError("sidecar host must be a loopback IP address") from error
    if not address.is_loopback or isinstance(port, bool) or not isinstance(port, int) or not 0 <= port <= 65535:
        raise ValueError("sidecar must bind a valid loopback endpoint")
    if not isinstance(model, str) or not model or len(model) > 200:
        raise ValueError("model id is required")
    if (
        not isinstance(bearer_token, str)
        or not 32 <= len(bearer_token) <= 1024
        or not bearer_token.isascii()
        or any(ord(char) < 33 or ord(char) > 126 for char in bearer_token)
    ):
        raise ValueError("sidecar requires a host-provided bearer token")

    class Handler(BaseHTTPRequestHandler):
        server_version = "LARM-Transformers/1"
        sys_version = ""

        def log_message(self, _format: str, *args: Any) -> None:
            return

        def _authorized(self) -> bool:
            header = self.headers.get("Authorization", "")
            supplied = header[7:] if header.startswith("Bearer ") else ""
            return hmac.compare_digest(supplied.encode("utf-8"), bearer_token.encode("utf-8"))

        def _send(self, status: int, value: dict[str, Any]) -> None:
            raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(raw)

        def _error(self, error: SidecarError) -> None:
            self._send(error.status, {"error": {"message": error.message, "type": error.code, "code": error.code}})

        def _body(self) -> Any:
            try:
                size = int(self.headers.get("Content-Length", "-1"))
            except ValueError:
                raise SidecarError(400, "invalid_request_error", "Content-Length no es válido.") from None
            if size < 0 or size > MAX_BODY_BYTES:
                raise SidecarError(413, "request_too_large", "El cuerpo supera el límite del sidecar.")
            try:
                return json.loads(
                    self.rfile.read(size).decode("utf-8", errors="strict"),
                    object_pairs_hook=_duplicate_rejecting_object,
                )
            except (UnicodeError, ValueError, json.JSONDecodeError):
                raise SidecarError(400, "invalid_request_error", "El cuerpo debe ser JSON UTF-8 válido.") from None

        def do_GET(self) -> None:
            if not self._authorized():
                self._send(401, {"error": {"message": "Autenticación requerida.", "type": "authentication_error", "code": "unauthorized"}})
                return
            if self.path == "/health":
                self._send(200, {"status": "ok", "model": model})
            elif self.path == "/v1/models":
                self._send(200, {"object": "list", "data": [{"id": model, "object": "model", "owned_by": "larm"}]})
            else:
                self._send(404, {"error": {"message": "Ruta desconocida.", "type": "invalid_request_error", "code": "not_found"}})

        def do_POST(self) -> None:
            if not self._authorized():
                self._send(401, {"error": {"message": "Autenticación requerida.", "type": "authentication_error", "code": "unauthorized"}})
                return
            try:
                body = self._body()
                if self.path == "/v1/completions":
                    prompt, limit, chat = completion_request(body, model)
                elif self.path == "/v1/chat/completions":
                    prompt, limit, chat = chat_request(body, model)
                else:
                    raise SidecarError(404, "invalid_request_error", "Ruta desconocida.")
                result = generate(prompt, limit)
                response = completion_response(
                    text=result["text"],
                    model=model,
                    prompt_tokens=result["prompt_tokens"],
                    completion_tokens=result["completion_tokens"],
                    finish_reason=result["finish_reason"],
                    chat=chat,
                )
                self._send(200, response)
            except SidecarError as error:
                self._error(error)
            except Exception:
                self._error(SidecarError(500, "runtime_error", "La generación local falló."))

    class LoopbackServer(ThreadingHTTPServer):
        address_family = socket.AF_INET6 if address.version == 6 else socket.AF_INET

    return LoopbackServer((host, port), Handler)
