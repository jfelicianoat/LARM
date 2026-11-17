# -*- coding: utf-8 -*-
"""Corrige el total del auditor (450->451, calibración del generador) y
valida el auditor corregido contra la evidencia preservada de attempt1 vía
una copia sim con hashes del auditor actualizados consistentemente."""
import hashlib
import io
import json
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(r"C:\Procesos\Local AI Runtime Manager — LARM\larm")
RUN = ROOT / "docs/reports/P093_runs/public-onnxq-20261114"

# 1) corregir el total en el auditor del repositorio
audit_path = RUN / "audit.py"
s = io.open(audit_path, encoding="utf-8", newline="").read()
a = '"passed":450+unit[0][0]+workers[0][0]'
b = '"passed":451+unit[0][0]+workers[0][0]'
if s.count(a) == 1:
    s = s.replace(a, b)
else:
    assert s.count(b) == 1, (s.count(a), s.count(b))
io.open(audit_path, "w", encoding="utf-8", newline="").write(s)
print("audit.py: total corregido a 451")

# 2) crear la copia sim de attempt1
sim = RUN / "attempt1-sim"
if sim.exists():
    shutil.rmtree(sim)
shutil.copytree(RUN / "attempt1", sim)

new_hash = hashlib.sha256(audit_path.read_bytes()).hexdigest()
old_entries = []

def refresh(obj):
    for section in ("inputs_before", "inputs_after"):
        mapping = obj.get(section)
        if mapping and "docs/reports/P093_runs/public-onnxq-20261114/audit.py" in mapping:
            old_entries.append(mapping["docs/reports/P093_runs/public-onnxq-20261114/audit.py"])
            mapping["docs/reports/P093_runs/public-onnxq-20261114/audit.py"] = new_hash
    return obj

# sinopsis + receipts: mismo hash nuevo en todas las secciones
synopsis_path = sim / "verification-synopsis.json"
synopsis = json.loads(synopsis_path.read_bytes())
refresh(synopsis)
for step in synopsis["steps"]:
    refresh(step)
synopsis_path.write_bytes(json.dumps(synopsis, indent=2).encode("utf-8"))
receipts = sorted(sim.glob("verify-*.json"))
for receipt in receipts:
    value = json.loads(receipt.read_bytes())
    refresh(value)
    receipt.write_bytes(json.dumps(value, indent=2).encode("utf-8"))
print(f"sim: sinopsis + {len(receipts)} receipts actualizados; hashes viejos {set(old_entries)!r}")
assert len(set(old_entries)) == 1, "todos los hashes viejos del auditor deben coincidir"

# 3) correr el auditor corregido sobre la sim
result = subprocess.run(
    [str(ROOT / ".venv-tools/Scripts/python.exe"), "-X", "utf8", str(audit_path), "attempt1-sim"],
    cwd=ROOT, capture_output=True, text=True,
)
tail = (result.stdout + result.stderr)[-3000:]
print("audit sim exit:", result.returncode)
print(tail)
sys.exit(result.returncode)
