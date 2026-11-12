"""Transformación streaming real; todos los accesos quedan bajo roots explícitas."""
import base64
import ctypes
import hashlib
import json
import os
import re
import shutil
from datetime import datetime, timezone

from larm_adapter_sdk import DomainError
from larm_adapter_sdk.access import path_allowed, relative
from larm_adapter_sdk.capabilities import canonical, digest

BLOCK = 48 << 10  # múltiplo de tres: concatenar bloques conserva Base64 RFC4648
MAX_INPUT = 64 << 20
CONTROL_BUDGET = 3 << 20  # DB <=1 MiB, rollback journal, status y sidecars acotados
TOOL_VERSION = "1.0.0"
CAPABILITY = "vendor.reference.base64"
MAPPING = "reference.binary.base64.v1"
ESTIMATOR = "reference.base64.buffers.v1"


def fail(code, reason, *, original=None):
    result = DomainError(code, "No se pudo completar la transformación de referencia", reason)
    if original is not None:
        result.data["details"]["original_error"] = {"system":"transformation", "code":type(original).__name__, "message":"owned_reference_operation_failed"}
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
    with path_allowed(path, roots).open("rb") as source:
        raw = source.read(limit + 1)
    if len(raw) > limit:
        raise fail("RESOURCE_UNAVAILABLE", "reference_transform_document_too_large")
    return json.loads(raw.decode("utf-8"), object_pairs_hook=unique, parse_constant=nonfinite)


def write_new(path, value, roots, limit=16 << 10):
    raw = canonical(value)
    if len(raw) > limit:
        raise fail("RESOURCE_UNAVAILABLE", "reference_transform_metadata_budget_exceeded")
    target = path_allowed(path, roots, writing=True)
    with target.open("xb") as output:
        path_allowed(target, roots)
        if os.fstat(output.fileno()).st_nlink != 1:
            raise fail("FORBIDDEN", "reference_transform_shared_write_denied")
        output.write(raw)
        output.flush()
        os.fsync(output.fileno())


def hash_file(path, roots, ctx, maximum):
    sha, size = hashlib.sha256(), 0
    with path_allowed(path, roots).open("rb") as source:
        path_allowed(path, roots)
        if not source.seekable() or os.fstat(source.fileno()).st_size > maximum:
            raise fail("RESOURCE_UNAVAILABLE", "reference_transform_file_budget_exceeded")
        while True:
            ctx.check_cancelled()
            chunk = source.read(BLOCK)
            if not chunk:
                break
            size += len(chunk)
            if size > maximum:
                raise fail("RESOURCE_UNAVAILABLE", "reference_transform_file_budget_exceeded")
            sha.update(chunk)
    return {"sha256":sha.hexdigest(), "size_bytes":str(size)}


def inspect_input(path, expected, roots, ctx):
    """Preflight sin escritura; fija hashes de entrada y de la salida determinista."""
    source_sha, output_sha, count, output_size = hashlib.sha256(), hashlib.sha256(), 0, 0
    with path_allowed(path, roots).open("rb") as source:
        path_allowed(path, roots)
        if os.fstat(source.fileno()).st_size != int(expected["size_bytes"]):
            raise fail("INTEGRITY_FAILED", "reference_transform_input_changed")
        while True:
            ctx.check_cancelled()
            chunk = source.read(BLOCK)
            if not chunk:
                break
            count += len(chunk)
            if count > MAX_INPUT or count > int(expected["size_bytes"]):
                raise fail("INTEGRITY_FAILED", "reference_transform_input_changed")
            source_sha.update(chunk)
            encoded = base64.b64encode(chunk)
            output_sha.update(encoded)
            output_size += len(encoded)
            ctx.progress("TRANSFORM_PREFLIGHT", count, int(expected["size_bytes"]), "reference_input_observed")
    if count != int(expected["size_bytes"]) or source_sha.hexdigest() != expected["sha256"]:
        raise fail("INTEGRITY_FAILED", "reference_transform_input_changed")
    return {"sha256":output_sha.hexdigest(), "size_bytes":str(output_size)}


def encode(path, target, expected, predicted, read_roots, write_roots, ctx, checkpoint, check_space):
    """No reanuda offsets ni sobrescribe; cada bloque se hace durable antes del ledger."""
    source_sha, output_sha, count, written = hashlib.sha256(), hashlib.sha256(), 0, 0
    target = path_allowed(target, write_roots, writing=True)
    with path_allowed(path, read_roots).open("rb") as source, target.open("xb") as output:
        path_allowed(path, read_roots)
        path_allowed(target, write_roots)
        if os.fstat(output.fileno()).st_nlink != 1:
            raise fail("FORBIDDEN", "reference_transform_shared_write_denied")
        while True:
            ctx.check_cancelled()
            chunk = source.read(BLOCK)
            if not chunk:
                break
            count += len(chunk)
            if count > int(expected["size_bytes"]):
                raise fail("INTEGRITY_FAILED", "reference_transform_input_changed")
            source_sha.update(chunk)
            data = base64.b64encode(chunk)
            if written + len(data) > int(predicted["size_bytes"]):
                raise fail("STORAGE_INSUFFICIENT", "reference_transform_output_budget_exceeded")
            check_space(written)
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            output_sha.update(data)
            written += len(data)
            checkpoint(written)
            ctx.progress("TRANSFORM_ENCODING", written, int(predicted["size_bytes"]), "reference_block_durable")
    if count != int(expected["size_bytes"]) or source_sha.hexdigest() != expected["sha256"] or output_sha.hexdigest() != predicted["sha256"] or written != int(predicted["size_bytes"]):
        raise fail("INTEGRITY_FAILED", "reference_transform_bytes_changed")


def bundle_hash(artifacts):
    descriptors,seen = [],set()
    for item in sorted(artifacts,key=lambda item:item["relative_path"]):
        name = relative(item["relative_path"])
        size = item["size_bytes"]
        if name.lower() in seen or not isinstance(size,str) or len(size)>16 or not re.fullmatch(r"0|[1-9][0-9]*",size) or int(size)>1<<53 or not re.fullmatch(r"[0-9a-f]{64}",item["sha256"]) or not isinstance(item["required"],bool):
            raise fail("VALIDATION_FAILED","reference_transform_manifest_invalid")
        seen.add(name.lower())
        # BundleEntry del dominio usa número exacto; el DTO ArtifactManifest usa string.
        descriptors.append({"relative_path":name,"sha256":item["sha256"],"size_bytes":int(size),"required":item["required"]})
    return digest(descriptors)


def memory(_input):
    result = {key:None for key in ("weights_bytes", "kv_bytes", "activation_bytes", "workspace_bytes", "overhead_bytes", "total_bytes", "multimodal_bytes", "temporary_bytes", "safety_margin_bytes")}
    result.update(workspace_bytes=str(3 * BLOCK), per_pool=[], confidence="UNKNOWN", evidence_refs=["reference:streaming-buffers-only-process-memory-unknown"])
    return result


def volume_key(path):
    if os.name != "nt":
        raise fail("UNSUPPORTED_CAPABILITY", "reference_transform_windows_only")
    mount, name = ctypes.create_unicode_buffer(32768), ctypes.create_unicode_buffer(128)
    kernel = ctypes.windll.kernel32
    if not kernel.GetVolumePathNameW(ctypes.c_wchar_p(str(path)), mount, len(mount)) or not kernel.GetVolumeNameForVolumeMountPointW(ctypes.c_wchar_p(mount.value), name, len(name)):
        raise fail("RESOURCE_UNAVAILABLE", "reference_transform_volume_unknown")
    return name.value.split("{")[1].split("}")[0].lower()


def storage_estimate(output_size, input_size, output, state):
    if volume_key(output) != volume_key(state):
        raise fail("UNSUPPORTED_CAPABILITY", "reference_transform_same_volume_required")
    upper = output_size + CONTROL_BUDGET
    margin = max(1 << 30, (upper + 19) // 20)
    # Esta referencia privada no observa reservas ni políticas globales de Core.
    return {"download_bytes":"0", "final_artifact_bytes":str(output_size), "installed_bytes":"0", "derived_cache_bytes":"0", "temporary_peak_bytes":str(CONTROL_BUDGET), "existing_bytes":str(input_size), "peak_extra_bytes":str(upper), "required_free_bytes":str(upper + margin), "upper_bound_bytes":str(upper), "confidence":"ESTIMATED", "reason":"reference_payload_exact_control_budget_private_only", "evidence_refs":["reference:atomic-same-volume-publish", "reference:sqlite-page-cap-and-sidecar-limits"], "per_volume":[{"volume_key":volume_key(output), "peak_extra_bytes":str(upper), "margin_bytes":str(margin), "other_reservations_bytes":"0", "usable_free_bytes":str(shutil.disk_usage(output).free), "eligible":False, "reason":"core_admission_and_global_reservations_not_accredited"}]}
