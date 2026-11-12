# ADR-0004 — Multidisco con identidad de volumen y store content-addressed

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
Los modelos pesan cientos de GB; la spec exige Volume Identity por GUID, reservas por volumen y deduplicación por hash de contenido.

## Decisión
- Toda ubicación de artefacto se ancla al **GUID del volumen** (no a letras de unidad), gestionado por `larm-storage` (P012/P014/P016).
- El contenido se almacena content-addressed (SHA-256) con deduplicación (P017); los movimientos son journalizados y verificables (P016).
- Las reservas de espacio se registran por volumen en un journal transaccional (P019).

## Consecuencias
- Los escenarios G04/G05 exigen un segundo volumen real; en un host de un solo volumen quedan BLOCKED_REAL (registrado en P000).
