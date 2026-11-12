#!/usr/bin/env python3
"""Private, fixture-only worker for the Microsoft Olive integration.

This process is not a public LARM API or a replacement for the adapter host.
It accepts one strict JSON request on stdin and emits one JSON response.
"""
from __future__ import annotations

import hashlib
import importlib
import importlib.metadata
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent
FIXTURE_LOCK_PATH = ROOT / "fixtures.json"
MAX_REQUEST_BYTES = 1024 * 1024
MAX_OUTPUT_BYTES = 1024 * 1024 * 1024
MAX_OUTPUT_FILES = 5000
MAX_TIMEOUT_SECONDS = 1800.0
PRECISIONS = {"fp32", "fp16", "int8", "int4"}
PROTOCOL_VERSION = "1.0"


class WorkerError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def _no_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise WorkerError("INVALID_REQUEST", "JSON contiene una clave duplicada.")
        result[key] = value
    return result


def _load_fixture_lock() -> dict[str, dict[str, Any]]:
    try:
        lock = json.loads(FIXTURE_LOCK_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise WorkerError("FIXTURE_LOCK_UNAVAILABLE", "No se pudo leer el lock de fixtures.") from exc
    if set(lock) != {"schema_version", "fixtures"} or lock["schema_version"] != "1.0":
        raise WorkerError("FIXTURE_LOCK_INVALID", "El lock de fixtures no cumple el protocolo.")
    fixtures: dict[str, dict[str, Any]] = {}
    for item in lock["fixtures"]:
        if set(item) != {"fixture_id", "relative_path", "sha256", "size_bytes", "format", "authorized_operations"}:
            raise WorkerError("FIXTURE_LOCK_INVALID", "Un descriptor del lock tiene campos inesperados.")
        if item["fixture_id"] in fixtures:
            raise WorkerError("FIXTURE_LOCK_INVALID", "El lock contiene IDs duplicados.")
        fixtures[item["fixture_id"]] = item
    return fixtures


FIXTURES = _load_fixture_lock()


def _fixture_path(fixture_id: str) -> tuple[dict[str, Any], Path]:
    item = FIXTURES.get(fixture_id)
    if item is None or "execute_fixture" not in item["authorized_operations"]:
        raise WorkerError("FIXTURE_NOT_AUTHORIZED", "La fixture no está autorizada para ejecución.")
    relative = Path(item["relative_path"])
    if relative.is_absolute() or ".." in relative.parts or relative.as_posix() != item["relative_path"]:
        raise WorkerError("FIXTURE_LOCK_INVALID", "El lock contiene una ruta no relativa segura.")
    path = ROOT / relative
    _no_reparse_path(path)
    if path.is_symlink() or (hasattr(os.path, "isjunction") and os.path.isjunction(path)):
        raise WorkerError("FIXTURE_NOT_AUTHORIZED", "La fixture no puede ser un enlace o junction.")
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(ROOT.resolve(strict=True))
    except (OSError, ValueError) as exc:
        raise WorkerError("FIXTURE_NOT_AUTHORIZED", "La fixture no está dentro del paquete autorizado.") from exc
    digest, size = _hash_file(resolved)
    if digest != item["sha256"] or size != item["size_bytes"]:
        raise WorkerError("FIXTURE_HASH_MISMATCH", "La fixture no coincide con su hash fijado.")
    return item, resolved


def _hash_file(path: Path) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def _managed_python() -> bool:
    configured = os.environ.get("LARM_PYTHON_ROOT")
    if not configured or sys.prefix == sys.base_prefix:
        return False
    try:
        expected = Path(configured).resolve(strict=True)
        actual = Path(sys.prefix).resolve(strict=True)
    except OSError:
        return False
    return os.path.normcase(str(expected)) == os.path.normcase(str(actual))


def _distribution_version(*names: str) -> tuple[str | None, str | None]:
    for name in names:
        try:
            return name, importlib.metadata.version(name)
        except importlib.metadata.PackageNotFoundError:
            continue
        except ValueError:
            continue
    return None, None


def _environment() -> dict[str, Any]:
    managed = _managed_python()
    if not managed:
        return {
            "managed_python": False,
            "python_version": platform_python_version(),
            "olive_distribution": None,
            "olive_ai_version": None,
            "onnxruntime_distribution": None,
            "onnxruntime_version": None,
            "onnx_version": None,
            "available_providers": None,
            "olive_optimize_available": None,
        }

    olive_distribution, olive_version = _distribution_version("olive-ai")
    ort_distribution, ort_version = _distribution_version(
        "onnxruntime", "onnxruntime-gpu", "onnxruntime-directml", "onnxruntime-openvino"
    )
    _, onnx_version = _distribution_version("onnx")
    providers: list[str] | None = None
    if ort_version is not None:
        try:
            ort = importlib.import_module("onnxruntime")
            detected = ort.get_available_providers()
            if isinstance(detected, (list, tuple)) and all(isinstance(value, str) for value in detected):
                providers = sorted(set(detected))
        except Exception:
            providers = None
    optimize_available: bool | None = None
    if olive_version is not None:
        try:
            olive = importlib.import_module("olive")
            optimize_available = callable(getattr(olive, "optimize", None))
        except Exception:
            optimize_available = False
    return {
        "managed_python": True,
        "python_version": platform_python_version(),
        "olive_distribution": olive_distribution,
        "olive_ai_version": olive_version,
        "onnxruntime_distribution": ort_distribution,
        "onnxruntime_version": ort_version,
        "onnx_version": onnx_version,
        "available_providers": providers,
        "olive_optimize_available": optimize_available,
    }


def platform_python_version() -> str:
    return f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"


def _versions(env: dict[str, Any]) -> dict[str, Any]:
    return {
        "managed_python": env["managed_python"],
        "python": env["python_version"],
        "olive_ai": env["olive_ai_version"],
        "onnxruntime": env["onnxruntime_version"],
        "onnx": env["onnx_version"],
    }


def _validate_request(request: Any) -> tuple[str, dict[str, Any]]:
    if not isinstance(request, dict) or request.get("protocol_version") != PROTOCOL_VERSION:
        raise WorkerError("INVALID_REQUEST", "Se requiere protocol_version 1.0.")
    operation = request.get("operation")
    if not isinstance(operation, str):
        raise WorkerError("INVALID_REQUEST", "operation debe ser texto.")
    if operation in {"probe", "version", "capabilities"}:
        expected = {"protocol_version", "operation"}
        if set(request) != expected:
            raise WorkerError("INVALID_REQUEST", "La operación contiene campos inesperados.")
        return operation, {}
    if operation == "plan":
        expected = {"protocol_version", "operation", "fixture_id", "provider_target", "precision"}
        if set(request) != expected:
            raise WorkerError("INVALID_REQUEST", "La solicitud plan no coincide con el contrato cerrado.")
        return operation, _validate_transform_parameters(request)
    if operation == "execute_fixture":
        expected = {"protocol_version", "operation", "fixture_id", "provider_target", "precision", "plan_hash"}
        if set(request) != expected:
            raise WorkerError("INVALID_REQUEST", "La solicitud execute_fixture no coincide con el contrato cerrado.")
        params = _validate_transform_parameters(request)
        plan_hash = request["plan_hash"]
        if not isinstance(plan_hash, str) or len(plan_hash) != 64 or any(ch not in "0123456789abcdef" for ch in plan_hash):
            raise WorkerError("INVALID_REQUEST", "plan_hash debe ser SHA256 hexadecimal minúsculo.")
        params["plan_hash"] = plan_hash
        return operation, params
    raise WorkerError("INVALID_REQUEST", "Operación desconocida.")


def _validate_transform_parameters(request: dict[str, Any]) -> dict[str, Any]:
    fixture_id = request.get("fixture_id")
    provider = request.get("provider_target")
    precision = request.get("precision")
    if not isinstance(fixture_id, str) or not isinstance(provider, str) or not provider or len(provider) > 80:
        raise WorkerError("INVALID_REQUEST", "Fixture o provider_target inválido.")
    if not all(ch in "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_" for ch in provider):
        raise WorkerError("INVALID_REQUEST", "provider_target contiene caracteres no permitidos.")
    if not isinstance(precision, str) or precision.lower() not in PRECISIONS:
        raise WorkerError("INVALID_REQUEST", "precision no está permitida por este worker.")
    _fixture_path(fixture_id)
    return {"fixture_id": fixture_id, "provider_target": provider, "precision": precision.lower()}


def _canonical_hash(value: dict[str, Any]) -> str:
    # The plan contains only strings, integers, booleans, nulls, arrays and objects;
    # this sorted UTF-8 serialization is RFC 8785-compatible for those values.
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _blockers(env: dict[str, Any], provider_target: str) -> list[str]:
    reasons: list[str] = []
    if not env["managed_python"]:
        reasons.append("MANAGED_PYTHON_REQUIRED")
    if env["olive_ai_version"] is None:
        reasons.append("OLIVE_NOT_INSTALLED")
    elif env["olive_optimize_available"] is not True:
        reasons.append("OLIVE_OPTIMIZE_UNAVAILABLE")
    if env["onnxruntime_version"] is None or env["available_providers"] is None:
        reasons.append("ONNXRUNTIME_UNAVAILABLE")
    elif provider_target not in env["available_providers"]:
        reasons.append("PROVIDER_UNAVAILABLE")
    return reasons


def _plan(params: dict[str, Any], env: dict[str, Any]) -> dict[str, Any]:
    fixture, _ = _fixture_path(params["fixture_id"])
    provider_available = None if env["available_providers"] is None else params["provider_target"] in env["available_providers"]
    blockers = _blockers(env, params["provider_target"])
    plan = {
        "fixture": {
            "fixture_id": fixture["fixture_id"],
            "sha256": fixture["sha256"],
            "size_bytes": str(fixture["size_bytes"]),
            "format": fixture["format"],
        },
        "input_estimate": {"bytes": str(fixture["size_bytes"]), "confidence": "EXACT"},
        "output_estimate": {"bytes": None, "confidence": "UNKNOWN", "reason": "NO_QUALIFIED_OLIVE_SIZE_ESTIMATOR"},
        "dependencies": _versions(env),
        "provider_target": params["provider_target"],
        "provider_target_available": provider_available,
        "parameters": {
            "precision": params["precision"],
            "provider_target": params["provider_target"],
            "trust_remote_code": False,
        },
        "eligible": not blockers,
        "blockers": blockers,
    }
    plan["plan_hash"] = _canonical_hash(plan)
    return plan


def _probe(env: dict[str, Any]) -> dict[str, Any]:
    return {
        "managed_python": env["managed_python"],
        "olive_ai_installed": None if not env["managed_python"] else env["olive_ai_version"] is not None,
        "onnxruntime_installed": None if not env["managed_python"] else env["onnxruntime_version"] is not None,
        "available_providers": env["available_providers"],
    }


def _capabilities(env: dict[str, Any]) -> dict[str, Any]:
    available = env["managed_python"] and env["olive_optimize_available"] is True and env["available_providers"] is not None
    return {
        "operations": ["probe", "version", "capabilities", "plan", "execute_fixture"],
        "fixture_ids": sorted(FIXTURES),
        "providers": env["available_providers"],
        "execute_fixture_available": bool(available),
        "input_formats": ["ONNX"],
        "output_format": "ONNX",
    }


def _no_reparse_path(path: Path) -> None:
    absolute = Path(os.path.abspath(path))
    current = Path(absolute.anchor)
    for component in absolute.parts[1:]:
        current = current / component
        if not current.exists():
            continue
        if current.is_symlink() or (hasattr(os.path, "isjunction") and os.path.isjunction(current)):
            raise WorkerError("OUTPUT_ROOT_INVALID", "La raíz de salida no puede atravesar enlaces o junctions.")


def _output_root() -> Path:
    configured = os.environ.get("LARM_OLIVE_OUTPUT_ROOT")
    if not configured:
        raise WorkerError("OUTPUT_ROOT_REQUIRED", "La raíz de salida gestionada no está configurada.")
    _no_reparse_path(Path(os.path.abspath(configured)))
    try:
        path = Path(configured).resolve(strict=True)
    except OSError as exc:
        raise WorkerError("OUTPUT_ROOT_UNAVAILABLE", "La raíz de salida gestionada no existe o no es accesible.") from exc
    if not path.is_dir():
        raise WorkerError("OUTPUT_ROOT_UNAVAILABLE", "La raíz de salida gestionada no es una carpeta.")
    _no_reparse_path(path)
    return path


def _safe_tree(root: Path) -> tuple[list[dict[str, Any]], int]:
    artifacts: list[dict[str, Any]] = []
    total = 0
    for path in sorted(root.rglob("*")):
        if path.is_symlink() or (hasattr(os.path, "isjunction") and os.path.isjunction(path)):
            raise WorkerError("OUTPUT_INVALID", "Olive produjo un enlace o junction no permitido.")
        if path.is_dir():
            continue
        if not path.is_file():
            raise WorkerError("OUTPUT_INVALID", "Olive produjo un tipo de archivo no permitido.")
        if len(artifacts) >= MAX_OUTPUT_FILES:
            raise WorkerError("OUTPUT_LIMIT_EXCEEDED", "Olive superó el límite de archivos de salida.")
        digest, size = _hash_file(path)
        total += size
        if total > MAX_OUTPUT_BYTES:
            raise WorkerError("OUTPUT_LIMIT_EXCEEDED", "Olive superó el límite de bytes de salida.")
        artifacts.append({"relative_path": path.relative_to(root).as_posix(), "sha256": digest, "size_bytes": str(size)})
    if not artifacts:
        raise WorkerError("OUTPUT_EMPTY", "Olive no produjo artefactos verificables.")
    return artifacts, total


def _run_olive(fixture_path: Path, output_path: Path, provider: str, precision: str) -> None:
    try:
        timeout = float(os.environ.get("LARM_OLIVE_TIMEOUT_SECONDS", str(MAX_TIMEOUT_SECONDS)))
    except ValueError as exc:
        raise WorkerError("INVALID_TIMEOUT", "El timeout configurado no es válido.") from exc
    if not 0.01 <= timeout <= MAX_TIMEOUT_SECONDS:
        raise WorkerError("INVALID_TIMEOUT", "El timeout debe estar entre 0.01 y 1800 segundos.")
    command = [
        sys.executable,
        "-I",
        str(ROOT / "olive_runner.py"),
        str(fixture_path),
        str(output_path),
        provider,
        precision,
    ]
    inherited = {key: os.environ[key] for key in ("PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR") if key in os.environ}
    inherited.update({"PYTHONNOUSERSITE": "1", "PYTHONUTF8": "1", "PYTHONIOENCODING": "utf-8"})
    creationflags = getattr(subprocess, "CREATE_NEW_PROCESS_GROUP", 0)
    try:
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   env=inherited, creationflags=creationflags)
    except OSError as exc:
        raise WorkerError("TOOL_START_FAILED", "No se pudo iniciar el proceso aislado de Olive.") from exc
    try:
        _, _ = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        process.terminate()
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
        raise WorkerError("TOOL_TIMEOUT", "Olive excedió el timeout permitido.") from exc
    except KeyboardInterrupt as exc:
        process.terminate()
        try:
            process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate()
        raise WorkerError("TOOL_CANCELLED", "La ejecución de Olive se canceló.") from exc
    if process.returncode != 0:
        raise WorkerError("TOOL_FAILED", "Olive terminó con error; la salida parcial no se publicará.")


def _execute(params: dict[str, Any], plan: dict[str, Any], env: dict[str, Any]) -> dict[str, Any]:
    if params["plan_hash"] != plan["plan_hash"]:
        raise WorkerError("PLAN_MISMATCH", "El plan cambió desde su preparación.")
    if not plan["eligible"]:
        raise WorkerError("ENVIRONMENT_UNAVAILABLE", "El entorno o provider no cumple el plan.")
    fixture, fixture_path = _fixture_path(params["fixture_id"])
    root = _output_root()
    candidates = root / "candidates"
    candidates.mkdir(exist_ok=True)
    _no_reparse_path(candidates)
    final_path = candidates / plan["plan_hash"]
    if final_path.exists():
        raise WorkerError("CANDIDATE_EXISTS", "Ya existe una salida para este plan.")
    stage = Path(tempfile.mkdtemp(prefix=f"{plan['plan_hash']}.", suffix=".partial", dir=candidates))
    published = False
    try:
        artifact_root = stage / "artifacts"
        artifact_root.mkdir()
        before_hash, before_size = _hash_file(fixture_path)
        _run_olive(fixture_path, artifact_root, params["provider_target"], params["precision"])
        after_hash, after_size = _hash_file(fixture_path)
        if (before_hash, before_size) != (fixture["sha256"], fixture["size_bytes"]) or (after_hash, after_size) != (before_hash, before_size):
            raise WorkerError("INPUT_CHANGED", "La fixture fuente cambió durante la transformación.")
        outputs, total = _safe_tree(artifact_root)
        provenance = {
            "schema_version": "1.0",
            "fixture_id": fixture["fixture_id"],
            "input_sha256": fixture["sha256"],
            "input_size_bytes": str(fixture["size_bytes"]),
            "tool": "Microsoft Olive",
            "tool_version": env["olive_ai_version"],
            "dependencies": _versions(env),
            "provider_target": params["provider_target"],
            "parameters": {
                "precision": params["precision"],
                "provider_target": params["provider_target"],
                "trust_remote_code": False,
            },
            "plan_hash": plan["plan_hash"],
            "output_artifacts": outputs,
            "output_size_bytes": str(total),
            "created_at": datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z"),
        }
        encoded = json.dumps(provenance, ensure_ascii=False, sort_keys=True, indent=2, allow_nan=False).encode("utf-8") + b"\n"
        provenance_path = stage / "provenance.json"
        with provenance_path.open("xb") as stream:
            stream.write(encoded)
            stream.flush()
            os.fsync(stream.fileno())
        bundle_hash = _canonical_hash({"artifacts": outputs})
        try:
            os.rename(stage, final_path)
        except OSError as exc:
            raise WorkerError("OUTPUT_PUBLISH_FAILED", "No se pudo publicar la carpeta candidata de forma atómica.") from exc
        published = True
        return {
            "candidate_id": plan["plan_hash"],
            "candidate_relative_path": f"candidates/{plan['plan_hash']}",
            "bundle_sha256": bundle_hash,
            "output_size_bytes": str(total),
            "output_artifacts": outputs,
            "provenance_sha256": hashlib.sha256(encoded).hexdigest(),
        }
    finally:
        if not published and stage.exists():
            shutil.rmtree(stage, ignore_errors=True)


def handle(request: Any) -> dict[str, Any]:
    operation, params = _validate_request(request)
    env = _environment()
    if operation == "probe":
        result = _probe(env)
    elif operation == "version":
        result = _versions(env)
    elif operation == "capabilities":
        result = _capabilities(env)
    elif operation == "plan":
        result = _plan(params, env)
    else:
        plan = _plan(params, env)
        result = _execute(params, plan, env)
    return {"protocol_version": PROTOCOL_VERSION, "ok": True, "operation": operation, "result": result}


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8", errors="strict")
    try:
        raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
        if len(raw) > MAX_REQUEST_BYTES:
            raise WorkerError("REQUEST_TOO_LARGE", "La solicitud supera el límite de 1 MiB.")
        request = json.loads(raw.decode("utf-8"), object_pairs_hook=_no_duplicate_pairs)
        response = handle(request)
        status = 0
    except WorkerError as exc:
        response = {
            "protocol_version": PROTOCOL_VERSION,
            "ok": False,
            "error": {"code": exc.code, "message": exc.message},
        }
        status = 2
    except (UnicodeDecodeError, json.JSONDecodeError):
        response = {"protocol_version": PROTOCOL_VERSION, "ok": False,
                    "error": {"code": "INVALID_REQUEST", "message": "La entrada debe ser un único objeto JSON UTF-8 válido."}}
        status = 2
    except Exception:
        response = {"protocol_version": PROTOCOL_VERSION, "ok": False,
                    "error": {"code": "INTERNAL_ERROR", "message": "La operación falló de forma segura."}}
        status = 3
    sys.stdout.write(json.dumps(response, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n")
    return status


if __name__ == "__main__":
    raise SystemExit(main())
