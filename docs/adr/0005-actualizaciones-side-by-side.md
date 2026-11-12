# ADR-0005 — Actualización side-by-side de runtimes

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
Los runtimes evolucionan rápido; la spec exige conservar la versión anterior, promoción explícita, rollback y pinning por lease (P032, escenario G01).

## Decisión
- Cada versión instalada vive en su propio directorio; la promoción a default es una transición de estado registrada, nunca un reemplazo destructivo.
- Los leases fijan versión (pinning) y sobreviven a la instalación de nuevas versiones.
- La desinstalación exige que ninguna instancia activa use la versión.

## Consecuencias
- Coste de disco por versiones coexistentes; aceptado.
