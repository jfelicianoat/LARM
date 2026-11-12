#!/usr/bin/env python3
"""Comprueba el recibo de navegador contra SQLite y los hashes actuales.

No vuelve a ejecutar las interacciones ni las convierte en una prueba de
WebView: el recibo procede de la sesión CUA descrita en el informe P043.
"""
import hashlib
import json
import sqlite3
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def main():
    record = json.loads((REPO / "docs/reports/P043_runs/ui-real.json").read_text(encoding="utf-8"))
    assert record["result"] == "PASS"
    assert record["cleanup"] == {"principal_revoked": True, "daemon_exit_code": 0}
    assert [r["status"] for r in record["requests"]] == [201, 200, 204]
    for name, expected in record["source_sha256"].items():
        assert hashlib.sha256((REPO / name).read_bytes()).hexdigest() == expected, name
    db = REPO / record["run"] / "data/state/larm.sqlite"
    with sqlite3.connect(db) as connection:
        assert connection.execute("SELECT count(*) FROM model_source").fetchone()[0] == 0
        assert connection.execute("SELECT resource_version FROM event WHERE resource_type='model_source' ORDER BY event_id").fetchall() == [(1,), (2,), (3,)]
        assert connection.execute("SELECT count(*) FROM event_outbox o JOIN event e ON e.event_id=o.event_id WHERE e.resource_type='model_source'").fetchone()[0] == 3
    print("PASS: recibo de UI real vigente; POST 201, PATCH 200, DELETE 204; SQLite/outbox 1-2-3; credencial revocada y proceso detenido. No acredita WebView nativo.")


if __name__ == "__main__":
    main()
