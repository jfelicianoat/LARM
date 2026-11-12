#!/usr/bin/env python3
"""Compatibilidad: el antiguo runner parcial ahora ejecuta la evidencia completa.

El recibo preparation.checks.json anterior permanece como evidencia histórica;
no se sobrescribe ni se usa para acreditar el estado actual.
"""
from p025_evidence import main


if __name__ == "__main__":
    raise SystemExit(main())
