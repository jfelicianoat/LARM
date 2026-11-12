# -*- coding: utf-8 -*-
"""Valida la re-derivación Q6_K del auditor contra la evidencia real."""
import json
import struct
import hashlib
import pathlib


def audit_f32(value):
    return struct.unpack("<f", struct.pack("<f", value))[0]


def audit_nearest_int(fval):
    val = struct.unpack("<f", struct.pack("<f", fval + 12582912.0))[0]
    (bits,) = struct.unpack("<i", struct.pack("<f", val))
    return (bits & 0x007FFFFF) - 0x00400000


def audit_make_qx_quants(values, nmax):
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


def main():
    base = pathlib.Path(r"C:\Procesos\Local AI Runtime Manager — LARM\larm")
    n = json.load(open(base / "docs/reports/P093_runs/native-public-gguf-quant.json", encoding="utf-8"))
    quant_dir = pathlib.Path(n["evidence"]) / "public-gguf-quant"
    quant_input = (quant_dir / "input.bin").read_bytes()
    (header_len,) = struct.unpack_from("<Q", quant_input, 0)
    header = json.loads(quant_input[8:8 + header_len].decode("utf-8"))
    source_data_start = 8 + header_len
    artifact = next(a for a in n["report"]["artifacts"] if a["quant_format"] == "Q6_K")
    data = (quant_dir / artifact["file"]).read_bytes()
    assert hashlib.sha256(data).hexdigest() == artifact["sha256"]
    # parseo GGUF mínimo
    raw = data
    assert raw[:4] == b"GGUF"
    off = 24
    metadata = {}
    tensors_count, kvs = struct.unpack_from("<QQ", raw, 8)
    for _ in range(kvs):
        (length,) = struct.unpack_from("<Q", raw, off)
        off += 8
        key = raw[off:off + length].decode()
        off += length
        (kind,) = struct.unpack_from("<I", raw, off)
        off += 4
        if kind == 8:
            (length,) = struct.unpack_from("<Q", raw, off)
            off += 8
            metadata[key] = raw[off:off + length].decode()
            off += length
        else:
            (value,) = struct.unpack_from("<I", raw, off)
            off += 4
            metadata[key] = value
    infos = []
    for _ in range(tensors_count):
        (length,) = struct.unpack_from("<Q", raw, off)
        off += 8
        name = raw[off:off + length].decode()
        off += length
        (n_dims,) = struct.unpack_from("<I", raw, off)
        off += 4
        dims = []
        for _ in range(n_dims):
            (dim,) = struct.unpack_from("<Q", raw, off)
            off += 8
            dims.append(dim)
        (ggml_type,) = struct.unpack_from("<I", raw, off)
        off += 4
        (data_offset,) = struct.unpack_from("<Q", raw, off)
        off += 8
        infos.append((name, dims, ggml_type, data_offset))
    assert metadata["general.file_type"] == 7
    data_begin = (off + 31) // 32 * 32
    for name, dims, ggml_type, offset in infos:
        if ggml_type != 14:
            continue
        entry = header[name]
        src_off, src_end = entry["data_offsets"]
        source_bytes = quant_input[source_data_start + src_off:source_data_start + src_end]
        begin = data_begin + offset
        blocks = len(source_bytes) // (256 * 4)
        observed = data[begin:begin + blocks * 210]
        for b in range(blocks):
            values = list(struct.unpack_from("<256f", source_bytes, b * 1024))
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
            expected = d_bytes_k + bytes(scale_byte & 0xFF for scale_byte in scales16)
            ql_k = bytearray(128)
            qh_k = bytearray(64)
            for j in range(0, 256, 128):
                base_j = j // 128
                for l in range(32):
                    q1 = L_all[j + l] & 0xF
                    q2 = L_all[j + l + 32] & 0xF
                    q3 = L_all[j + l + 64] & 0xF
                    q4 = L_all[j + l + 96] & 0xF
                    ql_k[base_j * 64 + l] = q1 | (q3 << 4)
                    ql_k[base_j * 64 + l + 32] = q2 | (q4 << 4)
                    qh_k[base_j * 32 + l] = (
                        (L_all[j + l] >> 4)
                        | ((L_all[j + l + 32] >> 4) << 2)
                        | ((L_all[j + l + 64] >> 4) << 4)
                        | ((L_all[j + l + 96] >> 4) << 6)
                    )
            expected += bytes(ql_k)
            expected += bytes(qh_k)
            boff = b * 210
            assert observed[boff:boff + 210] == expected, "divergencia en super-bloque %d" % b
        print("Q6_K super-bloques:", blocks, "| ok: True")
    print("AUDITOR Q6_K VALIDADO CONTRA EVIDENCIA REAL")


if __name__ == "__main__":
    main()
