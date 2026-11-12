#!/usr/bin/env python3
"""Compatibilidad: la preparación parcial fue sustituida por el runner P043.

preparation.checks.json se conserva como recibo histórico, con su fecha/scope.
"""
from p043_evidence import main

if __name__ == "__main__":
    raise SystemExit(main())
