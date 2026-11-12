# Revalidación focal P107/P100 — 2026-10-24

Alcance: pruebas locales actuales de la capa Broker–LARM y del repositorio SQLite de leases en el checkout Windows. No se iniciaron AI Broker, Agora, Ollama ni una instancia productiva del daemon; las pruebas del daemon sí levantan fixtures locales. Tampoco se usaron credenciales externas.

| Fase | Comando | Resultado |
|---|---|---|
| P107 dominio | `cargo test -p larm-domain bridge` | PASS, 6/6 |
| P107 core | `cargo test -p larm-core` | PASS, 55/55 (50 unitarias, 5 de conformidad) |
| P107 API | `cargo test -p larm-api broker` | PASS, 5/5 |
| P107 runtime adoptado | `cargo test -p larm-runtime external_adoption` | PASS, 22/22 con loopback habilitado; incluye admisión durable activa con referencias cualificadas, rechazo cerrado, renovación y liberación |
| P107 daemon | `cargo test -p larm-daemon` | PASS, 78/78; incluye fixtures HTTP locales y workers nativos cualificados |
| P100 SQLite leases | `cargo test -p larm-db runtime_leases` | PASS, 7/7; incluye renovación idempotente con CAS/outbox y vencimiento durable con gracia |
| P100 resolvedor P033/P036 del daemon | `cargo test -p larm-daemon reference_store_requires_catalog_bindings_and_http_qualification` | PASS, 1/1 con HTTP loopback; comprueba bindings del catálogo, rechazo de backend no registrado, cualificación y persistencia atómica |
| Calidad y todos los targets DB | `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test -p larm-db --all-targets -j 1 -- --test-threads=1` | PASS |
| Suite completa Rust | `cargo test --workspace -j 1 -- --test-threads=1` | PASS, 1.188/1.188 en la puerta P100 actual |

La prueba de admisión durable ahora cubre ambos resultados: sin referencias cualificadas, el job termina `FAILED`, SQLite registra `ADMITTING → FAILED` y el runtime simulado no recibe una carga; con el resolvedor conectado, el worker valida el endpoint y la lease queda `ACTIVE` usando los IDs cualificados persistidos. El test del daemon también verifica que la instancia externa `READY`, el endpoint `QUALIFIED` y ambos eventos/outbox se registran juntos después de confirmar host, runtime, instalación, backend, variante y perfil.

La nueva rebanada añade renovación durable idempotente con CAS, respuesta de replay, evento `runtime_lease.renewed` y outbox; el sweeper consulta SQLite y persiste `ACTIVE → EXPIRING → EXPIRED` tras la gracia; la liberación usa la transición durable `ACTIVE → RELEASED`. El puente renueva, reproduce la respuesta guardada, libera en SQLite y revoca el secreto local. La suite `external_adoption` pasa 22/22, la cualificación SQL/HTTP del daemon 1/1 y `runtime_leases` 7/7. Los schemas de métodos y capacidades del SDK se regeneraron tras detectar que no coincidían con los contratos canónicos; los workers de referencia y la suite completa pasan con esos artefactos actualizados. El test del catálogo de operaciones quedó sincronizado con las 148 operaciones del contrato actual. La evidencia oficial P100 queda en 15 PASS/0 FAIL/1 REAL BLOCKED; el gate termina 7/7 y el workspace 1.188/1.188. También pasan Clippy, formato, todos los targets de `larm-db` y las 67 copias contractuales 3.9.1.

Esto acredita código y fixtures locales, no la integración real completa. P107 conserva su prueba real del 2026-10-20 como evidencia histórica. En esta revalidación no se iniciaron AI Broker, Agora, Ollama ni el daemon productivo, y no se usaron credenciales externas. Sigue pendiente aceptar el ciclo de admisión con un runtime externo instalado, la prueba desde el PC principal y la aceptación de dependencias.
