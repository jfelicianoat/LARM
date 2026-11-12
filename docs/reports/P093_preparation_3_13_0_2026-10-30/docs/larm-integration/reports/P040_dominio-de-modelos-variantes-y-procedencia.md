# P040 — Dominio de modelos, variantes y procedencia

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas pasan, incluida la real (6 pasos), pero sus dependencias P003 y P004 no tienen REAL_PASS. [evidence.json](P040_dominio-de-modelos-variantes-y-procedencia.evidence.json) · [operación](../operations/models.md)

## Qué se construyó

- **Dominio** (`larm-domain/src/model.rs`):
  - `CanonicalModel`, `ModelDistribution`, `ModelRevision` y `ArtifactProvenance` según el schema;
  - capacidades como identificadores extensibles: alias del contrato normalizados y desconocidas aceptadas pero sin cualificar;
  - formato, cuantización y arquitectura como identificadores, no enums cerrados;
  - claves de revisión `upstream`, `pending:{UUIDv7}` y `content:{hash}`, con sus reglas de estado;
  - clase de origen de §19: editor oficial, distribución verificada, conversión de la comunidad u origen desconocido.
- **SQLite** (`larm-db/src/models.rs`): altas validadas con la identidad del contrato 01, comprobadas con el bloqueo de escritura y con su evento `model.state_changed` en el outbox de la misma transacción; listas con filtros; procedencia.
- **Registro** (`larm-models/src/registry.rs`):
  - agrupa por `upstream_identity` explícita y nunca por nombre;
  - anota los homónimos como `possible_duplicates` en los dos modelos;
  - es idempotente;
  - rechaza una clave de revisión ya registrada con otro contenido y una publicación que ya es de otro modelo.
- **API de solo lectura**: `GET /models[/{id}]`, `/model-distributions[/{id}]` y `/model-revisions[/{id}]`, con los filtros de su ruta; `openapi.json` regenerado (35 operaciones implementadas).
- **Daemon**: comando `models-import --file`, para alimentar el registro mientras no existan las fuentes (P042).

## Decisiones

- **Sin migración nueva**: las tablas y los índices únicos de identidad ya están en `0001_bootstrap` (el DDL literal del contrato). Una segunda migración, solo de índices de lectura, rompería las pruebas reales de P004, que esperan una única migración. Queda como mejora si el volumen lo pide.
- **Una sola revisión `CURRENT` por distribución**: lo comprueba la base al dar de alta. El contrato no fija transiciones de revisión.
- **Campos de §19 sin columna propia**: autor, uploader, repositorio, URL, gated, modelo base, cuantizador y fecha de descarga van en `metadata` con esos nombres. El schema cerrado no tiene columnas para ellos.

## Pruebas

| Prueba del prompt | Dónde |
|---|---|
| Roundtrip | Dominio (modelo), `revision_roundtrip_y_paginacion`, `procedencia_completa` (todos los campos de §19) |
| Unknown format/capability | Dominio (`audio.diarize` aceptada sin cualificar; `MLC_FUTURE` como formato válido) y real (`unqualified_capabilities`) |
| Multiple distributions same canonical | `varias_distribuciones_un_modelo_canonico` y real (Hugging Face + ModelScope = un modelo) |

**Negativos**:

- schema: campos desconocidos, capacidad mal formada e ids no UUIDv7;
- autorización: 401;
- filtros no previstos en cada ruta: 400;
- estado: revisión `pending:` activada y `PROVISIONAL` sin `pending:`;
- conflictos: clave inmutable, segunda `CURRENT` y publicación de otro modelo;
- procedencia: herramienta sin versión.

**Mutaciones**: cada una se aplicó y se restauró el código. Las 7 hacen fallar al menos una prueba:

1. sin agrupar por upstream;
2. fusión por nombre;
3. revisión mutable;
4. varias `CURRENT`;
5. alias sin normalizar;
6. `pending:` activable;
7. sin `possible_duplicates` en el modelo nuevo.

Esta última no se detectaba al principio: la prueba solo miraba el informe. Se reforzó para comprobar lo guardado.

## Prueba real (`tools/p040_real.py`, 6 pasos PASS)

Se ejecutó con el daemon en el PC IA, sobre una base nueva y con las dos fuentes sembradas con sqlite3 (su API es de P042).

1. Importación multifuente: Qwen-X por Hugging Face y ModelScope queda como un solo modelo con dos distribuciones. El homónimo sin upstream es otro modelo, con `possible_duplicates` en los dos.
2. Reimportar: nada creado y ningún evento nuevo.
3. Cambiar el contenido de una revisión: exit 1 con motivo `revision_key_immutable`, sin escribir.
4. API:
   - listas, detalle y alias `ocr`;
   - distribuciones por fuente y revisiones por estado;
   - metadata original conservada;
   - 404, 400 y 401;
   - 10 eventos en el outbox.

Nota: desde P041 la metadata del modelo incluye además `identity_evidence`. El oráculo de la API se ajustó a comprobar el campo propio (`family`) y esa evidencia, y la prueba real volvió a pasar.

## Pendiente

- **Variantes y artefactos**: dominio y API son de P041.
- **Fuentes reales**: P042.
- **Pantalla de modelos**: la API ya publica lo necesario.
