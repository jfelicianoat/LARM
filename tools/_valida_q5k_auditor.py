# -*- coding: utf-8 -*-
"""Valida la re-derivación Q5_K del auditor contra la evidencia real."""
import json
import math
import struct
import hashlib
import pathlib


def audit_f32(value):
    return struct.unpack("<f", struct.pack("<f", value))[0]


def audit_nearest_int(fval):
    val = struct.unpack("<f", struct.pack("<f", fval + 12582912.0))[0]
    (bits,) = struct.unpack("<i", struct.pack("<f", val))
    return (bits & 0x007FFFFF) - 0x00400000


def audit_make_qkx2_quants(values, weights, nmax=15, rmin=-1.0, rdelta=0.1, nstep=20):
    n = len(values)
    minimum = values[0]
    maximum = values[0]
    sum_w = weights[0]
    sum_x = audit_f32(sum_w * values[0])
    for i in range(1, n):
        if values[i] < minimum:
            minimum = values[i]
        if values[i] > maximum:
            maximum = values[i]
        w = weights[i]
        sum_w = audit_f32(sum_w + w)
        sum_x = audit_f32(sum_x + audit_f32(w * values[i]))
    if minimum > 0:
        minimum = 0.0
    if maximum == minimum:
        return 0.0, -minimum, [0] * n
    iscale = audit_f32(nmax / audit_f32(maximum - minimum))
    scale = audit_f32(1.0 / iscale)
    best_error = 0.0
    L = []
    for i in range(n):
        l = audit_nearest_int(audit_f32(iscale * audit_f32(values[i] - minimum)))
        l = max(0, min(nmax, l))
        L.append(l)
        diff = audit_f32(audit_f32(audit_f32(scale * float(l)) + minimum) - values[i])
        best_error = audit_f32(best_error + audit_f32(weights[i] * audit_f32(diff * diff)))
    Laux = [0] * n
    for is_step in range(nstep + 1):
        iscale = audit_f32(audit_f32(rmin + audit_f32(rdelta * float(is_step)) + float(nmax)) / audit_f32(maximum - minimum))
        sum_l = sum_l2 = sum_xl = 0.0
        for i in range(n):
            l = audit_nearest_int(audit_f32(iscale * audit_f32(values[i] - minimum)))
            l = max(0, min(nmax, l))
            Laux[i] = l
            w = weights[i]
            sum_l = audit_f32(sum_l + audit_f32(w * float(l)))
            sum_l2 = audit_f32(sum_l2 + audit_f32(w * audit_f32(float(l) * float(l))))
            sum_xl = audit_f32(sum_xl + audit_f32(w * audit_f32(audit_f32(float(l) * values[i]))))
        D = audit_f32(audit_f32(sum_w * sum_l2) - audit_f32(sum_l * sum_l))
        if D > 0:
            this_scale = audit_f32(audit_f32(audit_f32(sum_w * sum_xl) - audit_f32(sum_x * sum_l)) / D)
            this_min = audit_f32(audit_f32(audit_f32(sum_l2 * sum_x) - audit_f32(sum_l * sum_xl)) / D)
            if this_min > 0:
                this_min = 0.0
                this_scale = audit_f32(sum_xl / sum_l2)
            cur_error = 0.0
            for i in range(n):
                diff = audit_f32(audit_f32(audit_f32(this_scale * float(Laux[i])) + this_min) - values[i])
                cur_error = audit_f32(cur_error + audit_f32(weights[i] * audit_f32(diff * diff)))
            if cur_error < best_error:
                L = list(Laux)
                best_error = cur_error
                scale = this_scale
                minimum = this_min
    return scale, -minimum, L


def main():
    base = pathlib.Path(r"C:\Procesos\Local AI Runtime Manager — LARM\larm")
    n = json.load(open(base / "docs/reports/P093_runs/native-public-gguf-quant.json", encoding="utf-8"))
    quant_dir = pathlib.Path(n["evidence"]) / "public-gguf-quant"
    quant_input = (quant_dir / "input.bin").read_bytes()
    (header_len,) = struct.unpack_from("<Q", quant_input, 0)
    header = json.loads(quant_input[8:8 + header_len].decode("utf-8"))
    source_data_start = 8 + header_len
    artifact = next(a for a in n["report"]["artifacts"] if a["quant_format"] == "Q5_K")
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
        if ggml_type != 13:
            continue
        entry = header[name]
        src_off, src_end = entry["data_offsets"]
        source_bytes = quant_input[source_data_start + src_off:source_data_start + src_end]
        begin = data_begin + offset
        blocks = len(source_bytes) // (256 * 4)
        observed = data[begin:begin + blocks * 176]
        for b in range(blocks):
            values = list(struct.unpack_from("<256f", source_bytes, b * 1024))
            mins = []
            sub_scales = []
            L_all = [0] * 256
            max_scale = 0.0
            max_min = 0.0
            for sub in range(8):
                chunk = values[32 * sub:32 * sub + 32]
                sum_x2 = 0.0
                for v in chunk:
                    sum_x2 = audit_f32(sum_x2 + audit_f32(v * v))
                av_x = audit_f32(math.sqrt(audit_f32(sum_x2 / 32.0)))
                weights = [audit_f32(av_x + abs(v)) for v in chunk]
                scale, the_min, levels = audit_make_qkx2_quants(chunk, weights, nmax=31, rmin=-0.5, nstep=15)
                sub_scales.append(scale)
                mins.append(the_min)
                L_all[32 * sub:32 * sub + 32] = levels
                if scale > max_scale:
                    max_scale = scale
                if the_min > max_min:
                    max_min = the_min
            inv_scale = audit_f32(63.0 / max_scale) if max_scale > 0 else 0.0
            inv_min = audit_f32(63.0 / max_min) if max_min > 0 else 0.0
            ls_values = [min(63, audit_nearest_int(audit_f32(inv_scale * sub_scales[j]))) for j in range(8)]
            lm_values = [min(63, audit_nearest_int(audit_f32(inv_min * mins[j]))) for j in range(8)]
            scales12 = [0] * 12
            for j in range(8):
                ls, lm = ls_values[j], lm_values[j]
                if j < 4:
                    scales12[j] = ls
                    scales12[j + 4] = lm
                else:
                    scales12[j + 4] = (ls & 0xF) | ((lm & 0xF) << 4)
                    scales12[j - 4] |= (ls >> 4) << 6
                    scales12[j] |= (lm >> 4) << 6
            d_bytes_k = struct.pack("<e", audit_f32(max_scale / 63.0))
            dmin_bytes_k = struct.pack("<e", audit_f32(max_min / 63.0))
            d_block = struct.unpack("<e", d_bytes_k)[0]
            dmin_block = struct.unpack("<e", dmin_bytes_k)[0]
            for j in range(8):
                if j < 4:
                    sc = scales12[j] & 63
                    m = scales12[j + 4] & 63
                else:
                    sc = (scales12[j + 4] & 0xF) | ((scales12[j - 4] >> 6) << 4)
                    m = (scales12[j + 4] >> 4) | ((scales12[j] >> 6) << 4)
                sub_d = audit_f32(d_block * float(sc))
                if not sub_d:
                    continue
                sub_dm = audit_f32(dmin_block * float(m))
                for ii in range(32):
                    index = 32 * j + ii
                    l = audit_nearest_int(audit_f32(audit_f32(values[index] + sub_dm) / sub_d))
                    L_all[index] = max(0, min(31, l))
            expected = d_bytes_k + dmin_bytes_k + bytes(scales12)
            qh_k = bytearray(32)
            qs_k = bytearray(128)
            m1 = 1
            m2 = 2
            cursor = 0
            for n2 in range(0, 256, 64):
                for j in range(32):
                    l1 = L_all[n2 + j]
                    if l1 > 15:
                        l1 -= 16
                        qh_k[j] |= m1
                    l2 = L_all[n2 + j + 32]
                    if l2 > 15:
                        l2 -= 16
                        qh_k[j] |= m2
                    qs_k[cursor + j] = l1 | (l2 << 4)
                cursor += 32
                m1 <<= 2
                m2 <<= 2
            expected += bytes(qh_k)
            expected += bytes(qs_k)
            boff = b * 176
            assert observed[boff:boff + 176] == expected, "divergencia en super-bloque %d" % b
        print("Q5_K super-bloques:", blocks, "| ok: True")
    print("AUDITOR Q5_K VALIDADO CONTRA EVIDENCIA REAL")


if __name__ == "__main__":
    main()
