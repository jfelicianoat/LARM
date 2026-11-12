#!/usr/bin/env python3
"""P019 - Prueba real del Resource Guard con procesos y ficheros reales en el PC IA.

  python tools/p019_real.py concurrencia

Un fichero de 8 MiB alterna entre dos ubicaciones: una carpeta de prueba en C: y otra
dentro del recurso existente \\\\localhost\\Temp (dos volume_key distintos; el mismo disco
físico, porque el PC IA no tiene un segundo volumen). Sobre una base SQLite real:

  1. 30 lectores (procesos) con lock compartido y 6 movedores con lock exclusivo, a la vez:
     ningún intervalo de lectura se solapa con uno de movimiento; cada lector ve el fichero
     entero en exactamente un sitio y con el hash original; el hash final es el original;
  2. crash con el lock tomado (proceso que sale tras adquirir, journal en LOCKED): la
     reconciliación lo conserva; cuando el journal pasa a FAILED, lo libera;
  3. al final no queda ningún lock.
"""
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DAEMON = REPO / "target/debug/larm-daemon.exe"
WORKER = REPO / "target/debug/examples/guard_worker.exe"
RUNS = REPO / "docs/reports/P019_runs"


def step(log: list, name: str, ok: bool, detail: str) -> None:
    log.append({"step": name, "result": "PASS" if ok else "FAIL", "detail": detail})
    print(f"  [{'PASS' if ok else 'FAIL'}] {name}: {detail}")
    if not ok:
        raise AssertionError(f"{name}: {detail}")


def worker(db: Path, *args: str) -> dict:
    p = subprocess.run([str(WORKER), str(db), *args], capture_output=True, timeout=300)
    if p.returncode not in (0, 9):
        raise AssertionError(f"{args}: exit {p.returncode}: {p.stderr.decode('utf-8', 'replace')[-400:]}")
    out = p.stdout.decode("utf-8").strip()
    return json.loads(out) if out else {"exit": p.returncode}


def run() -> str:
    run_dir = RUNS / "real"
    shutil.rmtree(run_dir, ignore_errors=True)
    data = run_dir / "data"
    subprocess.run([str(DAEMON), "--data-root", str(data), "db-info"], capture_output=True, check=True,
                   env={k: v for k, v in os.environ.items() if not k.startswith("LARM_")})
    db = data / "state/larm.sqlite"
    a_dir = run_dir / "c"
    a_dir.mkdir(parents=True)
    b_dir = Path(r"\\localhost\Temp") / f"larm-p019-{uuid.uuid4().hex[:8]}"
    b_dir.mkdir()
    a, b = a_dir / "modelo.bin", b_dir / "modelo.bin"
    payload = os.urandom(8 << 20)
    a.write_bytes(payload)
    original = hashlib.sha256(payload).hexdigest()
    log: list = []
    try:
        jobs = [("read", f"lector-{i}") for i in range(30)] + [("move", f"movedor-{i}") for i in range(6)]
        jobs = sorted(jobs, key=lambda j: hash(j[1]))  # mezcla estable
        with ThreadPoolExecutor(max_workers=16) as pool:
            results = list(pool.map(lambda j: worker(db, j[0], j[1], str(a), str(b)), jobs))
        (RUNS / "intervalos.json").write_text(json.dumps(results, indent=1), encoding="utf-8")
        reads = [r for r in results if r["kind"] == "read"]
        moves = [r for r in results if r["kind"] == "move"]
        overlaps = [(r["owner"], m["owner"]) for r in reads for m in moves
                    if int(r["start"]) < int(m["end"]) and int(m["start"]) < int(r["end"])]
        mm = [(x["owner"], y["owner"]) for x in moves for y in moves if x is not y
              and int(x["start"]) < int(y["end"]) and int(y["start"]) < int(x["end"])]
        step(log, "sin-solapes", not overlaps and not mm and len(reads) == 30 and len(moves) == 6,
             f"30 lecturas y 6 movimientos en procesos distintos: 0 solapes lector/movedor y 0 entre movedores")
        bad = [r for r in reads if not r["found"] or r["hash"] != original]
        step(log, "lectores-ven-un-fichero-entero", not bad,
             f"cada lector vio el fichero en exactamente un sitio y con el hash original ({original[:12]}…)")
        final = [p for p in (a, b) if p.exists()]
        step(log, "hash-final", len(final) == 1 and hashlib.sha256(final[0].read_bytes()).hexdigest() == original
             and all(m["hash"] == original for m in moves),
             f"tras 6 movimientos C: ↔ UNC el fichero está en {final[0].parent.name if final else '-'} con el hash original")
        retries = sum(r["attempts"] - 1 for r in results)
        step(log, "hubo-contencion", retries > 0, f"{retries} reintentos por recurso ocupado: la carrera fue real")

        crashed = worker(db, "move", "caido", str(a), str(b), "--crash-after-lock")
        rec1 = worker(db, "reconcile")
        with sqlite3.connect(db) as c:
            phase = c.execute("SELECT phase FROM operation_journal WHERE id = 'caido'").fetchone()[0]
            c.execute("UPDATE operation_journal SET phase = 'FAILED' WHERE id = 'caido'")
        rec2 = worker(db, "reconcile")
        step(log, "crash-y-reconciliacion", crashed.get("exit") == 9 and phase == "LOCKED" and rec1["kept"] == ["caido"]
             and rec2["released"] == ["caido"],
             "proceso muerto con el lock: journal LOCKED -> se conserva; journal FAILED -> se libera (sin timeout)")
        with sqlite3.connect(db) as c:
            left = c.execute("SELECT COUNT(*) FROM resource_lock").fetchone()[0]
        step(log, "sin-locks-al-final", left == 0, f"{left} locks en resource_lock")
    finally:
        shutil.rmtree(b_dir, ignore_errors=True)
        RUNS.mkdir(parents=True, exist_ok=True)
        (RUNS / "steps.json").write_text(json.dumps(log, indent=2, ensure_ascii=False), encoding="utf-8")
    return f"{len(log)} pasos PASS"


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    try:
        print("PASS " + {"concurrencia": run}[sys.argv[1]]())
        raise SystemExit(0)
    except (AssertionError, KeyError, OSError, ValueError, subprocess.SubprocessError, sqlite3.Error) as e:
        print(f"FAIL {type(e).__name__}: {e}")
        raise SystemExit(1)
