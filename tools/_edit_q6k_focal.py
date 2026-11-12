# -*- coding: utf-8 -*-
"""Edita el oráculo focal: referencia Q6_K + test + rotación de negativo."""
import io

p = r'adapters/transformation/gguf/tests/test_gguf_conversion.py'
s = io.open(p, encoding='utf-8').read()

# 1) referencia independiente Q6_K
old = "def dequantize_q8_0(blocks):"
assert s.count(old) == 1
insert = '''def reference_make_qx_quants(values, nmax):
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


def dequantize_q8_0(blocks):'''
s = s.replace(old, insert, 1)

# 2) negativo rota de Q6_K a Q2_K
old = '''                request["parameters"] = {"quantization": "Q6_K"}'''
assert s.count(old) == 1
s = s.replace(old, '''                request["parameters"] = {"quantization": "Q2_K"}''')

# 3) enums de describe a 6 valores
s = s.replace('"enum": ["Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K"]', '"enum": ["Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K", "Q6_K"]')
s = s.replace('["Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K"],', '["Q8_0", "Q4_0", "Q5_0", "Q4_K", "Q5_K", "Q6_K"],')

# 4) test Q6_K
old = "    def test_fixture_estandar_con_q8_0_es_byte_identico_al_verbatim(self):"
assert s.count(old) == 1
test = '''    def test_cuantizacion_q6_k_publicada_y_verificada_por_referencia_independiente(self):
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

''' + old
s = s.replace(old, test, 1)

io.open(p, 'w', encoding='utf-8', newline='\n').write(s)
print("oráculo focal con Q6_K")
