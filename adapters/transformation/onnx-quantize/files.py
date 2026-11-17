"""Cuantización ONNX de producto en streaming; accesos bajo roots explícitas."""
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path

import onnxq_proto
from larm_adapter_sdk import DomainError
from larm_adapter_sdk.access import path_allowed, relative
from larm_adapter_sdk.capabilities import canonical, digest

BLOCK = 48 << 10
MAX_INPUT = 64 << 20
CONTROL_BUDGET = 3 << 20
TOOL_VERSION = "1.0.0"
CAPABILITY = "vendor.product.onnxq"
MAPPING = "product.onnx.onnxq.v1"
ESTIMATOR = "product.onnxq.buffers.v1"
QUANTIZATIONS = ("INT8",)


def fail(code, reason, *, original=None):
    result = DomainError(code, "No se pudo completar la cuantización ONNX", reason)
    if original is not None:
        result.data["details"]["original_error"] = {"system": "transformation", "code": type(original).__name__, "message": "owned_product_operation_failed"}
    return result


def stamp(value=None):
    return (value or datetime.now(timezone.utc)).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def unique(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ValueError("Documento duplicado")
        result[name] = value
    return result


def nonfinite(_value):
    raise ValueError("Número no finito")


def read_json(path, roots, limit=1 << 20):
    raw = path_allowed(path, roots).read_bytes()
    if len(raw) > limit:
        raise fail("RESOURCE_UNAVAILABLE", "product_onnxq_metadata_budget_exceeded")
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=unique, parse_constant=nonfinite)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise fail("VALIDATION_FAILED", "product_onnxq_document_invalid", original=error) from None


def write_new(path, value, roots, limit=16 << 10):
    raw = canonical(value)
    if len(raw) > limit:
        raise fail("RESOURCE_UNAVAILABLE", "product_onnxq_metadata_budget_exceeded")
    target = path_allowed(path, roots, writing=True)
    with target.open("xb") as output:
        path_allowed(target, roots)
        if os.fstat(output.fileno()).st_nlink != 1:
            raise fail("FORBIDDEN", "product_onnxq_shared_write_denied")
        output.write(raw)
        output.flush()
        os.fsync(output.fileno())


def hash_file(path, roots, ctx, maximum):
    sha, size = hashlib.sha256(), 0
    with path_allowed(path, roots).open("rb") as source:
        path_allowed(path, roots)
        if not source.seekable() or os.fstat(source.fileno()).st_size > maximum:
            raise fail("RESOURCE_UNAVAILABLE", "product_onnxq_file_budget_exceeded")
        while True:
            ctx.check_cancelled()
            chunk = source.read(BLOCK)
            if not chunk:
                break
            sha.update(chunk)
            size += len(chunk)
    return {"sha256": sha.hexdigest(), "size_bytes": str(size)}


def input_size(path, roots):
    with path_allowed(path, roots).open("rb") as source:
        return os.fstat(source.fileno()).st_size


def runtime_path():
    runtime = os.environ.get("OLIVE_RUNTIME", "")
    if not runtime:
        raise fail("RESOURCE_UNAVAILABLE", "product_onnxq_runtime_missing")
    return runtime


def inspect_input(path, expected, roots, ctx, quantization, runtime):
    """Preflight con la MISMA cuantización que la conversión: ejecuta el
    subproceso del runtime a un staging efímero y fija el hash y tamaño
    EXACTOS del output, así que predicción y salida no pueden divergir."""
    raw = path_allowed(path, roots).read_bytes()
    if len(raw) != int(expected["size_bytes"]) or hashlib.sha256(raw).hexdigest() != expected["sha256"]:
        raise fail("INTEGRITY_FAILED", "product_onnxq_input_changed")
    try:
        onnxq_proto.parse_onnx_structure(raw)
    except onnxq_proto.ONNXQError as error:
        raise fail("INTEGRITY_FAILED", "product_onnxq_input_invalid", original=error) from None
    import tempfile
    with tempfile.TemporaryDirectory(prefix="onnxq-preflight-") as directory:
        predicted_target = Path(directory) / "model.onnx"
        _run_quantization(path, predicted_target, quantization, runtime, ctx)
        predicted_raw = predicted_target.read_bytes()
    return {"sha256": hashlib.sha256(predicted_raw).hexdigest(), "size_bytes": str(len(predicted_raw))}


def volume_key(path):
    if os.name != "nt":
        raise fail("UNSUPPORTED_CAPABILITY", "product_onnxq_windows_only")
    root = os.path.splitdrive(str(Path(path).resolve()))[0].upper()
    if not root:
        raise fail("UNSUPPORTED_CAPABILITY", "product_onnxq_windows_only")
    return root + ':'


def storage_estimate(output_size, input_size, output, state):
    if volume_key(output) != volume_key(state):
        raise fail("UNSUPPORTED_CAPABILITY", "product_onnxq_same_volume_required")
    upper = output_size + CONTROL_BUDGET
    margin = max(1 << 30, (upper + 19) // 20)
    # Este conversor privado no observa reservas ni políticas globales de Core.
    return {"download_bytes": "0", "final_artifact_bytes": str(output_size), "installed_bytes": "0", "derived_cache_bytes": "0", "temporary_peak_bytes": str(CONTROL_BUDGET), "existing_bytes": str(input_size), "peak_extra_bytes": str(upper), "required_free_bytes": str(upper + margin), "upper_bound_bytes": str(upper), "confidence": "ESTIMATED", "reason": "product_payload_exact_control_budget_private_only", "evidence_refs": ["product:atomic-same-volume-publish", "product:sqlite-page-cap-and-sidecar-limits"], "per_volume": [{"volume_key": volume_key(output), "peak_extra_bytes": str(upper), "margin_bytes": str(margin), "other_reservations_bytes": "0", "usable_free_bytes": str(shutil.disk_usage(output).free), "eligible": False, "reason": "core_admission_and_global_reservations_not_accredited"}]}


def _run_quantization(path, target, quantization, runtime, ctx):
    """Subproceso del runtime gestionado python-olive con el script
    quantize.py del paquete (pass OnnxDynamicQuantization de Olive)."""
    interpreter = os.path.join(runtime, "Scripts", "python.exe")
    script = Path(__file__).resolve().parent / "quantize.py"
    # Scratch corto y ASCII para los ficheros temporales del runtime: los
    # lanzadores y ORT fallan con rutas extendidas \?\ bajo el TEMP de
    # trabajo del worker (que contiene la ruta gestionada con unicode).
    scratch = os.environ.get("USERPROFILE")
    scratch = Path(scratch, "AppData", "Local", "Temp") if scratch else None
    if scratch is not None:
        scratch.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "PYTHONIOENCODING": "utf-8"}
    if scratch is not None:
        env["TEMP"] = env["TMP"] = env["TMPDIR"] = str(scratch)
    process = subprocess.run(
        [interpreter, "-X", "utf8", str(script), str(path), str(target), quantization],
        capture_output=True, timeout=1800, env=env,
        # El subproceso del runtime no puede heredar el stdin RPC del worker:
        # la cadena de lanzadores del runtime sellado se bloquea con un pipe
        # de stdin abierto y sin EOF (colgue del plan completo).
        stdin=subprocess.DEVNULL,
    )
    lines = [line for line in process.stdout.decode("utf-8", "replace").splitlines() if line.startswith("{")]
    if not lines or process.returncode != 0:
        raise fail("INTEGRITY_FAILED", "product_onnxq_quant_failed", original=RuntimeError(process.stderr.decode("utf-8", "replace")[-400:]))
    result = json.loads(lines[-1])
    if result["status"] != "ok":
        raise fail("INTEGRITY_FAILED", "product_onnxq_quant_failed", original=RuntimeError(result.get("reason", "")))
    return result


def convert(path, target, predicted, read_roots, write_roots, ctx, checkpoint, check_space, quantization):
    """Cuantización real hacia el staging definitivo: el subproceso del
    runtime gestiona la emisión completa; el worker verifica hash, tamaño y
    estructura QDQ del resultado, y fija el checkpoint duradero."""
    runtime = runtime_path()
    total = int(predicted["size_bytes"])
    check_space(0)
    result = _run_quantization(path, path_allowed(target, write_roots, writing=True), quantization, runtime, ctx)
    raw = path_allowed(target, read_roots).read_bytes()
    if hashlib.sha256(raw).hexdigest() != result["sha256"] or str(len(raw)) != result["size_bytes"]:
        raise fail("INTEGRITY_FAILED", "product_onnxq_output_changed")
    if len(raw) != total:
        raise fail("CONFLICT", "product_onnxq_snapshot_changed")
    try:
        onnxq_proto.parse_quantized_structure(raw)
    except onnxq_proto.ONNXQError as error:
        raise fail("INTEGRITY_FAILED", "product_onnxq_output_invalid", original=error) from None
    checkpoint(len(raw))
    ctx.progress("ONNXQ_TENSOR", len(raw), total, "product_onnxq_model_durable")


def bundle_hash(artifacts):
    descriptors, seen = [], set()
    for item in sorted(artifacts, key=lambda item: item["relative_path"]):
        name = relative(item["relative_path"])
        size = item["size_bytes"]
        if name.lower() in seen or not isinstance(size, str) or len(size) > 16 or not re.fullmatch(r"0|[1-9][0-9]*", size) or int(size) > 1 << 53 or not re.fullmatch(r"[0-9a-f]{64}", item["sha256"]) or not isinstance(item["required"], bool):
            raise fail("VALIDATION_FAILED", "product_onnxq_manifest_invalid")
        seen.add(name.lower())
        descriptors.append({"relative_path": name, "sha256": item["sha256"], "size_bytes": int(size), "required": item["required"]})
    return digest(descriptors)


def memory(_input):
    result = {key: None for key in ("weights_bytes", "kv_bytes", "activation_bytes", "workspace_bytes", "overhead_bytes", "total_bytes", "multimodal_bytes", "temporary_bytes", "safety_margin_bytes")}
    result.update(workspace_bytes=str(3 * BLOCK), per_pool=[], confidence="UNKNOWN", evidence_refs=["product:streaming-buffers-only-process-memory-unknown"])
    return result
