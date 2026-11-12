# P093 — gate integral de consolidación del workspace

Fecha: 2026-11-12. Tras trece fases de conversores, la primera **batería completa del workspace** (`cargo test --workspace`) se ejecuta de una vez — los gates de las fases previas corrían suites específicas congeladas (`-j1 --test-threads=1`), nunca el conjunto. El gate destapa deuda preexistente de las fases de producto (que los wrappers no cubren) y deja la batería integral en verde.

## Corridas y defectos

1. **Corrida 1**: fallo en `conformance_profiles` — el test usaba `timeout_seconds = 601` como techo inválido; la calibración de las fases K elevó el techo a 2400 s. Corregido a 2401 (sigue inválido para todos los consumos: conformance.rs, qualification.rs, profiles.rs).
2. **Corrida 2**: 4 fallos en las aceptaciones nativas de `reference_adapters`:
   - 3 fallos (`source_watch_tcp`, `p048_download`, `p048_http_producto`) por **`larm-source-p048.exe` ausente en target/debug**: el bin de la source P048 es un crate binario sin dependencia de test, `cargo test --workspace` no lo reconstruye, y ningún wrapper lo construye explícitamente. Construido (`cargo build -p larm-source-p048`); los tres PASAN.
   - 1 fallo (`cinco_paquetes_originales...`) con `conformance_capability_profile_missing`: la regla estricta del profile introducida por las fases de producto deja el test de los cinco recetas originales (que no corre en ningún wrapper) con `capability_profile: None` y manifest con capabilities. Corregido a `Some("reference-capabilities.json")` — los cinco son paquetes de referencia; PASA.
3. **Corrida 3**: **103 suites OK**, un fallo residual intermitente: `consumer::tests::h08_publicacion_concurrente_del_mismo_blob_ocho_veces` (larm-download) — flaky bajo carga paralela del workspace run; **10/10 corridas secuenciales PASS** (1,1 s cada una). No guardado como fallo de producto; documentado para vigilancia.

## Resultado integral

- **Rust workspace**: 103 suites OK (dominio 181, DB 77, API models 6, openapi 11, promoción 4, host 64+49, adapter-host conformance...), 0 fallos tras correcciones; `cargo clippy --workspace --all-targets -D warnings` limpio; `cargo fmt --all --check` limpio.
- **UI**: 20 archivos / 157 pruebas PASS; `tsc -b` limpio; `vite build` exitoso.
- **Contratos**: arquitectura PASS, **80 copias idénticas al paquete 3.14.0**, P001 policies PASS.

## Estado de P093

Este gate es la **regresión integral de preparación** de P093 — no es el `CODE_PASS` del contrato (que exige los gates de aceptación del contrato y los REAL_PASS de sus dependencias bloqueadas). P093 sigue `BLOCKED_REAL`/preparación. Contrato vigente: **3.14.0**.

## Mantenimiento que deja fijado

- El bin `larm-source-p048` debe construirse antes de los gates de aceptación nativos que lo lanzan (los wrappers de fases futuras lo incluirán como paso explícito).
- Los tests del workspace no incluidos en wrappers deben revisarse cuando una regla de contrato se endurezca (lección del `capability_profile` y del techo de timeouts).
- El flaky H08 bajo carga queda en vigilancia (10/10 secuencial).
