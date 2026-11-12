"""Servidor NDJSON síncrono con callbacks concurrentes y cancelación cooperativa.

El host conserva timeout/kill y aislamiento. El SDK no concede permisos de IO,
no accede a secretos ni sustituye los guards/journal del producto.
"""
import concurrent.futures
import json
import re
import threading
import uuid

from .contracts import ContractViolation, Contracts

MAX_FRAME_BYTES = 1 << 20
UUID7 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")


def _unique(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("Clave duplicada")
        value[key] = item
    return value


def _constant(_value):
    raise ValueError("Número JSON no finito")


class RpcError(Exception):
    def __init__(self, code: int, message: str, data=None):
        super().__init__(message)
        self.code, self.message, self.data = code, message, data

    def wire(self):
        result = {"code": self.code, "message": self.message}
        if self.data is not None:
            result["data"] = self.data
        return result


class DomainError(RpcError):
    def __init__(self, code: str, message: str, reason: str, *, retryable=False):
        super().__init__(-32000, message, {
            "code": code, "message": message, "retryable": retryable,
            "correlation_id": str(uuid.uuid7()),
            "details": {"reason": reason, "evidence_refs": [], "resource_ids": [], "original_error": None},
        })


class CallContext:
    def __init__(self, request_id, cancelled, send):
        self.request_id, self._cancelled, self._send = request_id, cancelled, send

    @property
    def cancelled(self):
        return self._cancelled.is_set()

    def check_cancelled(self):
        if self.cancelled:
            raise DomainError("RESOURCE_UNAVAILABLE", "Operación cancelada", "operation_cancelled")

    def progress(self, phase, completed=None, total=None, message_code=None):
        # bytes siguen el tipo numérico del frame publicado, no el de entidades.
        self._send({"jsonrpc": "2.0", "method": "core.progress", "params": {
            "request_id": self.request_id, "phase": phase, "completed_bytes": completed,
            "total_bytes": total, "message_code": message_code,
        }}, "progress")


class ProtocolServer:
    def __init__(self, contracts: Contracts, manifest, handler, *, workers=4, queue_size=64):
        if not 1 <= workers <= 4 or not 0 <= queue_size <= 64:
            raise ValueError("Límites de concurrencia fuera del protocolo")
        self.contracts, self.manifest, self.handler = contracts, contracts.manifest(manifest), handler
        self.workers, self.capacity = workers, threading.BoundedSemaphore(workers + queue_size)
        self.pending, self.pending_lock, self.write_lock = {}, threading.Lock(), threading.Lock()
        self.handshaken = False
        self.output = None

    def _send(self, frame, kind="response"):
        self.contracts.frame(kind, frame)
        raw = json.dumps(frame, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")
        if len(raw) > MAX_FRAME_BYTES:
            raise DomainError("PROTOCOL_ERROR", "Frame demasiado grande", "frame_too_large")
        with self.write_lock:
            self.output.write(raw + b"\n")
            self.output.flush()

    def _error(self, request_id, error):
        frame = {"jsonrpc": "2.0", "id": request_id, "error": error.wire()}
        try:
            self._send(frame)
        except (ContractViolation, ValueError, DomainError):
            # Un error mal construido por el callback tampoco puede romper el wire.
            self._send({"jsonrpc": "2.0", "id": request_id, "error": DomainError("PROTOCOL_ERROR", "Error del adaptador fuera del contrato", "invalid_adapter_error").wire()})

    def _call(self, request, flag):
        identity, method = request["id"], request["method"]
        try:
            ctx = CallContext(identity, flag, self._send)
            ctx.check_cancelled()
            result = self.handler.call(method, request.get("params"), ctx)
            try:
                self.contracts.response(method, result)
            except ContractViolation as error:
                raise DomainError("PROTOCOL_ERROR", "Respuesta del adaptador fuera del contrato", "invalid_adapter_reply") from error
            self._send({"jsonrpc": "2.0", "id": identity, "result": result})
        except RpcError as error:
            self._error(identity, error)
        except Exception:
            # No imprime excepción, params, variables de entorno ni rutas privadas.
            self._error(identity, DomainError("INTERNAL", "Fallo interno del adaptador", "adapter_callback_failed"))
        finally:
            with self.pending_lock:
                self.pending.pop(identity, None)
            self.capacity.release()

    def _dispatch(self, request, pool):
        identity, method, params = request["id"], request["method"], request.get("params")
        with self.pending_lock:
            if identity in self.pending:
                raise DomainError("CONFLICT", "ID de llamada ya activo", "request_id_in_use")
        if method != "core.handshake" and not self.handshaken:
            raise DomainError("PROTOCOL_ERROR", "El primer mensaje debe ser handshake", "handshake_required")
        if method not in self.manifest["methods"]:
            raise RpcError(-32601, "Método no anunciado", DomainError("UNSUPPORTED_CAPABILITY", "Método no anunciado", "method_not_announced").data)
        try:
            if method == "core.handshake" and isinstance(params, dict):
                version = params.get("protocol_version")
                if isinstance(version, str) and re.fullmatch(r"\d+\.\d+", version) and version.split(".")[0] != "1":
                    # Comprueba la estructura del handshake antes de clasificar
                    # la incompatibilidad; no admite esa versión como contrato.
                    self.contracts.request(method, params | {"protocol_version": "1.0"})
                    raise DomainError("PROTOCOL_ERROR", "Handshake incompatible", "handshake_mismatch")
            self.contracts.request(method, params)
        except ContractViolation as error:
            raise RpcError(-32602, "Parámetros fuera del contrato") from error
        if method == "core.handshake":
            if params["adapter_id"] != self.manifest["id"] or params["protocol_version"].split(".")[0] != "1":
                raise DomainError("PROTOCOL_ERROR", "Handshake incompatible", "handshake_mismatch")
            if self.manifest["capabilities"]:
                # Import tardío: capabilities utiliza DomainError del servidor.
                from .capabilities import CapabilityRegistry
                registry = getattr(self.handler, "registry", None)
                if not isinstance(registry, CapabilityRegistry):
                    raise DomainError("UNSUPPORTED_CAPABILITY", "Falta el contrato de las capacidades anunciadas", "capability_descriptor_missing")
                registry.check_advertised(self.manifest["capabilities"])
            if not self.handshaken:
                start = getattr(self.handler, "open_session", None)
                if start:
                    try:
                        start()
                    except RpcError:
                        raise
                    except Exception:
                        raise DomainError("RESOURCE_UNAVAILABLE", "No se pudo abrir la sesión del adaptador", "adapter_session_start_failed") from None
            self.handshaken = True
            result = {"protocol_version": "1.0", "adapter_version": self.manifest["version"], "methods": self.manifest["methods"], "capabilities": self.manifest["capabilities"], "nonce": params["nonce"]}
        elif method == "core.heartbeat":
            with self.pending_lock:
                result = {"alive": True, "running_request_ids": sorted(self.pending)}
        elif method == "core.cancel":
            with self.pending_lock:
                flag = self.pending.get(params["request_id"])
                if flag:
                    flag.set()
                result = {"cancelled": flag is not None}
        else:
            with self.pending_lock:
                if identity in self.pending:
                    raise DomainError("CONFLICT", "ID de llamada ya activo", "request_id_in_use")
                if not self.capacity.acquire(blocking=False):
                    raise DomainError("RESOURCE_UNAVAILABLE", "Cola del adaptador llena", "adapter_queue_full", retryable=True)
                flag = threading.Event()
                self.pending[identity] = flag
            pool.submit(self._call, request, flag)
            return
        self.contracts.response(method, result)
        self._send({"jsonrpc": "2.0", "id": identity, "result": result})

    def serve(self, reader, writer):
        """Streams binarios. EOF cancela callbacks; shutdown libera recursos propios."""
        self.output = writer
        pool = concurrent.futures.ThreadPoolExecutor(max_workers=self.workers, thread_name_prefix="larm-adapter")
        try:
            while True:
                raw = reader.readline(MAX_FRAME_BYTES + 2)
                if not raw:
                    break
                if len(raw.removesuffix(b"\n")) > MAX_FRAME_BYTES:
                    while raw and not raw.endswith(b"\n"):
                        raw = reader.readline(MAX_FRAME_BYTES + 2)
                    self._error(None, RpcError(-32700, "Frame demasiado grande"))
                    continue
                if not raw.strip():
                    continue
                try:
                    request = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique, parse_constant=_constant)
                except (ValueError, UnicodeError, RecursionError):
                    self._error(None, RpcError(-32700, "JSON o UTF-8 inválidos"))
                    continue
                identity = request.get("id") if isinstance(request, dict) else None
                identity = identity if isinstance(identity, str) and UUID7.fullmatch(identity) else None
                try:
                    self.contracts.frame("request", request)
                except ContractViolation:
                    self._error(identity, RpcError(-32600, "Petición fuera del contrato"))
                    continue
                try:
                    self._dispatch(request, pool)
                except RpcError as error:
                    self._error(identity, error)
        finally:
            with self.pending_lock:
                for flag in self.pending.values():
                    flag.set()
            pool.shutdown(wait=True)
            shutdown = getattr(self.handler, "shutdown", None)
            if shutdown:
                shutdown()
