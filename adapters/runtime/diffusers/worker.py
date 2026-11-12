#!/usr/bin/env python3
"""Private, local-only Diffusers worker prepared for P072."""
from __future__ import annotations

import hashlib
import importlib
import importlib.metadata
import json
import os
import re
import shutil
import stat
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Callable

PROTOCOL_VERSION = "1"
MAX_REQUEST_BYTES = 1_048_576
MAX_MODEL_INDEX_BYTES = MAX_REQUEST_BYTES
MAX_CONFIG_BYTES = 1_048_576
MAX_OUTPUT_BYTES = 9_223_372_036_854_775_807
SHA256 = re.compile(r"^[0-9a-f]{64}$")
UUID7 = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")

SCHEDULERS = {
    "DDIMScheduler",
    "DEISMultistepScheduler",
    "DPMSolverMultistepScheduler",
    "EulerDiscreteScheduler",
    "EulerAncestralDiscreteScheduler",
    "HeunDiscreteScheduler",
    "LMSDiscreteScheduler",
    "PNDMScheduler",
}

PIPELINES: dict[str, dict[str, tuple[str, str, str]]] = {
    "StableDiffusionPipeline": {
        "unet": ("diffusers", "UNet2DConditionModel", "weights"),
        "vae": ("diffusers", "AutoencoderKL", "vae"),
        "text_encoder": ("transformers", "CLIPTextModel", "text_encoder"),
        "tokenizer": ("transformers", "CLIPTokenizer", "tokenizer"),
        "scheduler": ("diffusers", "*scheduler*", "config"),
    },
    "StableDiffusionXLPipeline": {
        "unet": ("diffusers", "UNet2DConditionModel", "weights"),
        "vae": ("diffusers", "AutoencoderKL", "vae"),
        "text_encoder": ("transformers", "CLIPTextModel", "text_encoder"),
        "text_encoder_2": ("transformers", "CLIPTextModelWithProjection", "text_encoder"),
        "tokenizer": ("transformers", "CLIPTokenizer", "tokenizer"),
        "tokenizer_2": ("transformers", "CLIPTokenizer", "tokenizer"),
        "scheduler": ("diffusers", "*scheduler*", "config"),
    },
    "FluxPipeline": {
        "transformer": ("diffusers", "FluxTransformer2DModel", "weights"),
        "vae": ("diffusers", "AutoencoderKL", "vae"),
        "text_encoder": ("transformers", "CLIPTextModel", "text_encoder"),
        "text_encoder_2": ("transformers", "T5EncoderModel", "text_encoder"),
        "tokenizer": ("transformers", "CLIPTokenizer", "tokenizer"),
        "tokenizer_2": ("transformers", "T5TokenizerFast", "tokenizer"),
        "scheduler": ("diffusers", "*scheduler*", "config"),
    },
}

OPTIONAL_COMPONENTS: dict[str, tuple[str, str, str]] = {
    "safety_checker": ("diffusers", "StableDiffusionSafetyChecker", "weights"),
    "feature_extractor": ("transformers", "CLIPImageProcessor", "processor"),
    "image_encoder": ("transformers", "CLIPVisionModelWithProjection", "weights"),
}
MODEL_METADATA_KEYS = {"_class_name", "_diffusers_version", "_name_or_path"}
COMPONENT_CONFIGS = {
    "weights": "config.json",
    "vae": "config.json",
    "text_encoder": "config.json",
    "tokenizer": None,
    "config": "scheduler_config.json",
    "processor": "preprocessor_config.json",
}
WEIGHT_COMPONENTS = {"weights", "vae", "text_encoder"}
TOKENIZER_FILES = {"tokenizer.json", "tokenizer_config.json", "vocab.json", "spiece.model"}
DEVICE_BY_BACKEND = {"CPU": "cpu", "CUDA": "cuda", "XPU": "xpu"}


class WorkerError(Exception):
    def __init__(self, code: str, reason: str):
        super().__init__(reason)
        self.code = code
        self.reason = reason


def _object(value: Any, required: set[str], optional: set[str] | frozenset[str] = frozenset()) -> dict[str, Any]:
    if not isinstance(value, dict) or not required.issubset(value) or set(value) - required - optional:
        raise WorkerError("INVALID_REQUEST", "El objeto no coincide con el contrato cerrado.")
    return value


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _is_reparse(path: Path) -> bool:
    try:
        info = path.lstat()
    except OSError:
        return False
    return path.is_symlink() or bool(
        getattr(info, "st_file_attributes", 0) & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)
    )


def _has_reparse_ancestor(path: Path) -> bool:
    current = Path(os.path.abspath(path))
    while True:
        if _is_reparse(current):
            return True
        if current.parent == current:
            return False
        current = current.parent


def _validate_uuid7(value: Any, field: str) -> str:
    if not isinstance(value, str) or not UUID7.fullmatch(value):
        raise WorkerError("INVALID_REQUEST", f"{field} debe ser UUIDv7 canónico.")
    try:
        parsed = uuid.UUID(value)
    except (ValueError, AttributeError) as error:
        raise WorkerError("INVALID_REQUEST", f"{field} debe ser UUIDv7 canónico.") from error
    if str(parsed) != value or parsed.version != 7:
        raise WorkerError("INVALID_REQUEST", f"{field} debe ser UUIDv7 canónico.")
    return value


def _managed_interpreter(
    *, runtime_root: str | None = None, prefix: str | None = None, base_prefix: str | None = None,
    executable: str | None = None,
) -> bool:
    runtime_root = runtime_root if runtime_root is not None else os.environ.get("LARM_PYTHON_ROOT")
    prefix = prefix if prefix is not None else sys.prefix
    base_prefix = base_prefix if base_prefix is not None else sys.base_prefix
    executable = executable if executable is not None else sys.executable
    if not runtime_root or not Path(runtime_root).is_absolute() or not Path(prefix).is_absolute() or not Path(executable).is_absolute():
        return False
    try:
        if any(_has_reparse_ancestor(Path(item)) for item in (runtime_root, prefix, executable)):
            return False
        root = Path(runtime_root).resolve(strict=True)
        active = Path(prefix).resolve(strict=True)
        base = Path(base_prefix).resolve(strict=True)
        interpreter = Path(os.path.abspath(executable))
        interpreter.resolve(strict=True).relative_to(root)
        return root.is_dir() and interpreter.is_file() and active == root and active != base
    except (OSError, ValueError):
        return False


def environment_plan(payload: Any) -> dict[str, Any]:
    value = _object(payload, {
        "runtime_id", "runtime_version_id", "backend_id", "python_version", "dependencies_lock_sha256", "strategy",
        "announced_strategies",
    })
    for field in ("runtime_id", "runtime_version_id", "backend_id"):
        _validate_uuid7(value[field], field)
    version = value["python_version"]
    if not isinstance(version, str) or len(version) > 17 or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise WorkerError("INVALID_REQUEST", "python_version debe ser una versión exacta de tres componentes.")
    if any(int(part) > 65_535 for part in version.split(".")):
        raise WorkerError("INVALID_REQUEST", "python_version supera el límite permitido.")
    lock_hash = value["dependencies_lock_sha256"]
    if not isinstance(lock_hash, str) or not SHA256.fullmatch(lock_hash):
        raise WorkerError("INVALID_REQUEST", "dependencies_lock_sha256 debe ser SHA256 hexadecimal minúsculo.")
    announced = value["announced_strategies"]
    if (
        not isinstance(announced, list)
        or not all(isinstance(item, str) and item and not any(ord(char) < 32 for char in item) for item in announced)
        or len(set(announced)) != len(announced)
    ):
        raise WorkerError("INVALID_REQUEST", "announced_strategies debe ser una lista de cadenas.")
    strategy = value["strategy"]
    if not isinstance(strategy, str) or strategy not in {"PIP", "UV", "CONDA", "MAMBA", "PYTHON_VENV"} or strategy not in announced:
        raise WorkerError("UNSUPPORTED_CAPABILITY", "La estrategia no está anunciada por el perfil de runtime.")
    identity = {key: value[key] for key in (
        "runtime_id", "runtime_version_id", "backend_id", "python_version", "dependencies_lock_sha256",
    )}
    encoded = json.dumps(identity, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    digest = hashlib.sha256(encoded).hexdigest()
    return {
        "environment_id": digest,
        "relative_path": "/".join(("python-envs", identity["runtime_id"], identity["runtime_version_id"], identity["backend_id"], digest[:2], digest)),
        "python_version": version,
        "dependencies_lock_sha256": lock_hash,
        "strategy": strategy,
        "isolated": True,
        "created": False,
    }


def _trusted_child(root: Path, relative: str, *, directory: bool | None = None) -> Path:
    if not relative or Path(relative).is_absolute() or "\\" in relative or any(part in {"", ".", ".."} for part in relative.split("/")):
        raise WorkerError("MODEL_INVALID", "El bundle contiene una ruta relativa no válida.")
    root = root.resolve(strict=True)
    if _is_reparse(root):
        raise WorkerError("MODEL_INVALID", "El directorio del bundle no puede ser un enlace o junction.")
    current = root
    for part in relative.split("/"):
        current = current / part
        if _is_reparse(current):
            raise WorkerError("MODEL_INVALID", "El bundle contiene un enlace o junction.")
    try:
        resolved = current.resolve(strict=True)
        resolved.relative_to(root)
    except (OSError, ValueError) as error:
        raise WorkerError("MODEL_INVALID", "La ruta del bundle no existe bajo el modelo local autorizado.") from error
    if directory is True and not resolved.is_dir():
        raise WorkerError("MODEL_INVALID", "Falta un directorio de componente del bundle.")
    if directory is False and not resolved.is_file():
        raise WorkerError("MODEL_INVALID", "Falta un archivo de componente del bundle.")
    return resolved


def _read_json(path: Path, max_bytes: int) -> dict[str, Any]:
    try:
        raw = path.read_bytes()
        if len(raw) > max_bytes:
            raise WorkerError("MODEL_INVALID", "Un archivo JSON del bundle supera el límite de lectura.")
        value = json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_object)
    except WorkerError:
        raise
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise WorkerError("MODEL_INVALID", "Un archivo JSON del bundle no es válido.") from error
    if not isinstance(value, dict):
        raise WorkerError("MODEL_INVALID", "La configuración del bundle debe ser un objeto JSON.")
    if "auto_map" in value:
        raise WorkerError("REMOTE_CODE_REQUIRED", "El bundle solicita código personalizado mediante auto_map.")
    return value


def _component_spec(pipeline: str, component: str, value: Any) -> tuple[str, str, str]:
    required = PIPELINES[pipeline]
    spec = required.get(component) or OPTIONAL_COMPONENTS.get(component)
    if spec is None:
        raise WorkerError("REMOTE_CODE_REQUIRED", "El bundle declara un componente personalizado o no cualificado.")
    if value is None and component in OPTIONAL_COMPONENTS:
        return spec
    if not isinstance(value, list) or len(value) != 2 or not all(isinstance(item, str) and item for item in value):
        raise WorkerError("REMOTE_CODE_REQUIRED", "El bundle declara una implementación de componente no estándar.")
    library, class_name = value
    if library != spec[0] or (spec[1] == "*scheduler*" and (library != "diffusers" or class_name not in SCHEDULERS)):
        raise WorkerError("REMOTE_CODE_REQUIRED", "El bundle solicita una biblioteca o pipeline personalizado.")
    if spec[1] != "*scheduler*" and class_name != spec[1]:
        raise WorkerError("REMOTE_CODE_REQUIRED", "El bundle solicita una clase de componente no cualificada.")
    return spec


def inspect_bundle(model_root: str, model_path: str) -> dict[str, Any]:
    try:
        raw_root = Path(model_root)
        raw_model = Path(model_path)
        if not raw_root.is_absolute() or not raw_model.is_absolute():
            raise ValueError("model paths must be absolute")
        raw_root = Path(os.path.abspath(raw_root))
        raw_model = Path(os.path.abspath(raw_model))
        if _has_reparse_ancestor(raw_root):
            raise ValueError("model root is a reparse point")
        root = raw_root.resolve(strict=True)
        model = raw_model
        if _is_reparse(root) or _is_reparse(model):
            raise ValueError("reparse point")
        model = model.resolve(strict=True)
        model.relative_to(root)
    except (OSError, ValueError) as error:
        raise WorkerError("MODEL_UNAVAILABLE", "El modelo debe estar dentro de la raíz local autorizada.") from error
    if not model.is_dir():
        raise WorkerError("MODEL_UNAVAILABLE", "El modelo local no es un directorio.")
    index_path = _trusted_child(model, "model_index.json", directory=False)
    index = _read_json(index_path, MAX_MODEL_INDEX_BYTES)
    pipeline = index.get("_class_name")
    if not isinstance(pipeline, str) or pipeline not in PIPELINES:
        raise WorkerError("REMOTE_CODE_REQUIRED", "La clase de pipeline no pertenece a la lista integrada cualificada.")
    expected = PIPELINES[pipeline]
    components: list[dict[str, Any]] = []
    mapped: list[dict[str, str]] = []
    for name, value in index.items():
        if name in MODEL_METADATA_KEYS:
            continue
        spec = _component_spec(pipeline, name, value)
        if value is None and name in OPTIONAL_COMPONENTS:
            continue
        is_required = name in expected
        component_dir = _trusted_child(model, name, directory=True)
        role = spec[2]
        facts = {"diffusers_component": name}
        mapped.append({"component": name, "role": role})
        if role in WEIGHT_COMPONENTS:
            config_path = _trusted_child(model, f"{name}/config.json", directory=False)
            _read_json(config_path, MAX_CONFIG_BYTES)
            files = sorted(component_dir.glob("*.safetensors"))
            if not files:
                raise WorkerError("UNSAFE_WEIGHTS", f"El componente {name} no contiene pesos safetensors.")
            for file_path in files:
                relative = file_path.relative_to(model).as_posix()
                safe_file = _trusted_child(model, relative, directory=False)
                size = safe_file.stat().st_size
                if size > MAX_OUTPUT_BYTES:
                    raise WorkerError("MODEL_INVALID", "Un artefacto del bundle supera el límite de tamaño.")
                components.append({
                    "relative_path": relative,
                    "kind": "safetensors",
                    "role": role,
                    "required": is_required,
                    "size_bytes": str(size),
                    "facts": facts,
                })
        elif role == "tokenizer":
            files = [component_dir / name for name in TOKENIZER_FILES if (component_dir / name).is_file()]
            if not files:
                raise WorkerError("MODEL_INVALID", f"El tokenizador {name} no contiene configuración local reconocida.")
            for relative_file in ("tokenizer_config.json", "tokenizer.json"):
                candidate = component_dir / relative_file
                if candidate.exists():
                    _read_json(_trusted_child(model, f"{name}/{relative_file}", directory=False), MAX_CONFIG_BYTES)
            for file_path in sorted(files):
                relative = file_path.relative_to(model).as_posix()
                safe_file = _trusted_child(model, relative, directory=False)
                components.append({
                    "relative_path": relative,
                    "kind": "tokenizer",
                    "role": role,
                    "required": is_required,
                    "size_bytes": str(safe_file.stat().st_size),
                    "facts": facts,
                })
        else:
            config_name = COMPONENT_CONFIGS[role]
            config_size = "0"
            if config_name is not None:
                config_path = _trusted_child(model, f"{name}/{config_name}", directory=False)
                _read_json(config_path, MAX_CONFIG_BYTES)
                config_size = str(config_path.stat().st_size)
            components.append({
                "relative_path": f"{name}/{config_name or 'tokenizer_config.json'}",
                "kind": "config",
                "role": role,
                "required": is_required,
                "size_bytes": config_size,
                "facts": facts,
            })
    for name in expected:
        if name not in index or index[name] is None:
            raise WorkerError("MODEL_INVALID", f"Falta el componente obligatorio {name} en el Model Bundle.")
    components.append({
        "relative_path": "model_index.json",
        "kind": "diffusers_index",
        "role": "config",
        "required": True,
        "size_bytes": str(index_path.stat().st_size),
        "facts": {"diffusers_component": "model_index"},
    })
    components.sort(key=lambda item: item["relative_path"])
    return {
        "pipeline_class": pipeline,
        "diffusers_version": index.get("_diffusers_version"),
        "model_index_sha256": hashlib.sha256(index_path.read_bytes()).hexdigest(),
        "component_roles": sorted(mapped, key=lambda item: item["component"]),
        "components": components,
    }


def validate_image_input(value: Any) -> dict[str, Any]:
    item = _object(value, {"prompt", "negative_prompt", "width", "height", "steps", "seed"})
    if not isinstance(item["prompt"], str) or not item["prompt"]:
        raise WorkerError("INVALID_REQUEST", "prompt debe ser texto no vacío.")
    if not isinstance(item["negative_prompt"], str):
        raise WorkerError("INVALID_REQUEST", "negative_prompt debe ser texto.")
    if len(item["prompt"].encode("utf-8")) + len(item["negative_prompt"].encode("utf-8")) > MAX_REQUEST_BYTES:
        raise WorkerError("INVALID_REQUEST", "Los prompts superan el límite del frame de protocolo.")
    for field in ("width", "height"):
        number = item[field]
        if isinstance(number, bool) or not isinstance(number, int) or not 64 <= number <= 8192:
            raise WorkerError("INVALID_REQUEST", f"{field} debe estar entre 64 y 8192.")
    steps = item["steps"]
    if isinstance(steps, bool) or not isinstance(steps, int) or not 1 <= steps <= 200:
        raise WorkerError("INVALID_REQUEST", "steps debe estar entre 1 y 200.")
    seed = item["seed"]
    if seed is not None and (isinstance(seed, bool) or not isinstance(seed, int) or not 0 <= seed <= 9_223_372_036_854_775_807):
        raise WorkerError("INVALID_REQUEST", "seed debe ser null o un entero no negativo dentro del límite.")
    return item


def _available_device(torch: Any, backend: str) -> str:
    if backend not in DEVICE_BY_BACKEND:
        raise WorkerError("UNSUPPORTED_CAPABILITY", "El backend Diffusers debe ser CPU, CUDA o XPU explícito.")
    if backend == "CPU":
        return "cpu"
    device = getattr(torch, backend.lower(), None)
    if device is None or not getattr(device, "is_available", lambda: False)():
        raise WorkerError("UNSUPPORTED_CAPABILITY", f"El backend {backend} no está disponible; no se aplicará fallback.")
    return DEVICE_BY_BACKEND[backend]


def load_pipeline(
    model_path: str,
    backend: str,
    *,
    importer: Callable[[str], Any] = importlib.import_module,
) -> tuple[Any, Any, float]:
    if not _managed_interpreter():
        raise WorkerError("UNMANAGED_PYTHON", "Diffusers sólo se carga dentro del intérprete gestionado exacto.")
    model_root = os.environ.get("LARM_DIFFUSERS_MODEL_ROOT")
    if not model_root:
        raise WorkerError("MODEL_UNAVAILABLE", "El host no definió la raíz local autorizada de modelos Diffusers.")
    inspection = inspect_bundle(model_root, model_path)
    os.environ["HF_HUB_OFFLINE"] = "1"
    os.environ["TRANSFORMERS_OFFLINE"] = "1"
    try:
        torch = importer("torch")
        device = _available_device(torch, backend)
        diffusers = importer("diffusers")
        pipeline_class = getattr(diffusers, inspection["pipeline_class"], None)
        if pipeline_class is None or not callable(getattr(pipeline_class, "from_pretrained", None)):
            raise WorkerError("UNSUPPORTED_CAPABILITY", "La clase integrada no está disponible en esta versión de Diffusers.")
        started = time.perf_counter()
        pipeline = pipeline_class.from_pretrained(
            model_path,
            local_files_only=True,
            use_safetensors=True,
            trust_remote_code=False,
        )
        pipeline.to(device)
        load_seconds = max(0.0, time.perf_counter() - started)
        return pipeline, torch, load_seconds
    except WorkerError:
        raise
    except Exception as error:
        raise WorkerError("INFERENCE_FAILED", "No se pudo cargar el bundle local con la versión gestionada.") from error


def _peak_memory(torch: Any, backend: str) -> tuple[int | None, str | None]:
    if backend not in {"CUDA", "XPU"}:
        return None, "CPU_PEAK_MEMORY_UNAVAILABLE"
    device = getattr(torch, backend.lower(), None)
    getter = getattr(device, "max_memory_allocated", None) if device is not None else None
    if not callable(getter):
        return None, "BACKEND_PEAK_MEMORY_UNAVAILABLE"
    try:
        synchronize = getattr(device, "synchronize", None)
        if callable(synchronize):
            synchronize()
        value = getter()
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            return None, "BACKEND_PEAK_MEMORY_UNKNOWN"
        return value, None
    except Exception:
        return None, "BACKEND_PEAK_MEMORY_UNKNOWN"


def _write_json_atomic(path: Path, value: dict[str, Any]) -> None:
    temporary = path.with_name(path.name + ".tmp")
    raw = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    try:
        with temporary.open("xb") as handle:
            handle.write(raw)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except OSError as error:
        try:
            temporary.unlink(missing_ok=True)
        except OSError:
            pass
        raise WorkerError("OUTPUT_IO_FAILED", "No se pudo escribir metadata del artefacto de imagen.") from error


def generate_image(
    request_id: str,
    value: Any,
    *,
    pipeline_loader: Callable[[str, str], tuple[Any, Any, float]] = load_pipeline,
    progress: Callable[[int, int], None] | None = None,
    cancelled: Callable[[], bool] | None = None,
) -> dict[str, Any]:
    _validate_uuid7(request_id, "request_id")
    image_input = validate_image_input(value)
    model_root = os.environ.get("LARM_DIFFUSERS_MODEL_ROOT")
    model_path = os.environ.get("LARM_DIFFUSERS_MODEL_PATH")
    output_root = os.environ.get("LARM_DIFFUSERS_OUTPUT_ROOT")
    backend = os.environ.get("LARM_DIFFUSERS_BACKEND")
    if not model_root or not model_path or not output_root or not backend:
        raise WorkerError("MODEL_UNAVAILABLE", "El host no entregó modelo, destino y backend gestionados.")
    if not Path(output_root).is_absolute():
        raise WorkerError("OUTPUT_IO_FAILED", "El root de salida del host debe ser una ruta absoluta.")
    inspection = inspect_bundle(model_root, model_path)
    try:
        raw_root = Path(os.path.abspath(output_root))
        if _has_reparse_ancestor(raw_root):
            raise ValueError("output root is a reparse point")
        root = raw_root.resolve(strict=True)
        if not root.is_dir():
            raise ValueError("invalid output root")
        output_dir = root / request_id
        output_dir.mkdir(exist_ok=False)
        output_dir.resolve(strict=True).relative_to(root)
    except FileExistsError as error:
        raise WorkerError("OUTPUT_CONFLICT", "Ya existe salida para este request_id.") from error
    except (OSError, ValueError) as error:
        try:
            candidate = Path(output_root) / request_id
            if candidate.is_dir() and not _has_reparse_ancestor(candidate):
                candidate.rmdir()
        except OSError:
            pass
        raise WorkerError("OUTPUT_IO_FAILED", "El directorio de salida no pertenece a la raíz autorizada.") from error
    should_cancel = cancelled or (lambda: False)
    image_path = output_dir / "image.png"
    metadata_path = output_dir / "metadata.json"
    completed = False
    try:
        if should_cancel():
            raise WorkerError("CANCELLED", "El trabajo se canceló antes de cargar el pipeline.")
        pipeline, torch, load_seconds = pipeline_loader(model_path, backend)
        device = _available_device(torch, backend)
        generator = None
        if image_input["seed"] is not None:
            generator = torch.Generator(device=device)
            generator.manual_seed(image_input["seed"])
        reset = getattr(getattr(torch, backend.lower(), None), "reset_peak_memory_stats", None) if backend != "CPU" else None
        if callable(reset):
            reset()
        started = time.perf_counter()

        def on_step_end(_pipe: Any, step: int, _timestep: Any, callback_kwargs: dict[str, Any]) -> dict[str, Any]:
            if should_cancel():
                raise WorkerError("CANCELLED", "El trabajo se canceló durante la generación.")
            if progress is not None:
                progress(step + 1, image_input["steps"])
            return callback_kwargs

        result = pipeline(
            prompt=image_input["prompt"],
            negative_prompt=image_input["negative_prompt"],
            width=image_input["width"],
            height=image_input["height"],
            num_inference_steps=image_input["steps"],
            generator=generator,
            callback_on_step_end=on_step_end,
            callback_on_step_end_tensor_inputs=[],
        )
        generation_seconds = max(0.0, time.perf_counter() - started)
        if should_cancel():
            raise WorkerError("CANCELLED", "El trabajo se canceló antes de publicar el resultado.")
        images = getattr(result, "images", None)
        if not isinstance(images, (list, tuple)) or len(images) != 1:
            raise WorkerError("INFERENCE_FAILED", "El pipeline debe devolver exactamente una imagen.")
        image = images[0]
        if tuple(getattr(image, "size", ())) != (image_input["width"], image_input["height"]):
            raise WorkerError("INFERENCE_FAILED", "La dimensión devuelta no coincide con la solicitud.")
        image.save(image_path.with_name("image.png.tmp"), format="PNG")
        temporary_image = image_path.with_name("image.png.tmp")
        if _is_reparse(temporary_image) or not temporary_image.is_file():
            raise WorkerError("OUTPUT_IO_FAILED", "El pipeline no generó un PNG local válido.")
        os.replace(temporary_image, image_path)
        size = image_path.stat().st_size
        if size <= 0 or size > MAX_OUTPUT_BYTES or should_cancel():
            raise WorkerError("CANCELLED" if should_cancel() else "OUTPUT_IO_FAILED", "La salida excede límites o se canceló.")
        hasher = hashlib.sha256()
        with image_path.open("rb") as image_handle:
            for block in iter(lambda: image_handle.read(1_048_576), b""):
                hasher.update(block)
        image_hash = hasher.hexdigest()
        peak_memory, memory_reason = _peak_memory(torch, backend)
        torch_version = str(getattr(torch, "__version__", "unknown"))
        try:
            diffusers_version = str(importlib.metadata.version("diffusers"))
        except importlib.metadata.PackageNotFoundError:
            diffusers_version = "unknown"
        metadata = {
            "schema_version": "1",
            "request_id": request_id,
            "pipeline_class": inspection["pipeline_class"],
            "model_index_sha256": inspection["model_index_sha256"],
            "backend": backend,
            "torch_version": torch_version,
            "diffusers_version": diffusers_version,
            "width": image_input["width"],
            "height": image_input["height"],
            "steps": image_input["steps"],
            "seed": image_input["seed"],
            "model_load_seconds": load_seconds,
            "seconds_per_image": generation_seconds,
            "peak_memory_bytes": peak_memory,
            "memory_reason": memory_reason,
            "artifact_sha256": image_hash,
            "artifact_size_bytes": str(size),
        }
        _write_json_atomic(metadata_path, metadata)
        completed = True
        return {
            "output": {
                "images": [{
                    "artifact_id": request_id,
                    "sha256": image_hash,
                    "media_type": "image/png",
                    "size_bytes": str(size),
                }]
            },
            "metrics": {
                "model_load_seconds": load_seconds,
                "seconds_per_image": generation_seconds,
                "peak_memory_bytes": peak_memory,
                "memory_reason": memory_reason,
                "metadata_path": metadata_path.relative_to(root).as_posix(),
            },
        }
    except WorkerError:
        raise
    except OSError as error:
        raise WorkerError("OUTPUT_IO_FAILED", "Falló la escritura local del artefacto de imagen.") from error
    except Exception as error:
        raise WorkerError("INFERENCE_FAILED", "Falló la inferencia local de Diffusers.") from error
    finally:
        # A failed or cancelled job never leaves a partial image or metadata file.
        if not completed:
            try:
                shutil.rmtree(output_dir)
            except OSError:
                pass
