# -*- coding: utf-8 -*-
"""Edita gguf.py: núcleo Q6_K. Uso único durante la fase Q6_K."""
import io

p = r'adapters/transformation/gguf/gguf.py'
s = io.open(p, encoding='utf-8').read()

# 1) constantes y enum
old = "GGML_TYPE_Q4_K = 12\nGGML_TYPE_Q5_K = 13"
new = "GGML_TYPE_Q4_K = 12\nGGML_TYPE_Q5_K = 13\nGGML_TYPE_Q6_K = 14"
assert s.count(old) == 1
s = s.replace(old, new)

old = "Q5_K_BLOCK_BYTES = 2 + 2 + K_SCALE_SIZE + QK_K // 8 + QK_K // 2  # + bits altos y nibbles 5-bit"
new = (
    "Q5_K_BLOCK_BYTES = 2 + 2 + K_SCALE_SIZE + QK_K // 8 + QK_K // 2  # + bits altos y nibbles 5-bit\n"
    "Q6_K_BLOCK_BYTES = 2 + QK_K // 16 + QK_K // 2 + QK_K // 4  # d f16 + scales int8 + ql + qh"
)
assert s.count(old) == 1
s = s.replace(old, new)

old = 'QUANTIZATIONS = ("Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K")'
new = 'QUANTIZATIONS = ("Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K", "Q6_K")'
assert s.count(old) == 1
s = s.replace(old, new)

# 2) make_qx_quants + quantize_q6_k_block tras q5_k
old = "def quantize_q8_0_block(source_bytes, offset, dtype):"
assert s.count(old) == 1
insert = '''def _make_qx_quants(values, nmax):
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


''' + old
s = s.replace(old, insert, 1)

# 3) regla por tensor: los tres K comparten (ancla de contexto)
old = '''    if quant in ("Q4_K", "Q5_K"):
        total = dims[0]'''
new = '''    if quant in ("Q4_K", "Q5_K", "Q6_K"):
        total = dims[0]'''
assert s.count(old) == 1
s = s.replace(old, new)

# 4) longitud por formato
old = '''            if quant in ("Q4_K", "Q5_K"):
                block_bytes = Q4_K_BLOCK_BYTES if quant == "Q4_K" else Q5_K_BLOCK_BYTES
                length = (elements // QK_K) * block_bytes'''
new = '''            if quant in ("Q4_K", "Q5_K", "Q6_K"):
                block_bytes = {
                    "Q4_K": Q4_K_BLOCK_BYTES,
                    "Q5_K": Q5_K_BLOCK_BYTES,
                }.get(quant, Q6_K_BLOCK_BYTES)
                length = (elements // QK_K) * block_bytes'''
assert s.count(old) == 1
s = s.replace(old, new)

# 5) tipo ggml por formato
old = '''            "ggml_type": {
                "Q4_0": GGML_TYPE_Q4_0,
                "Q5_0": GGML_TYPE_Q5_0,
                "Q4_K": GGML_TYPE_Q4_K,
                "Q5_K": GGML_TYPE_Q5_K,
            }.get(quant, GGML_TYPE_Q8_0)'''
new = '''            "ggml_type": {
                "Q4_0": GGML_TYPE_Q4_0,
                "Q5_0": GGML_TYPE_Q5_0,
                "Q4_K": GGML_TYPE_Q4_K,
                "Q5_K": GGML_TYPE_Q5_K,
                "Q6_K": GGML_TYPE_Q6_K,
            }.get(quant, GGML_TYPE_Q8_0)'''
assert s.count(old) == 1
s = s.replace(old, new)

# 6) despacho del quantizer en stream
old = '''                quantizer = {
                    "Q4_0": quantize_q4_0_block,
                    "Q5_0": quantize_q5_0_block,
                    "Q4_K": quantize_q4_k_block,
                    "Q5_K": quantize_q5_k_block,
                }.get(quant, quantize_q8_0_block)
                block_bytes = {
                    "Q4_0": Q4_0_BLOCK_BYTES,
                    "Q5_0": Q5_0_BLOCK_BYTES,
                    "Q4_K": Q4_K_BLOCK_BYTES,
                    "Q5_K": Q5_K_BLOCK_BYTES,
                }.get(quant, Q8_0_BLOCK_BYTES)
                qk = QK_K if quant in ("Q4_K", "Q5_K") else QK8_0'''
new = '''                quantizer = {
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
                qk = QK_K if quant in ("Q4_K", "Q5_K", "Q6_K") else QK8_0'''
assert s.count(old) == 1
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("núcleo Q6_K escrito")
