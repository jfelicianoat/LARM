# -*- coding: utf-8 -*-
"""Añade la rama Q6_K al auditor congelado. Uso único durante la fase Q6_K."""
import io

p = r'docs/reports/P093_runs/public-gguf-quant-20261108/audit.py'
s = io.open(p, encoding='utf-8').read()

# 1) make_qx_quants de referencia para el auditor
old = "def quant_parse_gguf(raw):"
assert s.count(old) == 1
insert = '''def audit_make_qx_quants(values, nmax):
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
    iscale = audit_f32(-float(nmax) / max_value)
    L = []
    sumlx = 0.0
    suml2 = 0.0
    for i in range(n):
        l = audit_nearest_int(audit_f32(iscale * values[i]))
        l = max(-nmax, min(nmax - 1, l))
        L.append(l + nmax)
        w = audit_f32(values[i] * values[i])
        sumlx = audit_f32(sumlx + audit_f32(w * audit_f32(audit_f32(values[i] * float(l)))))
        suml2 = audit_f32(suml2 + audit_f32(w * audit_f32(float(l) * float(l))))
    scale = audit_f32(sumlx / suml2) if suml2 else 0.0
    best = audit_f32(scale * sumlx)
    for is_step in range(-9, 10):
        if is_step == 0:
            continue
        iscale = audit_f32(-audit_f32(float(nmax) + audit_f32(0.1 * float(is_step))) / max_value)
        sumlx = 0.0
        suml2 = 0.0
        for i in range(n):
            l = audit_nearest_int(audit_f32(iscale * values[i]))
            l = max(-nmax, min(nmax - 1, l))
            w = audit_f32(values[i] * values[i])
            sumlx = audit_f32(sumlx + audit_f32(w * audit_f32(audit_f32(values[i] * float(l)))))
            suml2 = audit_f32(suml2 + audit_f32(w * audit_f32(float(l) * float(l))))
        if suml2 > 0 and audit_f32(sumlx * sumlx) > audit_f32(best * suml2):
            L = []
            for i in range(n):
                l = audit_nearest_int(audit_f32(iscale * values[i]))
                l = max(-nmax, min(nmax - 1, l))
                L.append(l + nmax)
            scale = audit_f32(sumlx / suml2)
            best = audit_f32(scale * sumlx)
    return scale, L


def quant_parse_gguf(raw):'''
s = s.replace(old, insert, 1)

# 2) rama Q6_K de re-derivación, tras la de Q5_K
old = "                elif ggml_type == 12:"
assert s.count(old) == 1
insert = '''                elif ggml_type == 14:
                    # Super-bloque Q6_K: cuantización SIMÉTRICA (make_qx_quants
                    # con niveles [-32,31] desplazados a [0,63]), 16
                    # sub-bloques de 16 con escalas de 8 bits con signo.
                    sub_scales = []
                    L_all = [0] * 256
                    max_scale = 0.0
                    for sub in range(16):
                        chunk = values[16 * sub:16 * sub + 16]
                        scale, levels = audit_make_qx_quants(chunk, 32)
                        sub_scales.append(scale)
                        L_all[16 * sub:16 * sub + 16] = levels
                        if scale > max_scale:
                            max_scale = scale
                    if max_scale < 1e-15:
                        d_bytes_k = struct.pack("<e", 0.0)
                        scales16 = [0] * 16
                        L_all = [0] * 256
                    else:
                        iscale = audit_f32(-128.0 / max_scale)
                        d_bytes_k = struct.pack("<e", audit_f32(1.0 / iscale))
                        scales16 = [min(127, audit_nearest_int(audit_f32(iscale * sub_scales[j]))) for j in range(16)]
                        d_block = struct.unpack("<e", d_bytes_k)[0]
                        for j in range(16):
                            d_sub = audit_f32(d_block * float(scales16[j]))
                            if not d_sub:
                                continue
                            for ii in range(16):
                                index = 16 * j + ii
                                l = audit_nearest_int(audit_f32(values[index] / d_sub))
                                L_all[index] = max(-32, min(31, l)) + 32
                    expected_blocks += d_bytes_k
                    expected_blocks += bytes(scale_byte & 0xFF for scale_byte in scales16)
                    ql_k = bytearray(128)
                    qh_k = bytearray(64)
                    for j in range(0, 256, 128):
                        base = j // 128
                        for l in range(32):
                            q1 = L_all[j + l] & 0xF
                            q2 = L_all[j + l + 32] & 0xF
                            q3 = L_all[j + l + 64] & 0xF
                            q4 = L_all[j + l + 96] & 0xF
                            ql_k[base * 64 + l] = q1 | (q3 << 4)
                            ql_k[base * 64 + l + 32] = q2 | (q4 << 4)
                            qh_k[base * 32 + l] = (
                                (L_all[j + l] >> 4)
                                | ((L_all[j + l + 32] >> 4) << 2)
                                | ((L_all[j + l + 64] >> 4) << 4)
                                | ((L_all[j + l + 96] >> 4) << 6)
                            )
                    expected_blocks += bytes(ql_k)
                    expected_blocks += bytes(qh_k)
                elif ggml_type == 12:'''
s = s.replace(old, insert, 1)

# 3) cierre: formatos y conteo
old = '''assert quant_formats_verified == {8, 2, 6, 12, 13}, quant_formats_verified
assert quant_blocks_verified == 98304 * 3 + 12288 * 2'''
new = '''assert quant_formats_verified == {8, 2, 6, 12, 13, 14}, quant_formats_verified
assert quant_blocks_verified == 98304 * 3 + 12288 * 3'''
assert s.count(old) == 1
s = s.replace(old, new)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("auditor con rama Q6_K")
