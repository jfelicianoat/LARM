# P090 — Motor de compatibilidad y puertas duras

Fecha: 2026-10-22  
Estado: `BLOCKED_REAL`

P090 materializa el registro contractual de compatibilidad y un evaluador puro que decide las hard gates antes de cualquier score. Un gate obligatorio ausente queda `UNKNOWN`; un `FAIL` produce `INCOMPATIBLE`; y sólo el conjunto completo en `PASS` puede producir `COMPATIBLE` o `COMPATIBLE_WITH_LIMITATIONS`. P090 deja `technical_fit` nulo para todos los resultados, pues el cálculo pertenece a P091.

## Cambios

- [compatibility.rs de dominio](../../crates/larm-domain/src/compatibility.rs) define `CompatibilityRecord`, estados y gates con campos exactos del schema v3, sin `engine_version` ni campos alternativos. La validación exige UUIDv7, timestamps UTC con milisegundos, SHA-256, gates obligatorios únicos y evidencia para resultados `PASS`/`FAIL`.
- [compatibility.rs de core](../../crates/larm-core/src/compatibility.rs) combina observaciones de forma determinista y conservadora (`FAIL > UNKNOWN > PASS`), añade gates obligatorios sin evidencia como `UNKNOWN`, preserva referencias y limitaciones de forma ordenada, materializa el registro contractual y expone una clave de caché ligada al tuple completo: variante, runtime version, backend, huella de hardware y hash de perfil.
- [CompatibilityRepository](../../crates/larm-domain/src/ports.rs), [repositorio SQLite](../../crates/larm-db/src/compatibility.rs) y [ruta API](../../crates/larm-api/src/compatibility.rs) implementan la lectura contractual `GET /api/v1/compatibility`; el daemon la conecta a la tabla existente. El endpoint devuelve `items`, `next_cursor` nulo y `snapshot_at`. OpenAPI marca la ruta implementada en el [documento generado](../../docs/api/openapi.json).
- La evaluación aún no escribe filas: el contrato actual solo publica esta operación como lectura y no se inventó una operación de mutación.

## Validación

La evidencia focal registra 10 PASS, 0 FAIL y 1 prueba real BLOCKED. Pasan los tres tests del DTO, diez tests del motor, la ruta HTTP, la lectura de una base SQLite migrada, los casos válidos e inválidos contra el schema canónico, Clippy focal, formato, OpenAPI y arquitectura. Los casos del motor cubren formato incompatible, arquitectura desconocida, backend ausente, memoria insuficiente, bundle incompleto, puertas ausentes, conflictos, determinismo, score nulo y cambios en la clave de caché.

La puerta común `tools/check.ps1 -Prompt P090 -Real -SkipFrontend -SkipDesktop` terminó con 6/7 checks PASS: `cargo_fmt`, Clippy global, fixtures, suite completa `cargo test --workspace`, arquitectura y runner P090 pasaron. El único FAIL es `package_copy`, que sigue detectando las tres divergencias locales previas en `adapter-methods.json`, `api-operations.json` y `schemas/larm.schema.json`; no son cambios introducidos por P090. El recibo PromptEvidence se validó contra el schema v3.

El test etiquetado REAL ejecuta los fixtures del motor en este host Windows. No constituye una cualificación de una combinación real modelo/runtime/backend. La prueba real de admisión está registrada como BLOCKED porque no están conectados los probes de versión/support matrix, hardware/bundle, perfiles ni admisión; las dependencias obligatorias P013, P035, P041, P030 y P019 tampoco tienen `REAL_PASS`.

## Pendiente

Para cerrar P090 faltan la composición de evidencias reales y su invalidación enlazada al ciclo de vida de las dimensiones, la persistencia duradera de evaluaciones producidas por el motor y una prueba con modelo/backend cualificados en Windows. P091 calculará `technical_fit` únicamente después de que todas las puertas requeridas pasen. Por contrato 00, P090 permanece `BLOCKED_REAL` hasta completar esas integraciones y satisfacer las dependencias.

El recibo formal está en [P090 PromptEvidence](P090_compatibility-engine-hard-gates.evidence.json); la puerta común queda en [check_P090_real.json](check_P090_real.json).
