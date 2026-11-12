# P007 — CI local, quality gates y fixtures comunes

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 11/11 PASS (3 reales); solo bloqueado por dependencias. [evidence.json](P007_ci-local-quality-gates-y-fixtures-comunes.evidence.json) · [guía de pruebas](../operations/testing.md)

## Qué se construyó

- `tools/check.ps1` definitivo:
  - puertas Rust del workspace y del shell Tauri, arquitectura, copia del paquete y frontend;
  - `-Prompt PXXX -Real` (runner de evidencia del prompt);
  - `-Scenario Gxx`: sin runner sale con 2 (BLOCKED), nunca PASS;
  - `-Coverage`: BLOCKED_TOOLING si falta la herramienta.
- `tools/test-integration.ps1`: objetivos `tests/*.rs` y verificación de fixtures con procesos reales.
- **Fixtures comunes**:
  - base temporal migrada (`larm_db::testing`);
  - servidor HTTP falso (Range/206/416, ETag/304, 429 `Retry-After`, HEAD, latencia, corte a mitad del cuerpo, registro sin secretos);
  - worker JSON-RPC del contrato 03 con 9 modos de fallo;
  - ficheros sparse NTFS y densos.
- **Cobertura** con `cargo-llvm-cov` 0.9.1 fijado e instalado en `tools/.bin`, sin tocar la instalación global; `llvm-tools` declarado en `rust-toolchain.toml`. Línea base: 78,56 % de líneas. El contrato no fija umbral.
- Convención de etiquetas `real-hardware` / `network` / `destructive-disabled`, aplicada por `verify_p007.py tags`.
- `GET /api/v1/jobs/{id}` (operación de P007 en el contrato): ruta genérica con el puerto `JobSource`. El puente P107 pasa a ser una fuente más, en vez de tener la ruta propia; cada fuente filtra por principal (H06).

## Defectos encontrados al verificar

- **`make_sparse` no creaba ficheros sparse**: `truncate()` de Python en Windows rellena con ceros, y un «sparse» de 50 GiB ocupó 50 GB reales (borrado al momento). Ahora se extiende escribiendo el último byte y la herramienta falla si el fichero no queda sparse: 8 GiB lógicos ocupan 64 KiB.
- Carrera en la prueba de cancelación del worker (llega un progreso más tras cancelar): la prueba lee hasta ver ambas respuestas.

## Pendiente

- Los escenarios globales G01–G15 no tienen runner todavía: `-Scenario` los informa como BLOCKED hasta que sus prompts propietarios estén hechos.
