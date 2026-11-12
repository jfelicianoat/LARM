# P047 — Adaptador de fuente ModelScope

**Estado: `BLOCKED_REAL` · 2026-10-23**

P047 incorpora un worker portable y aislado para ModelScope. Consulta metadata pública, conserva campos upstream después de redactar valores sensibles, normaliza modelos y variantes, fija revisiones por commit y distingue tamaños exactos, de alta confianza y desconocidos. Los bytes solo se devuelven al Download Manager en bloques de hasta 512 KiB; el adaptador no escribe destinos ni inicia descargas.

La revisión contractual 3.5.0 añade `source.download_chunk` al perfil ModelScope y fija la referencia oficial `modelscope_hub` v0.4.0. La firma y los límites siguen los DTO de la revisión 3.3.0; selección de artefactos, preflight del destino y confirmación explícita siguen bajo responsabilidad del Core/Download Manager. El campo `id` usado por la respuesta actual de búsqueda del Hub quedó incluido en el mapeo de identidad y en un fixture de regresión.

## Comprobaciones

| Comprobación | Resultado |
| --- | --- |
| Suite focal `tests.integration.modelscope_adapter.py` | 18/18 PASS: metadata, hashes y tamaños; schema inválido; identidad y revisiones; descarga por rangos/reanudación/EOF; redirección; privacidad; acceso privado y gated; rate limit; timeout, IO y recuperación; cancelación. |
| Worker portable | PASS: hash del paquete, handshake aislado y auth probe anónimo. |
| Consulta pública real desde el worker | PASS: búsqueda anónima read-only con límite upstream de un resultado; devolvió dos variantes; no se descargaron archivos. |
| Compilación Python y sincronización contractual | PASS; 59 copias idénticas al paquete 3.5.0. |
| Gate común `tools/check.ps1 -Prompt P047 -Real` | 14/14 PASS en la repetición con loopback permitido. El workspace Rust reportó 1.087 pruebas pasadas, cero fallidas y cero ignoradas; formato, Clippy, fixtures, arquitectura, package copy, escritorio y lint/typecheck/test/build del frontend también pasaron. |

La primera corrida sandbox del gate falló en tres pruebas HTTP `declarative` porque el health probe local no alcanzó `READY`/`DEGRADED`; la repetición con sockets loopback permitidos pasó las 14 comprobaciones. El recibo final está en [check_P047_real.json](check_P047_real.json); la evidencia de fase, con hashes de logs y fixtures, está en [P047_modelscope-source-adapter.evidence.json](P047_modelscope-source-adapter.evidence.json).

## Límites pendientes

P047 no puede completar hasta que P025, P043 y P045 tengan `REAL_PASS`. La autenticación de repositorios privados y el acceso gated siguen pendientes de P045/P141. El paquete permite por defecto el origen de API configurado; los orígenes de object storage usados por redirecciones requieren calificación y autorización explícitas antes de habilitar transferencia real. Por ello, la consulta pública live pasa, pero no se afirma aceptación real de una descarga completa.

La importación de carpetas de P098 ya crea o reutiliza automáticamente una fuente local por ruta canónica y conserva la procedencia; la respuesta de producto sobre esa elección no requiere modificación en P047.

## Archivos de esta fase

- Worker: `adapters/source/modelscope/`
- Pruebas: `tests/integration/modelscope_adapter.py`
- Herramientas: `tools/p047_live_probe.py`, `tools/p047_package_smoke.py`, `tools/p047_evidence.py`
- Perfil y revisión: `docs/contracts/source-profiles.json`, `docs/contracts/REVISION_3_5_0_P047.md`
- Especificación de integración: `docs/adapters/modelscope.md`

El perfil permanece en `REQUIRES_REAL_QUALIFICATION` y este avance en `BLOCKED_REAL`; las pruebas mock, el smoke portable y una búsqueda de metadata no sustituyen las dependencias ni la calificación de la transferencia.
