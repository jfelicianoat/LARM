# -*- coding: utf-8 -*-
"""Comparación byte a byte de los TypeProto de los outputs L vs R."""
import json
import re
import sys

s = sys.stdin.read()
m = re.search(r"left: \[(.*?)\]\s*right: \[(.*?)\]", s, re.S)
l = json.loads("[" + m.group(1) + "]")
r = json.loads("[" + m.group(2) + "]")


def varint(raw, off):
    res, sh = 0, 0
    while True:
        b = raw[off]
        off += 1
        res |= (b & 0x7F) << sh
        if not b & 0x80:
            return res, off
        sh += 7


def fields(raw, start, end):
    out = []
    off = start
    while off < end:
        key, off = varint(raw, off)
        f, w = key >> 3, key & 7
        if w == 2:
            ln, off = varint(raw, off)
            out.append((f, w, off, ln))
            off += ln
        elif w == 0:
            v, off = varint(raw, off)
            out.append((f, w, v))
    return out


def graph(raw):
    off = 0
    while off < len(raw):
        key, off = varint(raw, off)
        f, w = key >> 3, key & 7
        if w == 2:
            ln, off = varint(raw, off)
            if f == 7:
                return raw[off:off + ln]
            off += ln
        elif w == 0:
            _, off = varint(raw, off)
    return None


glb = graph(l)
grb = graph(r)
glf = fields(glb, 0, len(glb))
grf = fields(grb, 0, len(grb))
ol = [glb[a:a + b] for f, w, a, b in glf if f == 12]
orr = [grb[a:a + b] for f, w, a, b in grf if f == 12]
for i, (a, b) in enumerate(zip(ol, orr)):
    print(f"output {i}: L len {len(a)} R len {len(b)}")
    tp_l = None
    tp_r = None
    off = 0
    while off < len(a):
        key, off = varint(a, off)
        f, w = key >> 3, key & 7
        if w == 2:
            ln, off = varint(a, off)
            if f == 3:
                tp_l = bytes(a[off:off + ln])
            off += ln
        elif w == 0:
            _, off = varint(a, off)
    off = 0
    while off < len(b):
        key, off = varint(b, off)
        f, w = key >> 3, key & 7
        if w == 2:
            ln, off = varint(b, off)
            if f == 3:
                tp_r = bytes(b[off:off + ln])
            off += ln
        elif w == 0:
            _, off = varint(b, off)
    print(f"  TypeProto L ({len(tp_l)}): {tp_l.hex()}")
    print(f"  TypeProto R ({len(tp_r)}): {tp_r.hex()}")

    def walk(raw, label):
        off = 0
        while off < len(raw):
            key, off = varint(raw, off)
            f, w = key >> 3, key & 7
            if w == 2:
                ln, off = varint(raw, off)
                print(f"    {label} field {f} wire2 len {ln}: {raw[off:off+min(ln, 12)].hex()}")
                off += ln
            elif w == 0:
                v, off = varint(raw, off)
                print(f"    {label} field {f} varint {v}")


    walk(tp_l, "L")
    walk(tp_r, "R")
