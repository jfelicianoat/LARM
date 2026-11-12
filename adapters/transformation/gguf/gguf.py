"""Conversor real safetensors → GGUF v3, sólo stdlib.

Lee la cabecera safetensors (JSON + tensores crudos little-endian) y emite un
contenedor GGUF v3 válido: magic, versión, kv de metadatos tipados, infos de
tensor (dims en orden ggml, dtype ggml, offset alineado) y datos de tensor:
verbatim o cuantizados Q8_0 cuando el parámetro lo pide, con alineación a 32
bytes.

Una única ruta de emisión (`stream`) alimenta el preflight (hash sin
escritura) y la conversión (escritura durable con checkpoint por tensor):
ambas pasan exactamente por los mismos bytes.
"""
import json
import math
import struct

GGUF_MAGIC = b"GGUF"
GGUF_VERSION = 3
GGUF_ALIGNMENT = 32
MAX_HEADER_BYTES = 16 << 20
PREFIX_BYTES = 4 + 4 + 8 + 8  # magic + versión + tensor_count + kv_count

# ggml_type (ggml.h): dtypes de safetensors que este conversor copia sin
# recuantizar; los bytes del tensor van verbatim (little-endian en ambos
# contenedores). Con el parámetro de cuantización Q8_0, los tensores F32/F16
# cuyo eje rápido es múltiplo de QK8_0 se emiten como bloques cuantizados.
GGML_TYPE_F32 = 0
GGML_TYPE_F16 = 1
GGML_TYPE_Q8_0 = 8
GGML_TYPE_Q4_0 = 2
GGML_TYPE_Q5_0 = 6
GGML_TYPE_Q4_K = 12
GGML_TYPE_Q5_K = 13
GGML_TYPE_Q6_K = 14
GGML_TYPE_I8 = 16
GGML_TYPE_I16 = 17
GGML_TYPE_I32 = 18
GGML_TYPE_I64 = 26
GGML_TYPE_F64 = 30
GGML_TYPE_BF16 = 31

QK8_0 = 32
Q8_0_BLOCK_BYTES = 2 + QK8_0  # scale f16 + 32 int8
QK4_0 = 32
Q4_0_BLOCK_BYTES = 2 + 16  # scale f16 + 16 bytes de nibbles
QK5_0 = 32
Q5_0_BLOCK_BYTES = 2 + 4 + 16  # scale f16 + uint32 de bits altos + 16 bytes de nibbles
QK_K = 256
K_SCALE_SIZE = 12
Q4_K_BLOCK_BYTES = 2 + 2 + K_SCALE_SIZE + QK_K // 2  # d f16 + dmin f16 + scales + nibbles
Q5_K_BLOCK_BYTES = 2 + 2 + K_SCALE_SIZE + QK_K // 8 + QK_K // 2  # + bits altos y nibbles 5-bit
Q6_K_BLOCK_BYTES = 2 + QK_K // 16 + QK_K // 2 + QK_K // 4  # d f16 + scales int8 + ql + qh

# general.file_type (u32) según la tabla ftype de GGUF: 0 ALL_F32, 7
# MOSTLY_Q8_0 (con al menos un tensor cuantizado).
FILE_TYPE_F32 = 0
FILE_TYPE_Q8_0 = 7

QUANTIZATIONS = ("Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K", "Q6_K")

SAFETENSORS_DTYPES = {
    "F64": GGML_TYPE_F64,
    "F32": GGML_TYPE_F32,
    "F16": GGML_TYPE_F16,
    "BF16": GGML_TYPE_BF16,
    "I64": GGML_TYPE_I64,
    "I32": GGML_TYPE_I32,
    "I16": GGML_TYPE_I16,
    "I8": GGML_TYPE_I8,
}

ELEMENT_BYTES = {"F64": 8, "F32": 4, "F16": 2, "BF16": 2, "I64": 8, "I32": 4, "I16": 2, "I8": 1}

# GGUF metadata value types (gguf.h)
TYPE_I64 = 11
TYPE_STRING = 8
TYPE_U32 = 4

_F32 = struct.Struct("<f")
_F16 = struct.Struct("<e")
_I8 = struct.Struct("<b")


class GGUFError(ValueError):
    pass


def _string(value):
    raw = value.encode("utf-8")
    return struct.pack("<Q", len(raw)) + raw


def _kv_value(value):
    if isinstance(value, tuple):
        inner, kind = value
        if kind == TYPE_U32:
            return struct.pack("<I", TYPE_U32) + struct.pack("<I", inner)
        raise GGUFError("metadata_type_unsupported")
    if not isinstance(value, str):
        raise GGUFError("metadata_type_unsupported")
    return struct.pack("<I", TYPE_STRING) + _string(value)


def metadata_entries(quantized=False):
    """KVs de metadatos del contenedor, en orden de emisión. El file_type y
    el alignment son u32 según la especificación GGUF; el file_type refleja
    si el contenedor lleva tensores Q8_0."""
    return [
        ("general.architecture", "product.test"),
        ("general.name", "larm-gguf-product"),
        ("general.file_type", (FILE_TYPE_Q8_0 if quantized else FILE_TYPE_F32, TYPE_U32)),
        ("general.alignment", (GGUF_ALIGNMENT, TYPE_U32)),
        ("general.source.format", "safetensors"),
    ]


def _f32(value):
    """Redondeo a f32 con un solo redondeamiento: el producto/cociente de dos
    valores f32 exactos es exacto en double; empacar a '<f' redondea una vez.
    Misma semántica que el operador f32 de Rust para operandos f32 exactos."""
    return _F32.unpack(_F32.pack(value))[0]


def _round_half_away(value):
    """roundf de C: mitad away-from-zero sobre el valor f32 exacto."""
    if value >= 0:
        return math.floor(value + 0.5)
    return math.ceil(value - 0.5)


def _nearest_int(fval):
    """nearest_int de ggml: punto mágico — f32(fval+12582912) reinterpretado
    como int32; (i & 0x007fffff) - 0x00400000. Bit a bit idéntico al C."""
    val = _F32.unpack(_F32.pack(fval + 12582912.0))[0]
    (bits,) = struct.unpack("<i", _F32.pack(val))
    return (bits & 0x007FFFFF) - 0x00400000


def _make_qkx2_quants(values, weights, nmax, rmin, rdelta, nstep):
    """Espejo verbatim de make_qkx2_quants de ggml (use_mad=false). Cada
    producto y cada acumulación se redondea a f32 paso a paso, como el C.
    Devuelve (escala, -min, L)."""
    n = len(values)
    minimum = values[0]
    maximum = values[0]
    sum_w = weights[0]
    sum_x = _f32(sum_w * values[0])
    for i in range(1, n):
        if values[i] < minimum:
            minimum = values[i]
        if values[i] > maximum:
            maximum = values[i]
        w = weights[i]
        sum_w = _f32(sum_w + w)
        sum_x = _f32(sum_x + _f32(w * values[i]))
    if minimum > 0:
        minimum = 0.0
    if maximum == minimum:
        return 0.0, -minimum, [0] * n
    iscale = _f32(nmax / _f32(maximum - minimum))
    scale = _f32(1.0 / iscale)
    best_error = 0.0
    L = []
    for i in range(n):
        l = _nearest_int(_f32(iscale * _f32(values[i] - minimum)))
        l = max(0, min(nmax, l))
        L.append(l)
        diff = _f32(_f32(_f32(scale * float(l)) + minimum) - values[i])
        w = weights[i]
        best_error = _f32(best_error + _f32(w * _f32(diff * diff)))
    if nstep < 1:
        return scale, -minimum, L
    Laux = [0] * n
    for is_step in range(nstep + 1):
        iscale = _f32(_f32(rmin + _f32(rdelta * float(is_step)) + float(nmax)) / _f32(maximum - minimum))
        sum_l = 0.0
        sum_l2 = 0.0
        sum_xl = 0.0
        for i in range(n):
            l = _nearest_int(_f32(iscale * _f32(values[i] - minimum)))
            l = max(0, min(nmax, l))
            Laux[i] = l
            w = weights[i]
            sum_l = _f32(sum_l + _f32(w * float(l)))
            sum_l2 = _f32(sum_l2 + _f32(w * _f32(float(l) * float(l))))
            sum_xl = _f32(sum_xl + _f32(w * _f32(_f32(float(l) * values[i]))))
        D = _f32(_f32(sum_w * sum_l2) - _f32(sum_l * sum_l))
        if D > 0:
            this_scale = _f32(_f32(_f32(sum_w * sum_xl) - _f32(sum_x * sum_l)) / D)
            this_min = _f32(_f32(_f32(sum_l2 * sum_x) - _f32(sum_l * sum_xl)) / D)
            if this_min > 0:
                this_min = 0.0
                this_scale = _f32(sum_xl / sum_l2)
            cur_error = 0.0
            for i in range(n):
                diff = _f32(_f32(_f32(this_scale * float(Laux[i])) + this_min) - values[i])
                w = weights[i]
                cur_error = _f32(cur_error + _f32(w * _f32(diff * diff)))
            if cur_error < best_error:
                L = list(Laux)
                best_error = cur_error
                scale = this_scale
                minimum = this_min
    return scale, -minimum, L


def quantize_q4_k_block(source_bytes, offset, dtype):
    """Un super-bloque Q4_K (type 12) desde 256 elementos F32/F16: 144 bytes.
    8 sub-bloques de 32 con escala y mínimo propios (make_qkx2_quants de la
    referencia ggml), escalas de 6 bits empaquetadas en scales[12], y una
    re-cuantización final con la escala del super-bloque. Espejo verbatim del
    C con f32 por operación."""
    fmt = "<f" if dtype == "F32" else "<e"
    values = list(struct.unpack_from("<256" + ("f" if dtype == "F32" else "e"), source_bytes, offset))
    if any(not math.isfinite(value) for value in values):
        raise GGUFError("gguf_quant_nonfinite")
    mins = []
    sub_scales = []
    L_all = []
    max_scale = 0.0
    max_min = 0.0
    for sub in range(8):
        chunk = values[32 * sub:32 * sub + 32]
        sum_x2 = 0.0
        for value in chunk:
            sum_x2 = _f32(sum_x2 + _f32(value * value))
        av_x = _f32(math.sqrt(_f32(sum_x2 / 32.0)))
        weights = [_f32(av_x + (v if v >= 0 else -v)) for v in chunk]
        scale, the_min, L = _make_qkx2_quants(chunk, weights, 15, -1.0, 0.1, 20)
        sub_scales.append(scale)
        mins.append(the_min)
        L_all.extend(L)
        if scale > max_scale:
            max_scale = scale
        if the_min > max_min:
            max_min = the_min
    inv_scale = _f32(63.0 / max_scale) if max_scale > 0 else 0.0
    inv_min = _f32(63.0 / max_min) if max_min > 0 else 0.0
    ls_values = []
    lm_values = []
    for j in range(8):
        ls = min(63, _nearest_int(_f32(inv_scale * sub_scales[j])))
        lm = min(63, _nearest_int(_f32(inv_min * mins[j])))
        ls_values.append(ls)
        lm_values.append(lm)
    scales = [0] * K_SCALE_SIZE
    for j in range(8):
        ls, lm = ls_values[j], lm_values[j]
        if j < 4:
            scales[j] = ls
            scales[j + 4] = lm
        else:
            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4)
            scales[j - 4] |= (ls >> 4) << 6
            scales[j] |= (lm >> 4) << 6
    try:
        d_bytes = _F16.pack(_f32(max_scale / 63.0))
        dmin_bytes = _F16.pack(_f32(max_min / 63.0))
    except OverflowError:
        raise GGUFError("gguf_quant_scale_overflow") from None
    # Re-cuantización final con la escala del super-bloque (f16 → f32).
    d_block = _F16.unpack(d_bytes)[0]
    dmin_block = _F16.unpack(dmin_bytes)[0]
    for j in range(8):
        sc = scales[j] & 63 if j < 4 else (scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)
        m = scales[j + 4] & 63 if j < 4 else (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)
        sub_d = _f32(d_block * float(sc))
        if not sub_d:
            continue
        sub_dm = _f32(dmin_block * float(m))
        for ii in range(32):
            index = 32 * j + ii
            l = _nearest_int(_f32(_f32(values[index] + sub_dm) / sub_d))
            L_all[index] = max(0, min(15, l))
    qs = bytearray(128)
    for j in range(0, 256, 64):
        for l in range(32):
            qs[j // 2 + l] = L_all[j + l] | (L_all[j + l + 32] << 4)
    return d_bytes + dmin_bytes + bytes(scales) + bytes(qs)


def quantize_q5_k_block(source_bytes, offset, dtype):
    """Un super-bloque Q5_K (type 13) desde 256 elementos F32/F16: 176 bytes
    (d f16 + dmin f16 + scales[12] + qh[32] + qs[128]). Misma maquinaria de
    Q4_K con niveles de 5 bits: make_qkx2_quants(32, 31, ..., -0.5, 0.1, 15),
    re-cuantización final con clamp 0..31, byte de nibbles por mitades y el
    5º bit del elemento (n+j)/(n+j+32) en los bits 2·chunk/2·chunk+1 de
    qh[j]. Espejo verbatim de la referencia ggml."""
    fmt = "<f" if dtype == "F32" else "<e"
    values = list(struct.unpack_from("<256" + ("f" if dtype == "F32" else "e"), source_bytes, offset))
    if any(not math.isfinite(value) for value in values):
        raise GGUFError("gguf_quant_nonfinite")
    mins = []
    sub_scales = []
    L_all = []
    max_scale = 0.0
    max_min = 0.0
    for sub in range(8):
        chunk = values[32 * sub:32 * sub + 32]
        sum_x2 = 0.0
        for value in chunk:
            sum_x2 = _f32(sum_x2 + _f32(value * value))
        av_x = _f32(math.sqrt(_f32(sum_x2 / 32.0)))
        weights = [_f32(av_x + (v if v >= 0 else -v)) for v in chunk]
        scale, the_min, L = _make_qkx2_quants(chunk, weights, 31, -0.5, 0.1, 15)
        sub_scales.append(scale)
        mins.append(the_min)
        L_all.extend(L)
        if scale > max_scale:
            max_scale = scale
        if the_min > max_min:
            max_min = the_min
    inv_scale = _f32(63.0 / max_scale) if max_scale > 0 else 0.0
    inv_min = _f32(63.0 / max_min) if max_min > 0 else 0.0
    ls_values = []
    lm_values = []
    for j in range(8):
        ls = min(63, _nearest_int(_f32(inv_scale * sub_scales[j])))
        lm = min(63, _nearest_int(_f32(inv_min * mins[j])))
        ls_values.append(ls)
        lm_values.append(lm)
    scales = [0] * K_SCALE_SIZE
    for j in range(8):
        ls, lm = ls_values[j], lm_values[j]
        if j < 4:
            scales[j] = ls
            scales[j + 4] = lm
        else:
            scales[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4)
            scales[j - 4] |= (ls >> 4) << 6
            scales[j] |= (lm >> 4) << 6
    try:
        d_bytes = _F16.pack(_f32(max_scale / 63.0))
        dmin_bytes = _F16.pack(_f32(max_min / 63.0))
    except OverflowError:
        raise GGUFError("gguf_quant_scale_overflow") from None
    d_block = _F16.unpack(d_bytes)[0]
    dmin_block = _F16.unpack(dmin_bytes)[0]
    for j in range(8):
        sc = scales[j] & 63 if j < 4 else (scales[j + 4] & 0xF) | ((scales[j - 4] >> 6) << 4)
        m = scales[j + 4] & 63 if j < 4 else (scales[j + 4] >> 4) | ((scales[j] >> 6) << 4)
        sub_d = _f32(d_block * float(sc))
        if not sub_d:
            continue
        sub_dm = _f32(dmin_block * float(m))
        for ii in range(32):
            index = 32 * j + ii
            l = _nearest_int(_f32(_f32(values[index] + sub_dm) / sub_d))
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


def _make_qx_quants(values, nmax):
    """Espejo verbatim de make_qx_quants de ggml (rmse_type=1, qw=NULL):
    cuantización simétrica con ajuste por mínimos cuadrados ponderados por
    x². Cada producto y cada acumulación se redondea a f32 paso a paso, como
    el C. Devuelve (escala, L)."""
    n = len(values)
    max_value = 0.0
    amax = 0.0
    for value in values:
        ax = value if value >= 0 else -value
        if ax > amax:
            amax = ax
            max_value = value
    if amax < 1e-15:
        return 0.0, [0] * n
    iscale = _f32(-float(nmax) / max_value)
    L = []
    sumlx = 0.0
    suml2 = 0.0
    for i in range(n):
        l = _nearest_int(_f32(iscale * values[i]))
        l = max(-nmax, min(nmax - 1, l))
        L.append(l + nmax)
        w = _f32(values[i] * values[i])
        sumlx = _f32(sumlx + _f32(w * _f32(_f32(values[i] * float(l)))))
        suml2 = _f32(suml2 + _f32(w * _f32(float(l) * float(l))))
    scale = _f32(sumlx / suml2) if suml2 else 0.0
    best = _f32(scale * sumlx)
    for is_step in range(-9, 10):
        if is_step == 0:
            continue
        iscale = _f32(-_f32(float(nmax) + _f32(0.1 * float(is_step))) / max_value)
        sumlx = 0.0
        suml2 = 0.0
        for i in range(n):
            l = _nearest_int(_f32(iscale * values[i]))
            l = max(-nmax, min(nmax - 1, l))
            w = _f32(values[i] * values[i])
            sumlx = _f32(sumlx + _f32(w * _f32(_f32(values[i] * float(l)))))
            suml2 = _f32(suml2 + _f32(w * _f32(float(l) * float(l))))
        if suml2 > 0 and _f32(sumlx * sumlx) > _f32(best * suml2):
            L = []
            for i in range(n):
                l = _nearest_int(_f32(iscale * values[i]))
                l = max(-nmax, min(nmax - 1, l))
                L.append(l + nmax)
            scale = _f32(sumlx / suml2)
            best = _f32(scale * sumlx)
    return scale, L


def quantize_q6_k_block(source_bytes, offset, dtype):
    """Un super-bloque Q6_K (type 14) desde 256 elementos F32/F16: 210 bytes
    (d f16 + scales int8[16] + ql[128] + qh[64]). 16 sub-bloques de 16
    elementos con cuantización SIMÉTRICA (make_qx_quants, niveles [-32,31]
    desplazados a [0,63]), escalas de 8 bits con signo y empaquetado de 6
    bits. Espejo verbatim de la referencia ggml."""
    fmt = "<f" if dtype == "F32" else "<e"
    values = list(struct.unpack_from("<256" + ("f" if dtype == "F32" else "e"), source_bytes, offset))
    if any(not math.isfinite(value) for value in values):
        raise GGUFError("gguf_quant_nonfinite")
    sub_scales = []
    L_all = []
    max_scale = 0.0
    for sub in range(16):
        chunk = values[16 * sub:16 * sub + 16]
        scale, L = _make_qx_quants(chunk, 32)
        sub_scales.append(scale)
        L_all.extend(L)
        if scale > max_scale:
            max_scale = scale
    try:
        if max_scale < 1e-15:
            d_bytes = _F16.pack(0.0)
            scales = [0] * 16
            L_all = [0] * 256
        else:
            iscale = _f32(-128.0 / max_scale)
            d_bytes = _F16.pack(_f32(1.0 / iscale))
            scales = [min(127, _nearest_int(_f32(iscale * sub_scales[j]))) for j in range(16)]
            # Re-cuantización final con la escala del super-bloque.
            d_block = _F16.unpack(d_bytes)[0]
            for j in range(16):
                d_sub = _f32(d_block * float(scales[j]))
                if not d_sub:
                    continue
                for ii in range(16):
                    index = 16 * j + ii
                    l = _nearest_int(_f32(values[index] / d_sub))
                    L_all[index] = max(-32, min(31, l)) + 32
    except OverflowError:
        raise GGUFError("gguf_quant_scale_overflow") from None
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


def quantize_q8_0_block(source_bytes, offset, dtype):
    """Un bloque Q8_0 desde 32 elementos F32/F16 en `source_bytes[offset:]`.
    d = f16(amax/127 en f32, RNE); id = 1/d en f32 (0 si d==0);
    q[i] = clamp(roundf(v_f32 * id en f32), -127, 127)."""
    fmt = "<f" if dtype == "F32" else "<e"
    values = list(struct.unpack_from("<32" + ("f" if dtype == "F32" else "e"), source_bytes, offset))
    if any(not math.isfinite(value) for value in values):
        # Un bloque con infinito/NaN no tiene escala Q8_0 representable: el
        # conversor falla cerrado en vez de publicar un modelo corrupto.
        raise GGUFError("gguf_quant_nonfinite")
    amax = 0.0
    for value in values:
        magnitude = value if value >= 0 else -value
        if magnitude > amax:
            amax = magnitude
    d = _f32(amax / 127.0)
    idiv = _f32(1.0 / d) if d != 0 else 0.0
    qs = []
    for value in values:
        product = _f32(value * idiv)
        q = _round_half_away(product)
        if q > 127:
            q = 127
        elif q < -127:
            q = -127
        qs.append(q)
    try:
        scale = _F16.pack(d)
    except OverflowError:
        # La escala Q8_0 es f16 por especificación; un amax fuera del rango
        # f16 no tiene representación: falla cerrado, sin bloque truncado.
        raise GGUFError("gguf_quant_scale_overflow") from None
    return scale + struct.pack("<32b", *qs)


def quantize_q4_0_block(source_bytes, offset, dtype):
    """Un bloque Q4_0 (type 2) desde 32 elementos F32/F16: 18 bytes (escala
    f16 + 16 bytes de nibbles). d = f32(max/-8) con `max` el valor CON signo
    de mayor magnitud (empate: el primero); id = 1/d en f32 (0 si d==0);
    xi = min(15, trunc(f32(f32(v*id) + 8.5))) — el cast C trunca hacia cero
    y la suma +8.5 se redondea a f32 antes; nibbles: par=undero, impar=alto."""
    fmt = "<f" if dtype == "F32" else "<e"
    values = list(struct.unpack_from("<32" + ("f" if dtype == "F32" else "e"), source_bytes, offset))
    if any(not math.isfinite(value) for value in values):
        # Un bloque con infinito/NaN no tiene escala Q4_0 representable: el
        # conversor falla cerrado en vez de publicar un modelo corrupto.
        raise GGUFError("gguf_quant_nonfinite")
    max_value = 0.0
    max_magnitude = 0.0
    for value in values:
        magnitude = value if value >= 0 else -value
        if magnitude > max_magnitude:
            max_magnitude = magnitude
            max_value = value
    d = _f32(max_value / -8.0)
    idiv = _f32(1.0 / d) if d != 0 else 0.0
    xis = []
    for value in values:
        product = _f32(_f32(value * idiv) + 8.5)
        xi = math.trunc(product)
        if xi > 15:
            xi = 15
        elif xi < 0:
            xi = 0
        xis.append(xi)
    try:
        scale = _F16.pack(d)
    except OverflowError:
        raise GGUFError("gguf_quant_scale_overflow") from None
    # Empaquetado de MITADES según la referencia real de ggml: el byte j
    # une los elementos j (bajo) y j+16 (alto) del bloque.
    packed = bytearray(16)
    for j in range(16):
        packed[j] = xis[j] | (xis[j + 16] << 4)
    return scale + bytes(packed)


def quantize_q5_0_block(source_bytes, offset, dtype):
    """Un bloque Q5_0 (type 6) desde 32 elementos F32/F16: 22 bytes (escala
    f16 + uint32 LE de bits altos + 16 bytes de nibbles). d = f32(max/-16)
    con `max` el valor CON signo de mayor magnitud; xi = min(31,
    trunc(f32(f32(v*id) + 16.5))) — cast C truncante; byte j de qs une los
    elementos j (bajo) y j+16 (alto); el 5º bit del elemento k va al bit k
    del uint32 qh. Según la referencia real de ggml."""
    fmt = "<f" if dtype == "F32" else "<e"
    values = list(struct.unpack_from("<32" + ("f" if dtype == "F32" else "e"), source_bytes, offset))
    if any(not math.isfinite(value) for value in values):
        raise GGUFError("gguf_quant_nonfinite")
    max_value = 0.0
    max_magnitude = 0.0
    for value in values:
        magnitude = value if value >= 0 else -value
        if magnitude > max_magnitude:
            max_magnitude = magnitude
            max_value = value
    d = _f32(max_value / -16.0)
    idiv = _f32(1.0 / d) if d != 0 else 0.0
    xis = []
    for value in values:
        product = _f32(_f32(value * idiv) + 16.5)
        xi = math.trunc(product)
        if xi > 31:
            xi = 31
        elif xi < 0:
            xi = 0
        xis.append(xi)
    try:
        scale = _F16.pack(d)
    except OverflowError:
        raise GGUFError("gguf_quant_scale_overflow") from None
    packed = bytearray(16)
    qh = [0, 0, 0, 0]
    for j in range(16):
        packed[j] = (xis[j] & 0x0F) | ((xis[j + 16] & 0x0F) << 4)
        qh[j // 8] |= ((xis[j] >> 4) & 1) << (j % 8)
        qh[(j + 16) // 8] |= ((xis[j + 16] >> 4) & 1) << ((j + 16) % 8)
    return scale + bytes(qh) + bytes(packed)


class Safetensors:
    """Cabecera safetensors validada: tensores en orden de fichero."""

    def __init__(self, header_len, header, data_start, data_size):
        self.header_len = header_len
        self.header = header
        self.data_start = data_start
        self.data_size = data_size

    def tensors(self):
        for name, entry in self.header.items():
            if name == "__metadata__":
                continue
            yield name, entry


def read_safetensors(opener, expected_size):
    with opener() as source:
        prefix = source.read(8)
        if len(prefix) != 8:
            raise GGUFError("safetensors_prefix_invalid")
        header_len = struct.unpack("<Q", prefix)[0]
        if header_len == 0 or header_len > MAX_HEADER_BYTES or 8 + header_len > expected_size:
            raise GGUFError("safetensors_header_invalid")
        header_raw = source.read(header_len)
        if len(header_raw) != header_len:
            raise GGUFError("safetensors_header_truncated")
        try:
            header = json.loads(header_raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise GGUFError("safetensors_header_invalid") from error
        if not isinstance(header, dict):
            raise GGUFError("safetensors_header_invalid")
        data_start = 8 + header_len
        data_size = expected_size - data_start
        metadata = header.get("__metadata__")
        if metadata is not None and not isinstance(metadata, dict):
            raise GGUFError("safetensors_metadata_invalid")
        tensors = 0
        for name, entry in header.items():
            if name == "__metadata__":
                continue
            tensors += 1
            if not isinstance(name, str) or not name or not isinstance(entry, dict):
                raise GGUFError("safetensors_tensor_invalid")
            if set(entry) != {"dtype", "shape", "data_offsets"}:
                raise GGUFError("safetensors_tensor_invalid")
            dtype = entry["dtype"]
            if dtype not in SAFETENSORS_DTYPES:
                raise GGUFError("safetensors_dtype_unsupported")
            shape = entry["shape"]
            if (
                not isinstance(shape, list)
                or not 1 <= len(shape) <= 4
                or any(not isinstance(dim, int) or dim < 0 for dim in shape)
            ):
                raise GGUFError("safetensors_shape_invalid")
            elements = 1
            for dim in shape:
                elements *= dim
            offsets = entry["data_offsets"]
            if (
                not isinstance(offsets, list)
                or len(offsets) != 2
                or any(not isinstance(item, int) or item < 0 for item in offsets)
                or offsets[0] > offsets[1]
                or offsets[1] > data_size
                or offsets[1] - offsets[0] != elements * ELEMENT_BYTES[dtype]
            ):
                raise GGUFError("safetensors_offsets_invalid")
        if tensors == 0:
            raise GGUFError("safetensors_tensor_missing")
        return Safetensors(header_len, header, data_start, data_size)


def _quantizable(dtype, dims, quant):
    """Regla de cuantización por tensor: F32/F16 con eje rápido múltiplo de
    QK (32 para los formatos simples; el total de elementos múltiplo de
    QK_K=256 para Q4_K) se cuantiza al formato pedido; todo lo demás va
    verbatim. Determinista y total: nunca rechaza la conversión."""
    if quant not in QUANTIZATIONS or dtype not in ("F32", "F16") or dims[0] <= 0:
        return False
    if quant in ("Q4_K", "Q5_K", "Q6_K"):
        total = dims[0]
        for dim in dims[1:]:
            total *= dim
        return total % QK_K == 0
    return dims[0] % QK8_0 == 0


def _tensor_entries(safetensors, quant=None):
    entries = []
    for name, entry in safetensors.tensors():
        shape = list(entry["shape"])
        dims = list(reversed(shape))  # ggml ne[]: el eje más rápido primero
        dtype = entry["dtype"]
        length = entry["data_offsets"][1] - entry["data_offsets"][0]
        quantized = _quantizable(dtype, dims, quant)
        if quantized:
            elements = length // ELEMENT_BYTES[dtype]
            if quant in ("Q4_K", "Q5_K", "Q6_K"):
                block_bytes = {
                    "Q4_K": Q4_K_BLOCK_BYTES,
                    "Q5_K": Q5_K_BLOCK_BYTES,
                }.get(quant, Q6_K_BLOCK_BYTES)
                length = (elements // QK_K) * block_bytes
            else:
                block_bytes = {
                    "Q4_0": Q4_0_BLOCK_BYTES,
                    "Q5_0": Q5_0_BLOCK_BYTES,
                }.get(quant, Q8_0_BLOCK_BYTES)
                length = (elements // QK8_0) * block_bytes
        entries.append({
            "name": name,
            "dtype": dtype,
            "dims": dims,
            "ggml_type": {
                "Q4_0": GGML_TYPE_Q4_0,
                "Q5_0": GGML_TYPE_Q5_0,
                "Q4_K": GGML_TYPE_Q4_K,
                "Q5_K": GGML_TYPE_Q5_K,
                "Q6_K": GGML_TYPE_Q6_K,
            }.get(quant, GGML_TYPE_Q8_0)
            if quantized
            else SAFETENSORS_DTYPES[dtype],
            "quantized": quantized,
            "length": length,
            "source_length": entry["data_offsets"][1] - entry["data_offsets"][0],
            "source_offset": entry["data_offsets"][0],
        })
    return entries


def layout(safetensors, quant=None):
    """Layout exacto GGUF v3: infos (con offsets reales), padding del header,
    colocación por tensor y tamaño total. Los offsets u64 tienen anchura fija:
    una sola pasada los fija sin dependencia circular."""
    tensors = _tensor_entries(safetensors, quant)
    metadata = metadata_entries(quantized=any(tensor["quantized"] for tensor in tensors))
    # Cuerpo de infos SIN los contadores iniciales: el stream los emite
    # explícitamente antes (PREFIX_BYTES ya los cuenta).
    infos = b""
    for key, value in metadata:
        infos += _string(key) + _kv_value(value)
    offset = 0
    for tensor in tensors:
        tensor["gguf_offset"] = offset
        infos += _string(tensor["name"])
        infos += struct.pack("<I", len(tensor["dims"]))
        for dim in tensor["dims"]:
            infos += struct.pack("<Q", dim)
        infos += struct.pack("<I", tensor["ggml_type"])
        infos += struct.pack("<Q", offset)
        offset += tensor["length"]
        offset += (GGUF_ALIGNMENT - (offset % GGUF_ALIGNMENT)) % GGUF_ALIGNMENT
    if tensors:
        offset -= (GGUF_ALIGNMENT - (tensors[-1]["length"] % GGUF_ALIGNMENT)) % GGUF_ALIGNMENT
    data_size = offset
    header_padding = (GGUF_ALIGNMENT - ((PREFIX_BYTES + len(infos)) % GGUF_ALIGNMENT)) % GGUF_ALIGNMENT
    total = PREFIX_BYTES + len(infos) + header_padding + data_size
    return {
        "tensors": tensors,
        "infos": infos,
        "header_padding": header_padding,
        "data_size": data_size,
        "total": total,
    }


def _read_exact(source, count):
    parts = []
    while count > 0:
        chunk = source.read(count)
        if not chunk:
            raise GGUFError("safetensors_data_truncated")
        parts.append(chunk)
        count -= len(chunk)
    return b"".join(parts)


def stream(safetensors, opener, ctx, sink, checkpoint=None, on_tensor=None, quant=None):
    """Emite el GGUF completo hacia `sink(bytes)` leyendo los tensores del
    safetensors fuente (verbatim o cuantizados Q8_0 según `quant`).
    Devuelve (bytes_emitidos, tensores_emitidos)."""
    plan = layout(safetensors, quant)
    written = 0
    with opener() as source:
        def emit(chunk):
            nonlocal written
            sink(chunk)
            written += len(chunk)

        emit(GGUF_MAGIC)
        emit(struct.pack("<I", GGUF_VERSION))
        emit(struct.pack("<Q", len(plan["tensors"])))
        emit(struct.pack("<Q", len(metadata_entries(quantized=any(tensor["quantized"] for tensor in plan["tensors"])))))
        emit(plan["infos"])
        emit(b"\x00" * plan["header_padding"])
        for tensor in plan["tensors"]:
            ctx.check_cancelled()
            source.seek(safetensors.data_start + tensor["source_offset"])
            remaining = tensor["source_length"]
            if tensor["quantized"]:
                width = ELEMENT_BYTES[tensor["dtype"]]
                # Lecturas alineadas a bloque; cada grupo se cuantiza y se
                # emite como bloques del formato pedido (el sink mantiene
                # checkpoint/progreso por bloque).
                quantizer = {
                    "Q4_0": quantize_q4_0_block,
                    "Q5_0": quantize_q5_0_block,
                    "Q4_K": quantize_q4_k_block,
                    "Q5_K": quantize_q5_k_block,
                    "Q6_K": quantize_q6_k_block,
                }.get(quant, quantize_q8_0_block)
                block_bytes = {
                    "Q4_0": Q4_0_BLOCK_BYTES,
                    "Q5_0": Q5_0_BLOCK_BYTES,
                    "Q4_K": Q4_K_BLOCK_BYTES,
                    "Q5_K": Q5_K_BLOCK_BYTES,
                    "Q6_K": Q6_K_BLOCK_BYTES,
                }.get(quant, Q8_0_BLOCK_BYTES)
                qk = QK_K if quant in ("Q4_K", "Q5_K", "Q6_K") else QK8_0
                group = qk * 4096 * width
                while remaining > 0:
                    take = min(group, remaining)
                    chunk = _read_exact(source, take)
                    remaining -= len(chunk)
                    blocks = bytearray()
                    for offset in range(0, len(chunk), qk * width):
                        blocks += quantizer(chunk, offset, tensor["dtype"])
                    emit(bytes(blocks))
            else:
                while remaining > 0:
                    chunk = source.read(min(48 << 10, remaining))
                    if not chunk:
                        raise GGUFError("safetensors_data_truncated")
                    remaining -= len(chunk)
                    emit(chunk)
            padding = (GGUF_ALIGNMENT - (tensor["length"] % GGUF_ALIGNMENT)) % GGUF_ALIGNMENT
            if padding and tensor is not plan["tensors"][-1]:
                emit(b"\x00" * padding)
            if checkpoint is not None:
                checkpoint(written)
            if on_tensor is not None:
                on_tensor(tensor["name"], written)
        if written != PREFIX_BYTES + len(plan["infos"]) + plan["header_padding"] + plan["data_size"]:
            raise GGUFError("gguf_layout_mismatch")
    return written, len(plan["tensors"])
