"""Private protocol bridge for P103; this module does not open a server or own durable jobs."""
from __future__ import annotations

import threading
import time
import re
from dataclasses import dataclass, field
from typing import Any, Callable

from worker import WorkerError, _object, _validate_uuid7, generate_image

SHA256 = re.compile(r"^[0-9a-f]{64}$")


@dataclass
class _Job:
    request_id: str
    cancel_event: threading.Event = field(default_factory=threading.Event)
    state: str = "RUNNING"
    phase: str = "LOADING"
    completed_steps: int = 0
    total_steps: int = 0


class DiffusersProtocolHandler:
    """Adapt one image-generation capability to P103's protocol hooks."""

    def __init__(
        self,
        *,
        fingerprint_provider: Callable[[], dict[str, Any] | None],
        progress_sink: Callable[[dict[str, Any]], None] | None = None,
        image_generator: Callable[..., dict[str, Any]] = generate_image,
    ):
        self._fingerprint_provider = fingerprint_provider
        self._progress_sink = progress_sink or (lambda _event: None)
        self._image_generator = image_generator
        self._lock = threading.RLock()
        self._jobs: dict[str, _Job] = {}

    def invoke(self, payload: Any) -> dict[str, Any]:
        request = _object(payload, {"lease_id", "request_id", "capability", "input", "generation", "stream"})
        _validate_uuid7(request["lease_id"], "lease_id")
        request_id = _validate_uuid7(request["request_id"], "request_id")
        if request["capability"] != "image.generate":
            raise WorkerError("UNSUPPORTED_CAPABILITY", "Diffusers sólo anuncia image.generate.")
        if request["stream"] is not False or request["generation"] is not None:
            raise WorkerError("UNSUPPORTED_CAPABILITY", "image.generate no admite streaming ni generación externa.")
        fingerprint = self._fingerprint_provider()
        if not self._valid_fingerprint(fingerprint):
            raise WorkerError("FINGERPRINT_REQUIRED", "P090 debe entregar una huella de ejecución antes de inferir.")
        with self._lock:
            previous = self._jobs.get(request_id)
            if previous is not None and previous.state in {"RUNNING", "CANCEL_REQUESTED"}:
                raise WorkerError("REQUEST_IN_PROGRESS", "El request_id ya tiene una inferencia activa.")
            job = _Job(request_id=request_id)
            self._jobs[request_id] = job
        self._emit(job, "LOADING")

        def update_progress(completed_steps: int, total_steps: int) -> None:
            with self._lock:
                job.phase = "RUNNING"
                job.completed_steps = completed_steps
                job.total_steps = total_steps
            self._emit(job, "RUNNING")

        started = time.perf_counter()
        try:
            result = self._image_generator(
                request_id,
                request["input"],
                progress=update_progress,
                cancelled=job.cancel_event.is_set,
            )
            if job.cancel_event.is_set():
                raise WorkerError("CANCELLED", "El trabajo se canceló antes de publicar el resultado.")
            with self._lock:
                job.state = "COMPLETED"
                job.phase = "COMPLETED"
            self._emit(job, "COMPLETED")
            return {
                "request_id": request_id,
                "capability": "image.generate",
                "output": result["output"],
                "fingerprint": fingerprint,
                "usage": {
                    "tokens_input": None,
                    "tokens_output": None,
                    "latency_ms": max(0.0, (time.perf_counter() - started) * 1000),
                },
            }
        except WorkerError as error:
            with self._lock:
                job.state = "CANCELLED" if error.code == "CANCELLED" else "FAILED"
                job.phase = job.state
            self._emit(job, job.phase)
            raise
        except Exception:
            with self._lock:
                job.state = "FAILED"
                job.phase = "FAILED"
            self._emit(job, "FAILED")
            raise

    def cancel(self, payload: Any) -> dict[str, bool]:
        request = _object(payload, {"request_id"})
        request_id = _validate_uuid7(request["request_id"], "request_id")
        with self._lock:
            job = self._jobs.get(request_id)
            if job is None or job.state not in {"RUNNING", "CANCEL_REQUESTED"}:
                return {"cancelled": False}
            job.cancel_event.set()
            job.state = "CANCEL_REQUESTED"
            job.phase = "CANCEL_REQUESTED"
        self._emit(job, "CANCEL_REQUESTED")
        return {"cancelled": True}

    def heartbeat(self) -> dict[str, Any]:
        with self._lock:
            active = sorted(
                request_id for request_id, job in self._jobs.items()
                if job.state in {"RUNNING", "CANCEL_REQUESTED"}
            )
        return {"alive": True, "running_request_ids": active}

    def progress(self, request_id: str) -> dict[str, Any] | None:
        with self._lock:
            job = self._jobs.get(request_id)
            if job is None:
                return None
            return {
                "request_id": job.request_id,
                "state": job.state,
                "phase": job.phase,
                "completed_steps": job.completed_steps,
                "total_steps": job.total_steps,
            }

    def _emit(self, job: _Job, phase: str) -> None:
        self._progress_sink({
            "method": "core.progress",
            "params": {
                "request_id": job.request_id,
                "phase": phase,
                "completed_bytes": None,
                "total_bytes": None,
                "message_code": "diffusers_generation_progress",
            },
        })

    @staticmethod
    def _valid_fingerprint(value: Any) -> bool:
        if not isinstance(value, dict) or set(value) != {"hash", "components"}:
            return False
        if not isinstance(value["hash"], str) or not SHA256.fullmatch(value["hash"]):
            return False
        components = value["components"]
        if not isinstance(components, dict) or not components:
            return False
        for name, item in components.items():
            if not isinstance(name, str) or not name or not isinstance(item, dict):
                return False
            if set(item) != {"value", "status", "evidence_refs"}:
                return False
            if item["value"] is not None and (not isinstance(item["value"], str) or not item["value"]):
                return False
            if not isinstance(item["status"], str) or item["status"] not in {"VERIFIED", "DECLARED", "UNAVAILABLE"}:
                return False
            if not isinstance(item["evidence_refs"], list) or not all(isinstance(ref, str) for ref in item["evidence_refs"]):
                return False
        return True
