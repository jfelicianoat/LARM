# Pruebas de LARM

P007 · spec §142 · contrato 00 (CODE_PASS ≠ REAL_PASS; un mock no sustituye a una prueba real).

## Comandos

| Comando | Qué hace |
|---|---|
| `tools/check.ps1` | Puertas comunes: fmt, clippy `-D warnings`, `cargo test` del workspace y del shell Tauri, arquitectura, copia del paquete, lint/typecheck/test/build del frontend. |
| `tools/check.ps1 -Prompt PXXX -Real` | Además ejecuta `tools/pxxx_evidence.py`: todas las pruebas del prompt, reales incluidas, y escribe su `evidence.json`. |
| `tools/check.ps1 -Real -Scenario Gxx` | Escenario global de `acceptance.json` (`tools/scenarios/gxx.py`). Mientras no exista su runner sale con **2 = BLOCKED**, nunca PASS. |
| `tools/check.ps1 -Coverage` | Cobertura con `cargo-llvm-cov` 0.9.1 instalado en `tools/.bin`. Si falta: `BLOCKED_TOOLING` con el comando para instalarlo, no éxito por omisión. |
| `tools/test-integration.ps1` | Objetivos de integración de Rust (`tests/*.rs`) y verificación de los fixtures comunes con procesos reales. |
| `.venv-tools/Scripts/python tools/validate_evidence.py` | Valida todas las evidencias contra `PromptEvidence`. |

Las herramientas de Python usan `.venv-tools` (`tools/requirements.txt`). Rust 1.99.0 con `rustfmt`, `clippy` y `llvm-tools` (`rust-toolchain.toml`).

## Estado de un prompt

Lo calcula `tools/evidence.py`, nunca se escribe a mano:

- un `FAIL` deja el prompt en **BLOCKED**;
- una prueba `BLOCKED`, un bloqueo de entorno o una dependencia sin `REAL_PASS` lo dejan en **BLOCKED_REAL**;
- **REAL_PASS** exige pruebas `REAL` y que todas pasen.

Cada prueba guarda su transcripción con sha256 en `docs/reports/<PXXX>_runs/`.

## Tipos y etiquetas

- `UNIT`, `CONTRACT`, `INTEGRATION`, `FAULT`: `cargo test`, vitest y verificadores en Python. Corren en cualquier Windows con el toolchain.
- `REAL`: `tools/pxxx_real.py` y scripts equivalentes. Ejecutan el daemon, la ventana, SQLite, Credential Manager, pipes o el sistema de ficheros reales.
- Una prueba que no puede ejecutarse aquí no se omite en silencio. En `cargo test` va con `#[ignore = "<etiqueta>: motivo"]` y en vitest con `[<etiqueta>]` en el nombre. Etiquetas permitidas: `real-hardware`, `network`, `destructive-disabled`. `tools/verify_p007.py tags` rechaza cualquier omisión sin etiqueta. En la evidencia figura como `BLOCKED`, nunca como `PASS`.

| Necesita | Ejemplos | Dónde corre |
|---|---|---|
| nada especial | unitarias, contrato, integración con fakes | cualquier PC con el toolchain |
| Windows real | Credential Manager, DACL de pipes, NTFS sparse, daemon y ventana | PC IA (o cualquier Windows con el usuario del operador) |
| hardware del PC IA | GPU/NPU, runtimes, benchmarks | solo PC IA |
| red | Hugging Face, ModelScope, GitHub | PC IA con salida HTTPS |
| segundo usuario/volumen | ACL entre usuarios, multidisco | pendiente de que el operador lo provea |

## Fixtures comunes

| Fixture | Uso |
|---|---|
| `larm_db::testing::temp_migrated()` | Base SQLite temporal migrada por prueba; se borra al soltarla. |
| `tests/fixtures/http/http_fake.py` | Servidor HTTP en 127.0.0.1 con rutas en JSON: Range/206/416, ETag/304, 429 con `Retry-After`, HEAD, latencia, corte a mitad del cuerpo y registro de peticiones (sin valores de `Authorization`). |
| `tests/fixtures/workers/jsonrpc_fake.py` | Worker de adaptador NDJSON JSON-RPC 2.0 (contrato 03): handshake, heartbeat, cancelación, progreso y modos de fallo (`crash-after`, `hang`, `bad-nonce`, `bad-version`, `garbage`, `oversize`, `stderr-flood`, `no-heartbeat`). |
| `tools/make_sparse.py` | Fichero sparse NTFS (decenas de GiB que ocupan KiB) o denso con contenido conocido. Falla si no queda sparse. |
| `larm_core::security::memory` | Puertos de seguridad en memoria, solo en pruebas. |
| `tests/fixtures/generated/*` | Fixtures del paquete (PNG, WAV, tensor, ONNX) con hashes en `fixtures.lock.json`. |

## Cobertura

Línea base medida el 2026-10-11 con `cargo-llvm-cov`: 78,56 % de líneas, 69,77 % de funciones y 76,42 % de regiones (`docs/reports/P007_runs/coverage.json`). El contrato no fija umbral; se mide y se registra en cada evidencia de P007, sin aprobar ni suspender por ello.
