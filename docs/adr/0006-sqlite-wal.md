# ADR-0006 — SQLite con WAL como única base de datos

**Estado**: aceptado · **Fecha**: 2026-10-10 · **Prompt**: P002

## Contexto
La spec fija SQLite + SQLx con WAL y foreign keys; una sola máquina, un solo usuario operativo.

## Decisión
- SQLite en modo WAL con foreign_keys=ON; migraciones transaccionales gestionadas por `larm-db` (P004).
- Todo estado durable pasa por la DB + journal/outbox; los archivos complementarios (registry JSON, reportes) son derivados reconstruibles (P098).
- No se introduce otra base de datos salvo cambio de spec.

## Consecuencias
- Backup = copiar DB + store content-addressed; la recuperación sin DB es requisito de P098.
