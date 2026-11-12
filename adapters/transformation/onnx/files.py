"""Conversión ONNX de producto en streaming; accesos bajo roots explícitas."""
import ctypes
import hashlib
import json
import os
import re
import shutil
from datetime import datetime, timezone

import onnx
from larm_adapter_sdk import DomainError
from larm_adapter_sdk.access import path_allowed, relative
from larm_adapter_sdk.capabilities import canonical, digest

BLOCK = 48 << 10
MAX_INPUT = 64 << 20
CONTROL_BUDGET = 3 << 20  # DB <=1 MiB, rollback journal, status y sidecars acotados
TOOL_VERSION = "1.0.0"
CAPABILITY = "vendor.product.onnx"
MAPPING = "product.safetensors.onnx.v1"
ESTIMATOR = "product.onnx.buffers.v1"


def fail(code, reason, *, original=None):
    result = DomainError(code, "No se pudo completar la conversión ONNX", reason)
    if original is not None:
        result.data["details"]["original_error"] = {"system":"transformation", "code":type(original).__name__, "message":"owned_product_operation_failed"}
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
        raise fail("RESOURCE_UNAVAILABLE", "product_onnx_metadata_budget_exceeded")
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=unique, parse_constant=nonfinite)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        raise fail("VALIDATION_FAILED", "product_onnx_document_invalid", original=error) from None


def write_new(path, value, roots, limit=16 << 10):
    raw = canonical(value)
    if len(raw) > limit:
        raise fail("RESOURCE_UNAVAILABLE", "product_onnx_metadata_budget_exceeded")
    target = path_allowed(path, roots, writing=True)
    with target.open("xb") as output:
        path_allowed(target, roots)
        if os.fstat(output.fileno()).st_nlink != 1:
            raise fail("FORBIDDEN", "product_onnx_shared_write_denied")
        output.write(raw)
        output.flush()
        os.fsync(output.fileno())


def hash_file(path, roots, ctx, maximum):
    sha, size = hashlib.sha256(), 0
    with path_allowed(path, roots).open("rb") as source:
        path_allowed(path, roots)
        if not source.seekable() or os.fstat(source.fileno()).st_size > maximum:
            raise fail("RESOURCE_UNAVAILABLE", "product_onnx_file_budget_exceeded")
        while True:
            ctx.check_cancelled()
            chunk = source.read(BLOCK)
            if not chunk:
                break
            size += len(chunk)
            if size > maximum:
                raise fail("RESOURCE_UNAVAILABLE", "product_onnx_file_budget_exceeded")
            sha.update(chunk)
    return {"sha256":sha.hexdigest(), "size_bytes":str(size)}


def input_size(path, roots):
    with path_allowed(path, roots).open("rb") as source:
        return os.fstat(source.fileno()).st_size


def inspect_input(path, expected, roots, ctx):
    """Preflight sin escritura: valida el safetensors fuente y fija el hash y
    tamaño EXACTOS del modelo ONNX determinista — la misma ruta de emisión
    que la conversión recorre, así que predicción y salida no pueden
    divergir."""
    opener = lambda: path_allowed(path, roots).open("rb")
    with opener() as source:
        if os.fstat(source.fileno()).st_size != int(expected["size_bytes"]):
            raise fail("INTEGRITY_FAILED", "product_onnx_input_changed")
    # El sha del INPUT se verifica contra el perfil (input_changed); el sha
    # del modelo es el predicho de la salida determinista.
    input_sha = hashlib.sha256()
    with opener() as source:
        while True:
            chunk = source.read(BLOCK)
            if not chunk:
                break
            ctx.check_cancelled()
            input_sha.update(chunk)
    if input_sha.hexdigest() != expected["sha256"]:
        raise fail("INTEGRITY_FAILED", "product_onnx_input_changed")
    try:
        safetensors = onnx.read_safetensors(opener, int(expected["size_bytes"]))
    except onnx.ONNXError as error:
        raise fail("INTEGRITY_FAILED", "product_onnx_input_invalid", original=error) from None
    sha = hashlib.sha256()
    size = 0

    def sink(chunk):
        nonlocal size
        sha.update(chunk)
        size += len(chunk)

    onnx.stream(safetensors, opener, ctx, sink)
    return {"sha256":sha.hexdigest(), "size_bytes":str(size)}


def convert(path, target, predicted, read_roots, write_roots, ctx, checkpoint, check_space, on_tensor=None):
    """Conversión streaming real: modelo ONNX protobuf emitido hacia el
    staging con checkpoint duradero por bloque; el layout emitido se
    contrasta con el predicho antes de abrir el fichero de salida."""
    try:
        safetensors = onnx.read_safetensors(lambda: path_allowed(path, read_roots).open("rb"), input_size(path, read_roots))
        plan = onnx.layout(safetensors)
    except onnx.ONNXError as error:
        raise fail("INTEGRITY_FAILED", "product_onnx_input_invalid", original=error) from None
    if plan["total"] != int(predicted["size_bytes"]):
        raise fail("CONFLICT", "product_onnx_snapshot_changed")
    total = int(predicted["size_bytes"])
    with path_allowed(target, write_roots, writing=True).open("xb") as output:
        path_allowed(target, write_roots)
        if os.fstat(output.fileno()).st_nlink != 1:
            raise fail("FORBIDDEN", "product_onnx_shared_write_denied")

        def sink(chunk):
            check_space(output.tell())
            output.write(chunk)
            output.flush()
            # Checkpoint duradero ANTES del progreso: el host ve el evento y
            # consulta el offset, que ya debe estar confirmado en el ledger
            # (misma disciplina que el worker de referencia).
            checkpoint(output.tell())
            ctx.progress("ONNX_TENSOR", output.tell(), total, "product_onnx_block_durable")

        written, tensors = onnx.stream(safetensors, lambda: path_allowed(path, read_roots).open("rb"), ctx, sink, lambda _offset: None, on_tensor)
        if written != int(predicted["size_bytes"]) or tensors != len(plan["entries"]):
            raise fail("INTEGRITY_FAILED", "product_onnx_layout_mismatch")


def bundle_hash(artifacts):
    descriptors,seen = [],set()
    for item in sorted(artifacts,key=lambda item:item["relative_path"]):
        name = relative(item["relative_path"])
        size = item["size_bytes"]
        if name.lower() in seen or not isinstance(size,str) or len(size)>16 or not re.fullmatch(r"0|[1-9][0-9]*",size) or int(size)>1<<53 or not re.fullmatch(r"[0-9a-f]{64}",item["sha256"]) or not isinstance(item["required"],bool):
            raise fail("VALIDATION_FAILED","product_onnx_manifest_invalid")
        seen.add(name.lower())
        # BundleEntry del dominio usa número exacto; el DTO ArtifactManifest usa string.
        descriptors.append({"relative_path":name,"sha256":item["sha256"],"size_bytes":int(size),"required":item["required"]})
    return digest(descriptors)


def memory(_input):
    result = {key:None for key in ("weights_bytes", "kv_bytes", "activation_bytes", "workspace_bytes", "overhead_bytes", "total_bytes", "multimodal_bytes", "temporary_bytes", "safety_margin_bytes")}
    result.update(workspace_bytes=str(3 * BLOCK), per_pool=[], confidence="UNKNOWN", evidence_refs=["product:streaming-buffers-only-process-memory-unknown"])
    return result


def volume_key(path):
    if os.name != "nt":
        raise fail("UNSUPPORTED_CAPABILITY", "product_onnx_windows_only")
    mount, name = ctypes.create_unicode_buffer(32768), ctypes.create_unicode_buffer(128)
    kernel = ctypes.windll.kernel32
    if not kernel.GetVolumePathNameW(ctypes.c_wchar_p(str(path)), mount, len(mount)) or not kernel.GetVolumeNameForVolumeMountPointW(ctypes.c_wchar_p(mount.value), name, len(name)):
        raise fail("RESOURCE_UNAVAILABLE", "product_onnx_volume_unknown")
    return name.value.split("{")[1].split("}")[0].lower()


def storage_estimate(output_size, input_size, output, state):
    if volume_key(output) != volume_key(state):
        raise fail("UNSUPPORTED_CAPABILITY", "product_onnx_same_volume_required")
    upper = output_size + CONTROL_BUDGET
    margin = max(1 << 30, (upper + 19) // 20)
    # Este conversor privado no observa reservas ni políticas globales de Core.
    return {"download_bytes":"0", "final_artifact_bytes":str(output_size), "installed_bytes":"0", "derived_cache_bytes":"0", "temporary_peak_bytes":str(CONTROL_BUDGET), "existing_bytes":str(input_size), "peak_extra_bytes":str(upper), "required_free_bytes":str(upper + margin), "upper_bound_bytes":str(upper), "confidence":"ESTIMATED", "reason":"product_payload_exact_control_budget_private_only", "evidence_refs":["product:atomic-same-volume-publish", "product:sqlite-page-cap-and-sidecar-limits"], "per_volume":[{"volume_key":volume_key(output), "peak_extra_bytes":str(upper), "margin_bytes":str(margin), "other_reservations_bytes":"0", "usable_free_bytes":str(shutil.disk_usage(output).free), "eligible":False, "reason":"core_admission_and_global_reservations_not_accredited"}]}
