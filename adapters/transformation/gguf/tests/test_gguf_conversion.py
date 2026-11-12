"""Oráculo focal independiente del conversor GGUF de producto.

Construye el paquete real (empaquetado + Python fijado + SDK + contratos),
conduce el Adapter por las cuatro firmas transformation y valida el GGUF
publicado con un parser PROPIO del test que no reutiliza el escritor del
paquete: magic/versión/contadores, kv tipados, infos (dims en orden ggml,
dtype, offsets alineados y monótonos) y los bytes de cada tensor idénticos a
los del safetensors fuente. Incluye negativos de entrada corrupta.
"""
import io
import json
import math
import struct
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(REPO / "adapters/sdk/python"))
sys.path.insert(0, str(REPO / "adapters/transformation/gguf"))

import larm_adapter_sdk  # noqa: E402
from larm_adapter_sdk import Contracts  # noqa: E402
from larm_adapter_sdk.server import CallContext  # noqa: E402

import package as packaging  # noqa: E402

GGML_TYPES = {"F32": 0, "F16": 1, "I8": 16, "I16": 17, "I32": 18, "I64": 26, "F64": 30, "BF16": 31}
ELEMENT_BYTES = {"F64": 8, "F32": 4, "F16": 2, "BF16": 2, "I64": 8, "I32": 4, "I16": 2, "I8": 1}


def read_string(buffer, offset):
    (length,) = struct.unpack_from("<Q", buffer, offset)
    offset += 8
    return buffer[offset:offset + length].decode("utf-8"), offset + length


def read_value(buffer, offset):
    (kind,) = struct.unpack_from("<I", buffer, offset)
    offset += 4
    if kind == 8:
        return read_string(buffer, offset) + (kind,)
    if kind == 4:
        (value,) = struct.unpack_from("<I", buffer, offset)
        return value, offset + 4, kind
    if kind == 11:
        (value,) = struct.unpack_from("<q", buffer, offset)
        return value, offset + 8, kind
    raise AssertionError(f"tipo de metadato no cubierto por el oráculo: {kind}")


def parse_gguf(raw):
    """Parser GGUF v3 independiente: estructura completa del contenedor."""
    assert raw[:4] == b"GGUF", "magic GGUF ausente"
    (version,) = struct.unpack_from("<I", raw, 4)
    assert version == 3, f"versión GGUF {version}"
    (tensor_count, kv_count) = struct.unpack_from("<QQ", raw, 8)
    offset = 24
    metadata = {}
    for _ in range(kv_count):
        key, offset = read_string(raw, offset)
        value, offset, kind = read_value(raw, offset)
        metadata[key] = (value, kind)
    tensors = []
    for _ in range(tensor_count):
        name, offset = read_string(raw, offset)
        (n_dims,) = struct.unpack_from("<I", raw, offset)
        offset += 4
        dims = []
        for _ in range(n_dims):
            (dim,) = struct.unpack_from("<Q", raw, offset)
            offset += 8
            dims.append(dim)
        (ggml_type,) = struct.unpack_from("<I", raw, offset)
        offset += 4
        (data_offset,) = struct.unpack_from("<Q", raw, offset)
        offset += 8
        tensors.append({"name": name, "dims": dims, "type": ggml_type, "offset": data_offset})
    alignment = metadata.get("general.alignment", (32, 4))[0]
    # `offset` ya es absoluto (empezó en 24 tras los contadores): alinear sin
    # volver a sumar el prefijo.
    data_begin = ((offset + alignment - 1) // alignment) * alignment
    return {
        "version": version,
        "metadata": metadata,
        "tensors": tensors,
        "alignment": alignment,
        "data_begin": data_begin,
        "raw": raw,
    }


def parse_safetensors(raw):
    (header_len,) = struct.unpack_from("<Q", raw, 0)
    header = json.loads(raw[8:8 + header_len].decode("utf-8"))
    data_start = 8 + header_len
    return {name: entry for name, entry in header.items() if name != "__metadata__"}, data_start


def corrupt(source, at):
    damaged = bytearray(source)
    damaged[at] ^= 0x01
    return bytes(damaged)


def reference_q8_0_blocks(source_bytes, dtype):
    """Referencia INDEPENDIENTE de bloques Q8_0: aritmética del diseño (f32
    con un solo redondeo vía '<f'; roundf half-away; escala f16 RNE). El test
    no reutiliza la función del paquete: re-deriva los bloques desde el
    safetensors fuente."""
    fmt = "<32f" if dtype == "F32" else "<32e"
    out = bytearray()
    for offset in range(0, len(source_bytes), 32 * ELEMENT_BYTES[dtype]):
        values = list(struct.unpack_from(fmt, source_bytes, offset))
        amax = max(abs(value) for value in values)
        d = struct.unpack("<f", struct.pack("<f", amax / 127.0))[0]
        idiv = struct.unpack("<f", struct.pack("<f", 1.0 / d))[0] if d != 0 else 0.0
        qs = []
        for value in values:
            product = struct.unpack("<f", struct.pack("<f", value * idiv))[0]
            q = math.floor(product + 0.5) if product >= 0 else math.ceil(product - 0.5)
            qs.append(max(-127, min(127, q)))
        out += struct.pack("<e", d)
        out += struct.pack("<32b", *qs)
    return bytes(out)


def reference_q4_0_blocks(source_bytes, dtype):
    """Referencia INDEPENDIENTE de bloques Q4_0: escala con signo d=f32(max/-8),
    truncado del cast C sobre f32(v*id)+8.5, clamp 15, nibbles par-bajo."""
    fmt = "<32f" if dtype == "F32" else "<32e"
    out = bytearray()
    for offset in range(0, len(source_bytes), 32 * ELEMENT_BYTES[dtype]):
        values = list(struct.unpack_from(fmt, source_bytes, offset))
        max_value, max_magnitude = 0.0, 0.0
        for value in values:
            magnitude = abs(value)
            if magnitude > max_magnitude:
                max_magnitude, max_value = magnitude, value
        d = struct.unpack("<f", struct.pack("<f", max_value / -8.0))[0]
        idiv = struct.unpack("<f", struct.pack("<f", 1.0 / d))[0] if d != 0 else 0.0
        qs = []
        for value in values:
            scaled = struct.unpack("<f", struct.pack("<f", value * idiv))[0]
            shifted = struct.unpack("<f", struct.pack("<f", scaled + 8.5))[0]
            qs.append(min(15, math.trunc(shifted)))
        out += struct.pack("<e", d)
        # Empaquetado de MITADES según ggml real: byte j = elemento j (bajo)
        # con el elemento j+16 (alto).
        packed = bytearray(16)
        for j in range(16):
            packed[j] = qs[j] | (qs[j + 16] << 4)
        out += bytes(packed)
    return bytes(out)


def reference_q5_0_blocks(source_bytes, dtype):
    """Referencia INDEPENDIENTE de bloques Q5_0 según ggml real: escala con
    signo d=f32(max/-16), trunc sobre f32(v*id)+16.5, byte j = elementos
    j/j+16 en nibbles, 5º bit del elemento k en el bit k del uint32 qh."""
    fmt = "<32f" if dtype == "F32" else "<32e"
    out = bytearray()
    for offset in range(0, len(source_bytes), 32 * ELEMENT_BYTES[dtype]):
        values = list(struct.unpack_from(fmt, source_bytes, offset))
        max_value, max_magnitude = 0.0, 0.0
        for value in values:
            magnitude = abs(value)
            if magnitude > max_magnitude:
                max_magnitude, max_value = magnitude, value
        d = struct.unpack("<f", struct.pack("<f", max_value / -16.0))[0]
        idiv = struct.unpack("<f", struct.pack("<f", 1.0 / d))[0] if d != 0 else 0.0
        xis = []
        for value in values:
            scaled = struct.unpack("<f", struct.pack("<f", value * idiv))[0]
            shifted = struct.unpack("<f", struct.pack("<f", scaled + 16.5))[0]
            xis.append(min(31, math.trunc(shifted)))
        out += struct.pack("<e", d)
        qh = [0, 0, 0, 0]
        packed = bytearray(16)
        for j in range(16):
            packed[j] = (xis[j] & 0x0F) | ((xis[j + 16] & 0x0F) << 4)
            qh[j // 8] |= ((xis[j] >> 4) & 1) << (j % 8)
            qh[(j + 16) // 8] |= ((xis[j + 16] >> 4) & 1) << ((j + 16) % 8)
        out += bytes(qh)
        out += bytes(packed)
    return bytes(out)


def reference_nearest_int(fval):
    """Referencia independiente del nearest_int de ggml (punto mágico)."""
    val = struct.unpack("<f", struct.pack("<f", fval + 12582912.0))[0]
    (bits,) = struct.unpack("<i", struct.pack("<f", val))
    return (bits & 0x007FFFFF) - 0x00400000


def reference_f32(value):
    return struct.unpack("<f", struct.pack("<f", value))[0]


def reference_make_qkx2_quants(values, weights, nmax=15, rmin=-1.0, rdelta=0.1, nstep=20):
    """Referencia independiente de make_qkx2_quants (use_mad=false), con f32
    por operación como el C."""
    n = len(values)
    minimum = values[0]
    maximum = values[0]
    sum_w = weights[0]
    sum_x = reference_f32(sum_w * values[0])
    for i in range(1, n):
        if values[i] < minimum:
            minimum = values[i]
        if values[i] > maximum:
            maximum = values[i]
        w = weights[i]
        sum_w = reference_f32(sum_w + w)
        sum_x = reference_f32(sum_x + reference_f32(w * values[i]))
    if minimum > 0:
        minimum = 0.0
    if maximum == minimum:
        return 0.0, -minimum, [0] * n
    iscale = reference_f32(nmax / reference_f32(maximum - minimum))
    scale = reference_f32(1.0 / iscale)
    best_error = 0.0
    L = []
    for i in range(n):
        l = reference_nearest_int(reference_f32(iscale * reference_f32(values[i] - minimum)))
        l = max(0, min(nmax, l))
        L.append(l)
        diff = reference_f32(reference_f32(reference_f32(scale * float(l)) + minimum) - values[i])
        w = weights[i]
        best_error = reference_f32(best_error + reference_f32(w * reference_f32(diff * diff)))
    Laux = [0] * n
    for is_step in range(nstep + 1):
        iscale = reference_f32(reference_f32(rmin + reference_f32(rdelta * float(is_step)) + float(nmax)) / reference_f32(maximum - minimum))
        sum_l = 0.0
        sum_l2 = 0.0
        sum_xl = 0.0
        for i in range(n):
            l = reference_nearest_int(reference_f32(iscale * reference_f32(values[i] - minimum)))
            l = max(0, min(nmax, l))
            Laux[i] = l
            w = weights[i]
            sum_l = reference_f32(sum_l + reference_f32(w * float(l)))
            sum_l2 = reference_f32(sum_l2 + reference_f32(w * reference_f32(float(l) * float(l))))
            sum_xl = reference_f32(sum_xl + reference_f32(w * reference_f32(reference_f32(float(l) * values[i]))))
        D = reference_f32(reference_f32(sum_w * sum_l2) - reference_f32(sum_l * sum_l))
        if D > 0:
            this_scale = reference_f32(reference_f32(reference_f32(sum_w * sum_xl) - reference_f32(sum_x * sum_l)) / D)
            this_min = reference_f32(reference_f32(reference_f32(sum_l2 * sum_x) - reference_f32(sum_l * sum_xl)) / D)
            if this_min > 0:
                this_min = 0.0
                this_scale = reference_f32(sum_xl / sum_l2)
            cur_error = 0.0
            for i in range(n):
                diff = reference_f32(reference_f32(reference_f32(this_scale * float(Laux[i])) + this_min) - values[i])
                w = weights[i]
                cur_error = reference_f32(cur_error + reference_f32(w * reference_f32(diff * diff)))
            if cur_error < best_error:
                L = list(Laux)
                best_error = cur_error
                scale = this_scale
                minimum = this_min
    return scale, -minimum, L


def reference_q4_k_block(source_bytes, dtype):
    """Referencia independiente de un super-bloque Q4_K (144 bytes)."""
    fmt = "<256f" if dtype == "F32" else "<256e"
    values = list(struct.unpack_from(fmt, source_bytes, 0))
    mins = []
    sub_scales = []
    L_all = []
    max_scale = 0.0
    max_min = 0.0
    for sub in range(8):
        chunk = values[32 * sub:32 * sub + 32]
        sum_x2 = 0.0
        for value in chunk:
            sum_x2 = reference_f32(sum_x2 + reference_f32(value * value))
        av_x = reference_f32(math.sqrt(reference_f32(sum_x2 / 32.0)))
        weights = [reference_f32(av_x + abs(v)) for v in chunk]
        scale, the_min, L = reference_make_qkx2_quants(chunk, weights)
        sub_scales.append(scale)
        mins.append(the_min)
        L_all.extend(L)
        if scale > max_scale:
            max_scale = scale
        if the_min > max_min:
            max_min = the_min
    inv_scale = reference_f32(63.0 / max_scale) if max_scale > 0 else 0.0
    inv_min = reference_f32(63.0 / max_min) if max_min > 0 else 0.0
    ls_values = []
    lm_values = []
    for j in range(8):
        ls = min(63, reference_nearest_int(reference_f32(inv_scale * sub_scales[j])))
        lm = min(63, reference_nearest_int(reference_f32(inv_min * mins[j])))
        ls_values.append(ls)
        lm_values.append(lm)
    scales = [0] * 12
    for j in range(8):
        ls, lm = ls_values[j], lm_values[j]
        if j < 4:
            scales[j] = ls
            scales[j + 4] = lm
        else:
            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4)
            scales[j - 4] |= (ls >> 4) << 6
            scales[j] |= (lm >> 4) << 6
    d_bytes = struct.pack("<e", reference_f32(max_scale / 63.0))
    dmin_bytes = struct.pack("<e", reference_f32(max_min / 63.0))
    d_block = struct.unpack("<e", d_bytes)[0]
    dmin_block = struct.unpack("<e", dmin_bytes)[0]
    for j in range(8):
        if j < 4:
            sc = scales[j] & 63
            m = scales[j + 4] & 63
        else:
            sc = (scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)
            m = (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)
        sub_d = reference_f32(d_block * float(sc))
        if not sub_d:
            continue
        sub_dm = reference_f32(dmin_block * float(m))
        for ii in range(32):
            index = 32 * j + ii
            l = reference_nearest_int(reference_f32(reference_f32(values[index] + sub_dm) / sub_d))
            L_all[index] = max(0, min(15, l))
    qs = bytearray(128)
    for j in range(0, 256, 64):
        for l in range(32):
            qs[j // 2 + l] = L_all[j + l] | (L_all[j + l + 32] << 4)
    return d_bytes + dmin_bytes + bytes(scales) + bytes(qs)


def reference_q5_k_block(source_bytes, dtype):
    """Referencia independiente de un super-bloque Q5_K (176 bytes): misma
    maquinaria que Q4_K con niveles 5 bits y bits altos en qh."""
    fmt = "<256f" if dtype == "F32" else "<256e"
    values = list(struct.unpack_from(fmt, source_bytes, 0))
    mins = []
    sub_scales = []
    L_all = []
    max_scale = 0.0
    max_min = 0.0
    for sub in range(8):
        chunk = values[32 * sub:32 * sub + 32]
        sum_x2 = 0.0
        for value in chunk:
            sum_x2 = reference_f32(sum_x2 + reference_f32(value * value))
        av_x = reference_f32(math.sqrt(reference_f32(sum_x2 / 32.0)))
        weights = [reference_f32(av_x + abs(v)) for v in chunk]
        scale, the_min, L = reference_make_qkx2_quants(chunk, weights, nmax=31, rmin=-0.5, nstep=15)
        sub_scales.append(scale)
        mins.append(the_min)
        L_all.extend(L)
        if scale > max_scale:
            max_scale = scale
        if the_min > max_min:
            max_min = the_min
    inv_scale = reference_f32(63.0 / max_scale) if max_scale > 0 else 0.0
    inv_min = reference_f32(63.0 / max_min) if max_min > 0 else 0.0
    ls_values = []
    lm_values = []
    for j in range(8):
        ls = min(63, reference_nearest_int(reference_f32(inv_scale * sub_scales[j])))
        lm = min(63, reference_nearest_int(reference_f32(inv_min * mins[j])))
        ls_values.append(ls)
        lm_values.append(lm)
    scales = [0] * 12
    for j in range(8):
        ls, lm = ls_values[j], lm_values[j]
        if j < 4:
            scales[j] = ls
            scales[j + 4] = lm
        else:
            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4)
            scales[j - 4] |= (ls >> 4) << 6
            scales[j] |= (lm >> 4) << 6
    d_bytes = struct.pack("<e", reference_f32(max_scale / 63.0))
    dmin_bytes = struct.pack("<e", reference_f32(max_min / 63.0))
    d_block = struct.unpack("<e", d_bytes)[0]
    dmin_block = struct.unpack("<e", dmin_bytes)[0]
    for j in range(8):
        if j < 4:
            sc = scales[j] & 63
            m = scales[j + 4] & 63
        else:
            sc = (scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)
            m = (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)
        sub_d = reference_f32(d_block * float(sc))
        if not sub_d:
            continue
        sub_dm = reference_f32(dmin_block * float(m))
        for ii in range(32):
            index = 32 * j + ii
            l = reference_nearest_int(reference_f32(reference_f32(values[index] + sub_dm) / sub_d))
            L_all[index] = max(0, min(31, l))
    qh = bytearray(32)
    qs = bytearray(128)
    m1 = 1
    m2 = 2
    cursor = 0
    for n in range(0, 256, 64):
        for j in range(32):
            l1 = L_all[n + j]
            if l1 > 15:
                l1 -= 16
                qh[j] |= m1
            l2 = L_all[n + j + 32]
            if l2 > 15:
                l2 -= 16
                qh[j] |= m2
            qs[cursor + j] = l1 | (l2 << 4)
        cursor += 32
        m1 <<= 2
        m2 <<= 2
    return d_bytes + dmin_bytes + bytes(scales) + bytes(qh) + bytes(qs)


def reference_make_qx_quants(values, nmax):
    """Referencia independiente de make_qx_quants (rmse_type=1, qw=NULL) con
    f32 por operación, como el C de la referencia ggml."""
    n = len(values)
    max_value = 0.0
    amax = 0.0
    for value in values:
        ax = abs(value)
        if ax > amax:
            amax = ax
            max_value = value
    if amax < 1e-15:
        return 0.0, [0] * n
    iscale = reference_f32(-float(nmax) / max_value)
    L = []
    sumlx = 0.0
    suml2 = 0.0
    for i in range(n):
        l = reference_nearest_int(reference_f32(iscale * values[i]))
        l = max(-nmax, min(nmax - 1, l))
        L.append(l + nmax)
        w = reference_f32(values[i] * values[i])
        sumlx = reference_f32(sumlx + reference_f32(w * reference_f32(reference_f32(values[i] * float(l)))))
        suml2 = reference_f32(suml2 + reference_f32(w * reference_f32(float(l) * float(l))))
    scale = reference_f32(sumlx / suml2) if suml2 else 0.0
    best = reference_f32(scale * sumlx)
    for is_step in range(-9, 10):
        if is_step == 0:
            continue
        iscale = reference_f32(-reference_f32(float(nmax) + reference_f32(0.1 * float(is_step))) / max_value)
        sumlx = 0.0
        suml2 = 0.0
        for i in range(n):
            l = reference_nearest_int(reference_f32(iscale * values[i]))
            l = max(-nmax, min(nmax - 1, l))
            w = reference_f32(values[i] * values[i])
            sumlx = reference_f32(sumlx + reference_f32(w * reference_f32(reference_f32(values[i] * float(l)))))
            suml2 = reference_f32(suml2 + reference_f32(w * reference_f32(float(l) * float(l))))
        if suml2 > 0 and reference_f32(sumlx * sumlx) > reference_f32(best * suml2):
            L = []
            for i in range(n):
                l = reference_nearest_int(reference_f32(iscale * values[i]))
                l = max(-nmax, min(nmax - 1, l))
                L.append(l + nmax)
            scale = reference_f32(sumlx / suml2)
            best = reference_f32(scale * sumlx)
    return scale, L


def reference_q6_k_block(source_bytes, dtype):
    """Referencia independiente de un super-bloque Q6_K (210 bytes)."""
    fmt = "<256f" if dtype == "F32" else "<256e"
    values = list(struct.unpack_from(fmt, source_bytes, 0))
    sub_scales = []
    L_all = []
    max_scale = 0.0
    for sub in range(16):
        chunk = values[16 * sub:16 * sub + 16]
        scale, L = reference_make_qx_quants(chunk, 32)
        sub_scales.append(scale)
        L_all.extend(L)
        if scale > max_scale:
            max_scale = scale
    if max_scale < 1e-15:
        d_bytes = struct.pack("<e", 0.0)
        scales = [0] * 16
        L_all = [0] * 256
    else:
        iscale = reference_f32(-128.0 / max_scale)
        d_bytes = struct.pack("<e", reference_f32(1.0 / iscale))
        scales = [min(127, reference_nearest_int(reference_f32(iscale * sub_scales[j]))) for j in range(16)]
        d_block = struct.unpack("<e", d_bytes)[0]
        for j in range(16):
            d_sub = reference_f32(d_block * float(scales[j]))
            if not d_sub:
                continue
            for ii in range(16):
                index = 16 * j + ii
                l = reference_nearest_int(reference_f32(values[index] / d_sub))
                L_all[index] = max(-32, min(31, l)) + 32
    ql = bytearray(128)
    qh = bytearray(64)
    for j in range(0, 256, 128):
        base = j // 128
        for l in range(32):
            q1 = L_all[j + l] & 0xF
            q2 = L_all[j + l + 32] & 0xF
            q3 = L_all[j + l + 64] & 0xF
            q4 = L_all[j + l + 96] & 0xF
            ql[base * 64 + l] = q1 | (q3 << 4)
            ql[base * 64 + l + 32] = q2 | (q4 << 4)
            qh[base * 32 + l] = (
                (L_all[j + l] >> 4)
                | ((L_all[j + l + 32] >> 4) << 2)
                | ((L_all[j + l + 64] >> 4) << 4)
                | ((L_all[j + l + 96] >> 4) << 6)
            )
    return d_bytes + bytes(scale_byte & 0xFF for scale_byte in scales) + bytes(ql) + bytes(qh)


def dequantize_q8_0(blocks):
    """Dequantización semántica: (d, q) → d*q por elemento, para acotar el
    error de aproximación contra la fuente."""
    values = []
    for offset in range(0, len(blocks), 34):
        (d,) = struct.unpack_from("<e", blocks, offset)
        qs = struct.unpack_from("<32b", blocks, offset + 2)
        values.extend(d * q for q in qs)
    return values


def quantized_adapters(directory, fixture):
    """Empaqueta y conduce el Adapter real sobre `fixture`; devuelve el
    adaptador con sesión abierta y el contexto de llamada."""
    root = Path(directory)
    manifest, _profile, _entry = packaging.create(REPO, root / "package", root / "work", fixture)
    contracts = Contracts(root / "package" / "docs")
    sys.path.insert(0, str(root / "package" / "transformation"))
    from adapter import Adapter
    adapter = Adapter(root / "package", root / "work", manifest, contracts)
    adapter.open_session()
    ctx = CallContext(
        "01920000-0000-7000-8000-000000000003",
        __import__("threading").Event(),
        lambda *_: None,
    )
    return root, adapter, ctx, contracts


class GGUFConversionOracle(unittest.TestCase):
    def test_conversion_real_publicada_y_verificada_por_parse_independiente(self):
        fixture = packaging.build_safetensors_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, profile, entry = packaging.create(
                REPO, root / "package", root / "work", fixture
            )
            contracts = Contracts(root / "package" / "docs")
            sys.path.insert(0, str(root / "package" / "transformation"))
            try:
                from adapter import Adapter
                adapter = Adapter(
                    root / "package",
                    root / "work",
                    manifest,
                    contracts,
                )
                adapter.open_session()
                ctx = CallContext("01920000-0000-7000-8000-000000000001", __import__("threading").Event(), lambda *_: None)
                described = adapter.call("transformation.describe", {}, ctx)
                self.assertEqual(described["formats_output"], ["GGUF"])
                self.assertEqual(described["formats_input"], ["SAFETENSORS"])
                request = adapter.request()
                plan = adapter.call("transformation.plan", request, ctx)
                self.assertEqual(plan["steps"][0]["phase"], "GGUF")
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                self.assertTrue(data_artifact["relative_path"].endswith("/model.gguf"))
                published = root / "work" / "output" / data_artifact["relative_path"]
                raw = published.read_bytes()
                self.assertEqual(int(data_artifact["size_bytes"]), len(raw))
                self.assertEqual(data_artifact["sha256"], __import__("hashlib").sha256(raw).hexdigest())

                # El oráculo independiente parsea el contenedor completo.
                parsed = parse_gguf(raw)
                self.assertEqual(parsed["alignment"], 32)
                self.assertEqual(parsed["metadata"]["general.architecture"], ("product.test", 8))
                self.assertEqual(parsed["metadata"]["general.alignment"], (32, 4))
                source_tensors, source_data_start = parse_safetensors(fixture)
                self.assertEqual(len(parsed["tensors"]), len(source_tensors))
                previous_end = None
                for tensor in parsed["tensors"]:
                    source = source_tensors[tensor["name"]]
                    self.assertEqual(tensor["type"], GGML_TYPES[source["dtype"]])
                    self.assertEqual(
                        list(reversed(source["shape"])),
                        tensor["dims"],
                        "los dims ggml deben ir del eje más rápido al más lento",
                    )
                    begin = parsed["data_begin"] + tensor["offset"]
                    end = begin + (source["data_offsets"][1] - source["data_offsets"][0])
                    if previous_end is not None:
                        self.assertGreaterEqual(begin, previous_end)
                    self.assertEqual(begin % parsed["alignment"], 0, "offset del tensor desalineado")
                    expected = fixture[source_data_start + source["data_offsets"][0]:source_data_start + source["data_offsets"][1]]
                    self.assertEqual(raw[begin:end], expected, f"bytes del tensor {tensor['name']} alterados")
                    previous_end = end
                self.assertEqual(
                    parsed["tensors"][-1]["offset"] + (source_tensors[parsed["tensors"][-1]["name"]]["data_offsets"][1] - source_tensors[parsed["tensors"][-1]["name"]]["data_offsets"][0]),
                    len(raw) - parsed["data_begin"],
                    "el contenedor no termina donde termina el último tensor",
                )
                self.assertEqual(entry["fixture"]["output"]["sha256"], __import__("hashlib").sha256(raw).hexdigest(), "el fixture del paquete no coincide con la conversión real")
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_input_corrupto_falla_cerrado_sin_publicar(self):
        fixture = packaging.build_safetensors_fixture()
        (header_len,) = struct.unpack_from("<Q", fixture, 0)
        header_begin, header_end = 8, 8 + header_len
        dtype_at = fixture.index(b'"F32"', header_begin, header_end)
        damaged = corrupt(fixture, dtype_at + 2)  # el dtype del primer tensor deja de ser copiable
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest, _profile, _entry = packaging.create(
                REPO, root / "package", root / "work", damaged
            )
            contracts = Contracts(root / "package" / "docs")
            sys.path.insert(0, str(root / "package" / "transformation"))
            try:
                from adapter import Adapter
                adapter = Adapter(
                    root / "package",
                    root / "work",
                    manifest,
                    contracts,
                )
                adapter.open_session()
                ctx = CallContext("01920000-0000-7000-8000-000000000002", __import__("threading").Event(), lambda *_: None)
                # La cabecera corrupta no se parsea: el plan falla cerrado y
                # nada se publica.
                with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                    adapter.call("transformation.plan", adapter.request(), ctx)
                self.assertEqual(raised.exception.data["details"]["reason"], "product_gguf_input_invalid")
                self.assertFalse(any((root / "work" / "output").iterdir()))
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]


    def test_cuantizacion_q8_0_publicada_y_verificada_por_referencia_independiente(self):
        import math
        fixture = packaging.build_quantization_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                described = adapter.call("transformation.describe", {}, ctx)
                self.assertEqual(
                    described["parameter_schema"],
                    {"type": "object", "properties": {"quantization": {"type": "string", "enum": ["Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K", "Q6_K"]}}, "additionalProperties": False},
                )
                self.assertEqual(described["tool_version"], "1.1.0")
                request = adapter.request()
                request["parameters"] = {"quantization": "Q8_0"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                parsed = parse_gguf(raw)
                # La cuantización real queda registrada en el file_type u32.
                self.assertEqual(parsed["metadata"]["general.file_type"], (7, 4))
                self.assertEqual(len(parsed["tensors"]), 1)
                tensor = parsed["tensors"][0]
                self.assertEqual(tensor["name"], "q.weight")
                self.assertEqual(tensor["type"], 8, "el tensor debe quedar en Q8_0")
                self.assertEqual(tensor["dims"], [64])
                source_tensors, source_data_start = parse_safetensors(fixture)
                source_raw = fixture[source_data_start + source_tensors["q.weight"]["data_offsets"][0]:source_data_start + source_tensors["q.weight"]["data_offsets"][1]]
                begin = parsed["data_begin"] + tensor["offset"]
                blocks = raw[begin:begin + 2 * 34]
                # Bloques byte a byte contra la referencia independiente.
                self.assertEqual(blocks, reference_q8_0_blocks(source_raw, "F32"))
                self.assertEqual(len(raw) - parsed["data_begin"], 2 * 34, "el contenedor termina en el último bloque")
                # Cota semántica de aproximación: |v - d*q| <= d por elemento.
                approximated = dequantize_q8_0(blocks)
                source_values = list(struct.unpack("<64f", source_raw))
                index = 0
                for offset in range(0, len(blocks), 34):
                    (d,) = struct.unpack_from("<e", blocks, offset)
                    for q in struct.unpack_from("<32b", blocks, offset + 2):
                        self.assertLessEqual(abs(source_values[index] - d * q), d, f"elemento {index} fuera de la cota Q8_0")
                        index += 1
                self.assertEqual(index, 64)
                self.assertEqual(data_artifact["sha256"], __import__("hashlib").sha256(raw).hexdigest())
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_cuantizacion_q4_0_publicada_y_verificada_por_referencia_independiente(self):
        fixture = packaging.build_quantization_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                described = adapter.call("transformation.describe", {}, ctx)
                self.assertEqual(
                    described["parameter_schema"]["properties"]["quantization"]["enum"],
                    ["Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K", "Q6_K"],
                )
                request = adapter.request()
                request["parameters"] = {"quantization": "Q4_0"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                parsed = parse_gguf(raw)
                self.assertEqual(parsed["metadata"]["general.file_type"], (7, 4))
                tensor = parsed["tensors"][0]
                self.assertEqual(tensor["type"], 2, "el tensor debe quedar en Q4_0")
                self.assertEqual(tensor["dims"], [64])
                source_tensors, source_data_start = parse_safetensors(fixture)
                source_raw = fixture[source_data_start + source_tensors["q.weight"]["data_offsets"][0]:source_data_start + source_tensors["q.weight"]["data_offsets"][1]]
                begin = parsed["data_begin"] + tensor["offset"]
                blocks = raw[begin:begin + 2 * 18]
                self.assertEqual(blocks, reference_q4_0_blocks(source_raw, "F32"))
                self.assertEqual(len(raw) - parsed["data_begin"], 2 * 18, "el contenedor termina en el último bloque")
                # Cota semántica: |v - d*(xi-8)| <= |d| por elemento.
                source_values = list(struct.unpack("<64f", source_raw))
                checked = 0
                for block_index, offset in enumerate(range(0, len(blocks), 18)):
                    base = block_index * 32
                    (d,) = struct.unpack_from("<e", blocks, offset)
                    # La escala Q4_0 tiene signo (d = max/-8): la cota usa |d|.
                    # Empaquetado de mitades: byte j une los elementos j y j+16.
                    bound = abs(d)
                    raw_qs = blocks[offset + 2:offset + 18]
                    for j in range(16):
                        byte = raw_qs[j]
                        for place, element in ((0x0F, base + j), (0x10, base + j + 16)):
                            xi = (byte & place) if place == 0x0F else (byte >> 4)
                            approximated = d * (xi - 8)
                            self.assertLessEqual(abs(source_values[element] - approximated), bound, f"elemento {element} fuera de la cota Q4_0")
                            checked += 1
                self.assertEqual(checked, 64)
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_cuantizacion_q5_0_publicada_y_verificada_por_referencia_independiente(self):
        fixture = packaging.build_quantization_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q5_0"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                parsed = parse_gguf(raw)
                self.assertEqual(parsed["metadata"]["general.file_type"], (7, 4))
                tensor = parsed["tensors"][0]
                self.assertEqual(tensor["type"], 6, "el tensor debe quedar en Q5_0")
                self.assertEqual(tensor["dims"], [64])
                source_tensors, source_data_start = parse_safetensors(fixture)
                source_raw = fixture[source_data_start + source_tensors["q.weight"]["data_offsets"][0]:source_data_start + source_tensors["q.weight"]["data_offsets"][1]]
                begin = parsed["data_begin"] + tensor["offset"]
                blocks = raw[begin:begin + 2 * 22]
                self.assertEqual(blocks, reference_q5_0_blocks(source_raw, "F32"))
                self.assertEqual(len(raw) - parsed["data_begin"], 2 * 22, "el contenedor termina en el último bloque")
                # Cota semántica: |v - d*(xi-16)| <= |d| por elemento; xi se
                # reconstruye desde nibbles y bits altos del uint32 qh.
                source_values = list(struct.unpack("<64f", source_raw))
                checked = 0
                for block_index, offset in enumerate(range(0, len(blocks), 22)):
                    base = block_index * 32
                    (d,) = struct.unpack_from("<e", blocks, offset)
                    bound = abs(d)
                    (qh,) = struct.unpack_from("<I", blocks, offset + 2)
                    raw_qs = blocks[offset + 6:offset + 22]
                    for j in range(16):
                        byte = raw_qs[j]
                        for shift, element in ((0, base + j), (4, base + j + 16)):
                            xi = ((byte >> shift) & 0x0F) | (((qh >> (element - base)) & 1) << 4)
                            approximated = d * (xi - 16)
                            self.assertLessEqual(abs(source_values[element] - approximated), bound, f"elemento {element} fuera de la cota Q5_0")
                            checked += 1
                self.assertEqual(checked, 64)
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_cuantizacion_q4_k_publicada_y_verificada_por_referencia_independiente(self):
        fixture = packaging.build_quantization_k_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q4_K"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                parsed = parse_gguf(raw)
                self.assertEqual(parsed["metadata"]["general.file_type"], (7, 4))
                tensor = parsed["tensors"][0]
                self.assertEqual(tensor["type"], 12, "el tensor debe quedar en Q4_K")
                self.assertEqual(tensor["dims"], [256])
                source_tensors, source_data_start = parse_safetensors(fixture)
                source_raw = fixture[source_data_start + source_tensors["q.kweight"]["data_offsets"][0]:source_data_start + source_tensors["q.kweight"]["data_offsets"][1]]
                begin = parsed["data_begin"] + tensor["offset"]
                block = raw[begin:begin + 144]
                self.assertEqual(block, reference_q4_k_block(source_raw, "F32"))
                self.assertEqual(len(raw) - parsed["data_begin"], 144, "el contenedor termina en el super-bloque")
                # Cota semántica: |x - (d·sc·L − dmin·m)| <= |d·sc|·0.6 (el
                # 0.5 del redondeo más el épsilon de la escala f16). La
                # reconstrucción sigue get_scale_min_k4 y el empaquetado de
                # grupos de 64 (nibble bajo = primer semigrupo).
                d_value = struct.unpack_from("<e", block, 0)[0]
                dmin_value = struct.unpack_from("<e", block, 2)[0]
                scales = block[4:16]
                qs = block[16:]
                source_values = list(struct.unpack("<256f", source_raw))

                def scale_min_k4(j):
                    if j < 4:
                        return scales[j] & 63, scales[j + 4] & 63
                    return (scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4), (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)

                checked = 0
                for element in range(256):
                    group_base = (element // 64) * 64
                    position = element - group_base
                    byte = qs[group_base // 2 + (position if position < 32 else position - 32)]
                    xi = (byte & 0x0F) if position < 32 else (byte >> 4)
                    sc, m = scale_min_k4(element // 32)
                    approximated = d_value * sc * xi - dmin_value * m
                    # Cota conservadora: los sub-bloques con sc==0 se saltan la
                    # re-cuantización (el nivel base de qkx2 deja |x−(scale·L+min)|
                    # ≤ medio paso del sub-bloque), así que la cota se mide desde
                    # el cero del sub-bloque más la cuantización de 6 bits del
                    # mínimo y el épsilon f16 de las escalas.
                    chunk = source_values[32 * (element // 32):32 * (element // 32) + 32]
                    bound = max(abs(v) for v in chunk) + abs(d_value) * sc * 0.6 + abs(dmin_value) * m * 2.0 ** -11 + 1e-30
                    source_value = struct.unpack_from("<f", source_raw, element * 4)[0]
                    self.assertLessEqual(abs(source_value - approximated), bound, f"elemento {element} fuera de la cota Q4_K")
                    checked += 1
                self.assertEqual(checked, 256)
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_cuantizacion_q5_k_publicada_y_verificada_por_referencia_independiente(self):
        fixture = packaging.build_quantization_k_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q5_K"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                parsed = parse_gguf(raw)
                self.assertEqual(parsed["metadata"]["general.file_type"], (7, 4))
                tensor = parsed["tensors"][0]
                self.assertEqual(tensor["type"], 13, "el tensor debe quedar en Q5_K")
                self.assertEqual(tensor["dims"], [256])
                source_tensors, source_data_start = parse_safetensors(fixture)
                source_raw = fixture[source_data_start + source_tensors["q.kweight"]["data_offsets"][0]:source_data_start + source_tensors["q.kweight"]["data_offsets"][1]]
                begin = parsed["data_begin"] + tensor["offset"]
                block = raw[begin:begin + 176]
                self.assertEqual(block, reference_q5_k_block(source_raw, "F32"))
                self.assertEqual(len(raw) - parsed["data_begin"], 176, "el contenedor termina en el super-bloque")
                # Cota semántica: reconstrucción d·sc·L − dmin·m con L de 5 bits
                # reconstruido desde nibbles y qh; cota conservadora con el
                # máximo del sub-bloque para la ruta sc==0.
                d_value = struct.unpack_from("<e", block, 0)[0]
                dmin_value = struct.unpack_from("<e", block, 2)[0]
                scales = block[4:16]
                qh = block[16:48]
                qs = block[48:176]
                source_values = list(struct.unpack("<256f", source_raw))
                checked = 0
                for element in range(256):
                    sub = element // 32
                    chunk = source_values[32 * sub:32 * sub + 32]
                    if sub < 4:
                        sc = scales[sub] & 63
                        m = scales[sub + 4] & 63
                    else:
                        sc = (scales[sub + 4] & 0xF) | ((scales[sub - 4] >> 6) << 4)
                        m = (scales[sub + 4] >> 4) | ((scales[sub] >> 6) << 4)
                    group_base = (element // 64) * 64
                    position = element - group_base
                    byte = qs[(group_base // 2) + (position if position < 32 else position - 32)]
                    nibble = (byte & 0x0F) if position < 32 else (byte >> 4)
                    high_bit = (qh[element % 32] >> (2 * (element // 64))) & 1
                    if position >= 32:
                        high_bit = (qh[element % 32] >> (2 * (element // 64) + 1)) & 1
                    xi = nibble | (high_bit << 4)
                    approximated = d_value * sc * xi - dmin_value * m
                    bound = max(abs(v) for v in chunk) + abs(d_value) * sc * 0.6 + abs(dmin_value) * m * 2.0 ** -11 + 1e-30
                    self.assertLessEqual(abs(source_values[element] - approximated), bound, f"elemento {element} fuera de la cota Q5_K")
                    checked += 1
                self.assertEqual(checked, 256)
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_cuantizacion_q6_k_publicada_y_verificada_por_referencia_independiente(self):
        fixture = packaging.build_quantization_k_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q6_K"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()

                parsed = parse_gguf(raw)
                self.assertEqual(parsed["metadata"]["general.file_type"], (7, 4))
                tensor = parsed["tensors"][0]
                self.assertEqual(tensor["type"], 14, "el tensor debe quedar en Q6_K")
                self.assertEqual(tensor["dims"], [256])
                source_tensors, source_data_start = parse_safetensors(fixture)
                source_raw = fixture[source_data_start + source_tensors["q.kweight"]["data_offsets"][0]:source_data_start + source_tensors["q.kweight"]["data_offsets"][1]]
                begin = parsed["data_begin"] + tensor["offset"]
                block = raw[begin:begin + 210]
                self.assertEqual(block, reference_q6_k_block(source_raw, "F32"))
                self.assertEqual(len(raw) - parsed["data_begin"], 210, "el contenedor termina en el super-bloque")
                # Cota semántica: v ≈ d·sc·(L-32) con L de 6 bits reconstruido
                # desde ql/qh; cota conservadora con el máximo del sub-bloque.
                d_value = struct.unpack_from("<e", block, 0)[0]
                scales_raw = block[2:18]
                ql = block[18:146]
                qh = block[146:210]
                source_values = list(struct.unpack("<256f", source_raw))
                checked = 0
                for element in range(256):
                    sub = element // 16
                    chunk = source_values[16 * sub:16 * sub + 16]
                    sc = scales_raw[sub] - 256 if scales_raw[sub] > 127 else scales_raw[sub]
                    group = (element // 128) * 128
                    r = element - group
                    ql_base = (group // 128) * 64
                    qh_base = (group // 128) * 32
                    if r < 32:
                        byte = ql[ql_base + r]
                        xi = (byte & 0x0F) | (((qh[qh_base + r] >> 0) & 3) << 4)
                    elif r < 64:
                        byte = ql[ql_base + r]
                        xi = (byte & 0x0F) | (((qh[qh_base + r - 32] >> 2) & 3) << 4)
                    elif r < 96:
                        byte = ql[ql_base + r - 64]
                        xi = (byte >> 4) | (((qh[qh_base + r - 64] >> 4) & 3) << 4)
                    else:
                        byte = ql[ql_base + r - 64]
                        xi = (byte >> 4) | (((qh[qh_base + r - 96] >> 6) & 3) << 4)
                    approximated = d_value * sc * (xi - 32)
                    bound = max(abs(v) for v in chunk) + abs(d_value) * abs(sc) * 0.6 + 1e-30
                    self.assertLessEqual(abs(source_values[element] - approximated), bound, f"elemento {element} fuera de la cota Q6_K")
                    checked += 1
                self.assertEqual(checked, 256)
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_fixture_estandar_con_q8_0_es_byte_identico_al_verbatim(self):
        """Ningún tensor del fixture estándar es cuantizable (ne[0] no múltiplo
        de 32): la salida con Q8_0 pedido debe ser idéntica a la verbatim y el
        file_type queda en 0."""
        fixture = packaging.build_safetensors_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q8_0"}
                plan = adapter.call("transformation.plan", request, ctx)
                receipt = adapter.call("transformation.execute", {
                    "plan_id": plan["plan_id"],
                    "plan_hash": plan["plan_hash"],
                    "approval_id": None,
                }, ctx)
                self.assertEqual(receipt["state"], "COMPLETED")
                verified = adapter.call("transformation.verify", {"job_id": receipt["job_id"]}, ctx)
                data_artifact = next(a for a in verified["artifacts"] if a["role"] == "DATA")
                raw = (root / "work" / "output" / data_artifact["relative_path"]).read_bytes()
                parsed = parse_gguf(raw)
                self.assertEqual(parsed["metadata"]["general.file_type"], (0, 4))
                source_tensors, source_data_start = parse_safetensors(fixture)
                for tensor in parsed["tensors"]:
                    self.assertEqual(tensor["type"], GGML_TYPES[source_tensors[tensor["name"]]["dtype"]], "ningún tensor debía cuantizarse")
                self.assertEqual(
                    __import__("hashlib").sha256(raw).hexdigest(),
                    __import__("hashlib").sha256(packaging._conversion_fixture(fixture)).hexdigest(),
                    "la salida con Q8_0 sin cuantizables debe ser idéntica a la verbatim",
                )
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]

    def test_cuantizacion_enum_invalido_y_clave_extra_fallan_cerrado(self):
        fixture = packaging.build_safetensors_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root, adapter, ctx, _contracts = quantized_adapters(directory, fixture)
            try:
                request = adapter.request()
                request["parameters"] = {"quantization": "Q2_K"}
                with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                    adapter.call("transformation.plan", request, ctx)
                self.assertEqual(raised.exception.data["details"]["reason"], "product_gguf_quant_unsupported")
                request["parameters"] = {"quantization": "Q8_0", "extra": 1}
                with self.assertRaises(larm_adapter_sdk.DomainError) as raised:
                    adapter.call("transformation.plan", request, ctx)
                self.assertEqual(raised.exception.data["details"]["reason"], "product_gguf_options_not_supported")
                self.assertFalse(any((root / "work" / "output").iterdir()))
            finally:
                adapter.shutdown()
                sys.path.remove(str(root / "package" / "transformation"))
                for name in [name for name in list(sys.modules) if name in ("adapter", "files", "state", "gguf")]:
                    del sys.modules[name]


if __name__ == "__main__":
    unittest.main()
