#!/usr/bin/env python3
"""Private, fail-closed worker for managed PyTorch/Transformers runtimes (P071)."""
from __future__ import annotations

import hashlib
import importlib
import importlib.metadata
import json
import os
import re
import sys
import stat
import uuid
from pathlib import Path
from typing import Any, Callable

PROTOCOL_VERSION = "1"
MAX_REQUEST_BYTES = 1_048_576
MAX_CONFIG_BYTES = 1_048_576
MAX_PROMPT_CHARS = 16_000
MAX_NEW_TOKENS = 256
STRATEGIES = {"PIP", "UV", "CONDA", "MAMBA", "PYTHON_VENV"}
BACKENDS = {"CPU", "CUDA", "XPU"}
MODEL_ID_PART = re.compile(r"^[A-Za-z0-9._-]{1,100}$")
PYTHON_VERSION = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


class WorkerError(Exception):
    def __init__(self, code: str, reason: str):
        super().__init__(reason)
        self.code = code
        self.reason = reason


def _object(value: Any, required: set[str], optional: set[str] = frozenset()) -> dict[str, Any]:
    if not isinstance(value, dict) or not required.issubset(value) or set(value) - required - optional:
        raise WorkerError("INVALID_REQUEST", "El objeto no coincide con el contrato cerrado.")
    return value


def _duplicate_rejecting_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result


def _uuid7(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError):
        return False
    return str(parsed) == value and parsed.version == 7


def _managed_interpreter(
    *,
    runtime_root: str | None = None,
    prefix: str | None = None,
    base_prefix: str | None = None,
    executable: str | None = None,
) -> bool:
    runtime_root = runtime_root if runtime_root is not None else os.environ.get("LARM_PYTHON_ROOT")
    prefix = prefix if prefix is not None else sys.prefix
    base_prefix = base_prefix if base_prefix is not None else sys.base_prefix
    executable = executable if executable is not None else sys.executable
    if (
        not runtime_root
        or not Path(runtime_root).is_absolute()
        or not Path(prefix).is_absolute()
        or not Path(executable).is_absolute()
    ):
        return False
    try:
        root = Path(runtime_root).resolve(strict=True)
        active = Path(prefix).resolve(strict=True)
        base = Path(base_prefix).resolve(strict=True)
        interpreter = Path(executable).absolute()
        if _is_reparse(interpreter):
            return False
        interpreter.relative_to(root)
        return root.is_dir() and interpreter.is_file() and active == root and active != base
    except (OSError, ValueError):
        return False


def _installed_version(distribution: str) -> str | None:
    try:
        return importlib.metadata.version(distribution)
    except importlib.metadata.PackageNotFoundError:
        return None


def _probe(*, managed: bool | None = None, importer: Callable[[str], Any] = importlib.import_module) -> dict[str, Any]:
    is_managed = _managed_interpreter() if managed is None else managed
    result: dict[str, Any] = {
        "managed_python": is_managed,
        "python_version": sys.version.split()[0],
        "python_prefix": str(Path(sys.prefix).resolve()),
        "torch_version": None,
        "transformers_version": None,
        "available_backends": [],
    }
    if not is_managed:
        return result
    result["torch_version"] = _installed_version("torch")
    result["transformers_version"] = _installed_version("transformers")
    if result["torch_version"] is None:
        return result
    try:
        torch = importer("torch")
        backends = ["CPU"]
        if getattr(getattr(torch, "cuda", None), "is_available", lambda: False)():
            backends.append("CUDA")
        xpu = getattr(torch, "xpu", None)
        if xpu is not None and getattr(xpu, "is_available", lambda: False)():
            backends.append("XPU")
        result["available_backends"] = backends
    except Exception:
        # A broken binary import is observable as unavailable, never as support.
        result["available_backends"] = []
    return result


def _environment_plan(payload: Any) -> dict[str, Any]:
    value = _object(
        payload,
        {
            "runtime_id",
            "runtime_version_id",
            "backend_id",
            "python_version",
            "dependencies_lock_sha256",
            "strategy",
            "announced_strategies",
        },
    )
    if not all(_uuid7(value[key]) for key in ("runtime_id", "runtime_version_id", "backend_id")):
        raise WorkerError("INVALID_REQUEST", "Las identidades deben ser UUIDv7 canónicos.")
    if (
        not isinstance(value["python_version"], str)
        or len(value["python_version"]) > 17
        or not PYTHON_VERSION.fullmatch(value["python_version"])
    ):
        raise WorkerError("INVALID_REQUEST", "La versión exacta de Python no es válida.")
    if any(int(part) > 65_535 for part in value["python_version"].split(".")):
        raise WorkerError("INVALID_REQUEST", "Los componentes de la versión de Python superan el límite.")
    if not isinstance(value["dependencies_lock_sha256"], str) or not SHA256.fullmatch(value["dependencies_lock_sha256"]):
        raise WorkerError("INVALID_REQUEST", "El hash del lock debe ser SHA256 hexadecimal minúsculo.")
    strategies = value["announced_strategies"]
    if (
        not isinstance(strategies, list)
        or not all(isinstance(item, str) and item and not any(ord(ch) < 32 for ch in item) for item in strategies)
        or len(set(strategies)) != len(strategies)
    ):
        raise WorkerError("INVALID_REQUEST", "El perfil no anuncia una lista válida de estrategias.")
    strategy = value["strategy"]
    if not isinstance(strategy, str) or strategy not in STRATEGIES:
        raise WorkerError("UNSUPPORTED_CAPABILITY", "La estrategia no corresponde a un entorno Python aislado.")
    if strategy not in strategies:
        raise WorkerError("UNSUPPORTED_CAPABILITY", "El adaptador no anuncia la estrategia solicitada.")
    identity = {
        "runtime_id": value["runtime_id"],
        "runtime_version_id": value["runtime_version_id"],
        "backend_id": value["backend_id"],
        "python_version": value["python_version"],
        "dependencies_lock_sha256": value["dependencies_lock_sha256"],
    }
    encoded = json.dumps(identity, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    digest = hashlib.sha256(encoded).hexdigest()
    return {
        "environment_id": digest,
        "relative_path": "/".join(
            ("python-envs", identity["runtime_id"], identity["runtime_version_id"], identity["backend_id"], digest[:2], digest)
        ),
        "python_version": identity["python_version"],
        "dependencies_lock_sha256": identity["dependencies_lock_sha256"],
        "strategy": strategy,
        "isolated": True,
        "created": False,
    }


def _safe_model_directory(model_root: str | None, model_id: Any) -> Path:
    if not isinstance(model_id, str) or not model_id or len(model_id) > 202 or "\\" in model_id:
        raise WorkerError("INVALID_REQUEST", "El identificador local de modelo no es válido.")
    parts = model_id.split("/")
    if any(part in {"", ".", ".."} or not MODEL_ID_PART.fullmatch(part) for part in parts):
        raise WorkerError("INVALID_REQUEST", "El identificador local de modelo no es válido.")
    if not model_root or not Path(model_root).is_absolute():
        raise WorkerError("MODEL_ROOT_UNAVAILABLE", "No hay una raíz local de modelos configurada.")
    try:
        root = Path(model_root).resolve(strict=True)
        if _is_reparse(Path(model_root)):
            raise WorkerError("MODEL_PATH_UNSAFE", "La raíz de modelos es una ruta reparseada o simbólica.")
        candidate = root.joinpath(*parts)
        cursor = root
        for part in parts:
            cursor = cursor / part
            if _is_reparse(cursor):
                raise WorkerError("MODEL_PATH_UNSAFE", "El modelo contiene una ruta reparseada o simbólica.")
        resolved = candidate.resolve(strict=True)
        resolved.relative_to(root)
        if not resolved.is_dir():
            raise WorkerError("MODEL_NOT_FOUND", "El directorio del modelo local no existe.")
        return resolved
    except WorkerError:
        raise
    except (OSError, ValueError):
        raise WorkerError("MODEL_NOT_FOUND", "El directorio del modelo local no existe dentro de la raíz autorizada.") from None


def _is_reparse(path: Path) -> bool:
    try:
        info = path.lstat()
    except OSError:
        return False
    if stat.S_ISLNK(info.st_mode):
        return True
    attributes = getattr(info, "st_file_attributes", 0)
    reparse_flag = getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    return bool(attributes & reparse_flag)


def _read_json_file(directory: Path, name: str) -> dict[str, Any] | None:
    path = directory / name
    if not path.exists():
        return None
    if _is_reparse(path) or not path.is_file():
        raise WorkerError("MODEL_PATH_UNSAFE", "Un archivo de configuración del modelo no es regular.")
    try:
        raw = path.read_bytes()
        if len(raw) > MAX_CONFIG_BYTES:
            raise WorkerError("MODEL_CONFIG_TOO_LARGE", "La configuración supera el límite de lectura.")
        value = json.loads(raw.decode("utf-8", errors="strict"), object_pairs_hook=_duplicate_rejecting_object)
    except WorkerError:
        raise
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError):
        raise WorkerError("MODEL_CONFIG_INVALID", "La configuración local del modelo no es JSON válido.") from None
    if not isinstance(value, dict):
        raise WorkerError("MODEL_CONFIG_INVALID", "La configuración local del modelo debe ser un objeto JSON.")
    return value


def _inspect_model(model_root: str | None, model_id: Any) -> dict[str, Any]:
    directory = _safe_model_directory(model_root, model_id)
    config = _read_json_file(directory, "config.json")
    if config is None:
        raise WorkerError("MODEL_CONFIG_MISSING", "El modelo local no contiene config.json.")
    tokenizer_config = _read_json_file(directory, "tokenizer_config.json")
    remote_code_files = [
        name
        for name, value in (("config.json", config), ("tokenizer_config.json", tokenizer_config))
        if value is not None and value.get("auto_map")
    ]
    architectures = config.get("architectures", [])
    if not isinstance(architectures, list) or not all(isinstance(item, str) for item in architectures):
        architectures = []
    safe_weights = any(path.is_file() and not _is_reparse(path) for path in directory.glob("*.safetensors"))
    return {
        "model_id": model_id,
        "architectures": architectures,
        "remote_code_required": bool(remote_code_files),
        "remote_code_files": remote_code_files,
        "safetensors_present": safe_weights,
        "eligible_for_text_generation": bool(architectures) and not remote_code_files and safe_weights,
        "blockers": (
            ["REMOTE_CODE_REQUIRED"]
            if remote_code_files
            else (["SAFE_TENSORS_REQUIRED"] if not safe_weights else ([] if architectures else ["MODEL_ARCHITECTURE_UNKNOWN"]))
        ),
    }


def _generate_text(
    model_root: str | None,
    payload: Any,
    *,
    managed: bool,
    importer: Callable[[str], Any] = importlib.import_module,
) -> dict[str, Any]:
    value = _object(payload, {"model_id", "backend", "prompt", "max_new_tokens"})
    backend = value["backend"]
    prompt = value["prompt"]
    max_new_tokens = value["max_new_tokens"]
    if not isinstance(backend, str) or backend not in BACKENDS:
        raise WorkerError("INVALID_REQUEST", "El backend debe ser explícito: CPU, CUDA o XPU.")
    if not isinstance(prompt, str) or not prompt.strip() or len(prompt) > MAX_PROMPT_CHARS:
        raise WorkerError("INVALID_REQUEST", "El prompt está vacío o supera el límite local.")
    if isinstance(max_new_tokens, bool) or not isinstance(max_new_tokens, int) or not 1 <= max_new_tokens <= MAX_NEW_TOKENS:
        raise WorkerError("INVALID_REQUEST", "max_new_tokens debe estar entre 1 y 256.")
    if not managed:
        raise WorkerError("MANAGED_PYTHON_REQUIRED", "El runtime sólo se ejecuta desde su entorno gestionado.")
    info = _inspect_model(model_root, value["model_id"])
    if info["remote_code_required"]:
        raise WorkerError("REMOTE_CODE_REQUIRED", "El modelo requiere código externo; no se ejecutará sin aprobación explícita.")
    if not info["eligible_for_text_generation"]:
        raise WorkerError("MODEL_NOT_QUALIFIED", "El modelo no acredita arquitectura de texto y pesos safetensors locales.")
    try:
        torch = importer("torch")
        transformers = importer("transformers")
    except Exception:
        raise WorkerError("TRANSFORMERS_UNAVAILABLE", "PyTorch y Transformers no están disponibles en este entorno.") from None
    device = backend.lower()
    if backend == "CUDA" and not getattr(getattr(torch, "cuda", None), "is_available", lambda: False)():
        raise WorkerError("BACKEND_UNAVAILABLE", "CUDA no está disponible en el entorno gestionado.")
    if backend == "XPU" and not getattr(getattr(torch, "xpu", None), "is_available", lambda: False)():
        raise WorkerError("BACKEND_UNAVAILABLE", "XPU no está disponible en el entorno gestionado.")
    directory = _safe_model_directory(model_root, value["model_id"])
    try:
        tokenizer = transformers.AutoTokenizer.from_pretrained(
            str(directory), local_files_only=True, trust_remote_code=False
        )
        model = transformers.AutoModelForCausalLM.from_pretrained(
            str(directory),
            local_files_only=True,
            trust_remote_code=False,
            use_safetensors=True,
        )
        model = model.to(device)
        model.eval()
        inputs = tokenizer(prompt, return_tensors="pt")
        input_ids = inputs["input_ids"].to(device)
        attention_mask = inputs.get("attention_mask")
        if attention_mask is not None:
            attention_mask = attention_mask.to(device)
        inference_mode = getattr(torch, "inference_mode", None)
        if inference_mode is None:
            raise WorkerError("TRANSFORMERS_UNAVAILABLE", "El runtime Torch no ofrece inference_mode.")
        with inference_mode():
            output = model.generate(
                input_ids=input_ids,
                attention_mask=attention_mask,
                max_new_tokens=max_new_tokens,
                do_sample=False,
            )
        generated = output[0][input_ids.shape[-1] :]
        text = tokenizer.decode(generated, skip_special_tokens=True)
        generated_tokens = max(0, int(output[0].shape[-1]) - int(input_ids.shape[-1]))
    except WorkerError:
        raise
    except Exception:
        raise WorkerError("MODEL_LOAD_OR_GENERATION_FAILED", "No se pudo cargar o ejecutar el modelo local.") from None
    return {
        "model_id": value["model_id"],
        "backend": backend,
        "text": text,
        "prompt_tokens": int(input_ids.shape[-1]),
        "completion_tokens": generated_tokens,
        "finish_reason": "length" if generated_tokens >= max_new_tokens else "stop",
        "remote_code_used": False,
    }


def handle(request: Any, *, managed: bool | None = None, importer: Callable[[str], Any] = importlib.import_module) -> dict[str, Any]:
    value = _object(request, {"protocol_version", "operation"}, {"payload"})
    if value["protocol_version"] != PROTOCOL_VERSION:
        raise WorkerError("PROTOCOL_VERSION_UNSUPPORTED", "La versión de protocolo no está soportada.")
    operation = value["operation"]
    if not isinstance(operation, str):
        raise WorkerError("INVALID_REQUEST", "operation debe ser texto.")
    if operation in {"probe", "version", "capabilities"} and "payload" in value:
        raise WorkerError("INVALID_REQUEST", "Esta operación no acepta payload.")
    if operation not in {"probe", "version", "capabilities", "environment_plan", "inspect_model", "generate_text"}:
        raise WorkerError("UNSUPPORTED_OPERATION", "La operación no está habilitada.")
    is_managed = _managed_interpreter() if managed is None else managed
    if operation == "probe":
        result = _probe(managed=is_managed, importer=importer)
    elif operation == "version":
        result = {
            "python_version": sys.version.split()[0],
            "torch_version": _installed_version("torch") if is_managed else None,
            "transformers_version": _installed_version("transformers") if is_managed else None,
        }
    elif operation == "capabilities":
        probe = _probe(managed=is_managed, importer=importer)
        result = {
            "operations": ["probe", "version", "capabilities", "environment_plan", "inspect_model", "generate_text"],
            "capabilities": ["text.generate"] if probe["transformers_version"] else [],
            "backends": probe["available_backends"],
            "managed_python": probe["managed_python"],
            "openai_sidecar": False,
        }
    elif operation == "environment_plan":
        if "payload" not in value:
            raise WorkerError("INVALID_REQUEST", "Falta payload.")
        result = _environment_plan(value["payload"])
    elif operation == "inspect_model":
        payload = _object(value.get("payload"), {"model_id"})
        result = _inspect_model(os.environ.get("LARM_TRANSFORMERS_MODEL_ROOT"), payload["model_id"])
    else:
        if "payload" not in value:
            raise WorkerError("INVALID_REQUEST", "Falta payload.")
        result = _generate_text(
            os.environ.get("LARM_TRANSFORMERS_MODEL_ROOT"), value["payload"], managed=is_managed, importer=importer
        )
    return {"protocol_version": PROTOCOL_VERSION, "ok": True, "result": result}


def main() -> int:
    raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    try:
        if len(raw) > MAX_REQUEST_BYTES:
            raise WorkerError("REQUEST_TOO_LARGE", "La solicitud supera el límite.")
        request = json.loads(raw.decode("utf-8", errors="strict"), object_pairs_hook=_duplicate_rejecting_object)
        response = handle(request)
    except WorkerError as error:
        response = {
            "protocol_version": PROTOCOL_VERSION,
            "ok": False,
            "error": {"code": error.code, "reason": error.reason},
        }
    except (UnicodeError, ValueError, json.JSONDecodeError):
        response = {
            "protocol_version": PROTOCOL_VERSION,
            "ok": False,
            "error": {"code": "INVALID_JSON", "reason": "La solicitud no es JSON UTF-8 válido."},
        }
    sys.stdout.write(json.dumps(response, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n")
    sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
