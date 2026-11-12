#!/usr/bin/env python3
"""Fichero sparse (o pequeño) para pruebas de almacenamiento (fixture común, P007).

  python tools/make_sparse.py RUTA TAMAÑO_BYTES [--dense]

En NTFS marca el fichero como sparse (`fsutil sparse setflag`) antes de fijar su tamaño,
de modo que un fichero de decenas de GiB ocupa casi nada en disco. Con --dense escribe
los bytes de verdad (ficheros pequeños con contenido conocido). Imprime JSON con el
tamaño lógico, el tamaño ocupado en disco y si quedó sparse; sale con 2 si se pidió sparse
y el sistema de ficheros no lo admite (no se finge un sparse que no lo es).
"""
import argparse
import ctypes
import ctypes.wintypes as wt
import json
import subprocess
import sys
from pathlib import Path


def on_disk_bytes(path: Path) -> int:
    high = wt.DWORD()
    get = ctypes.windll.kernel32.GetCompressedFileSizeW
    get.restype = wt.DWORD
    low = get(str(path), ctypes.byref(high))
    return (high.value << 32) + low


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("path", type=Path)
    ap.add_argument("size", type=int)
    ap.add_argument("--dense", action="store_true")
    args = ap.parse_args()
    args.path.parent.mkdir(parents=True, exist_ok=True)
    if args.dense:
        with args.path.open("wb") as f:
            remaining = args.size
            pattern = bytes(range(256)) * 4096
            while remaining:
                chunk = pattern[: min(len(pattern), remaining)]
                f.write(chunk)
                remaining -= len(chunk)
        sparse = False
    else:
        args.path.write_bytes(b"")
        flag = subprocess.run(["fsutil", "sparse", "setflag", str(args.path)], capture_output=True, text=True, check=False)
        if flag.returncode != 0:
            print(json.dumps({"error": "sparse no admitido", "detail": (flag.stdout + flag.stderr).strip()[:300]}))
            return 2
        # truncate() en Windows rellena con ceros (asigna el espacio): se extiende
        # escribiendo solo el último byte, y el hueco de un fichero sparse queda sin asignar.
        if args.size:
            with args.path.open("r+b") as f:
                f.seek(args.size - 1)
                f.write(bytes(1))
        sparse = True
        allocated = on_disk_bytes(args.path)
        if args.size >= 1 << 20 and allocated > max(1 << 20, args.size // 100):
            print(json.dumps({"error": "el fichero no quedó sparse", "on_disk_bytes": str(allocated)}))
            args.path.unlink()
            return 2
    print(json.dumps({"path": str(args.path), "size_bytes": str(args.path.stat().st_size),
                      "on_disk_bytes": str(on_disk_bytes(args.path)), "sparse": sparse}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
