# P081 — WSL2 y Container execution host abstraction

Fecha: 2026-11-14. **Estado: CODE_PASS con aceptación REAL del registro sobre WSL2/Docker de este equipo; REAL_PASS completo bloqueado por la cualificación de instalación runtime (P031/P099 conservan locks UNQUALIFIED — ninguna instalación ha referenciado aún un execution host con lock cualificado).**

## Incremento

El prompt exige preparar LARM para runtimes compatibles con Windows ejecutados mediante WSL2 o contenedor, **sin modificar WSL/Docker global** (§5.1 Windows First, §52 estrategias `WSL`/`CONTAINER`). Todas las dependencias (P030/P022/P031) tienen informe con fase completada; WSL2 y Docker están instalados en el PC IA, así que la detección real es ejecutable.

## Implementación

- **Dominio** (`crates/larm-domain/src/runtime.rs`): enum contractual `ExecutionHostKind` (`WINDOWS_NATIVE`/`WINDOWS_PYTHON`/`WINDOWS_ML`/`WSL2`/`CONTAINER`/`EXTERNAL_LOCAL`) con la regla `allows_strategy` — un runtime Linux bajo WSL jamás se anuncia como nativo (la GUI distingue los seis modos). DTO `ExecutionHost` sellado por la **revisión contractual 3.15.0** (ver abajo) con `validate` completo (nombre, coherencia `wsl_version`↔`kind`, evidencia).
- **Detección read-only** (`crates/larm-hardware/src/virtualization.rs`): sondeo `wsl --status` / `wsl --list --verbose` / `docker --version` / `docker info --format` con timeout+kill, entorno mínimo sin credenciales, salida acotada, decodificación UTF-8/UTF-16LE (wsl.exe emite UTF-16 en tuberías) y **parsers tolerantes al locale** (status en español o inglés). La ausencia de un binario es observación válida, no error. 5 pruebas (incluida una sin binarios).
- **Abstracción** (`crates/larm-runtime/src/execution_host.rs`): mapeo explícito Windows↔WSL (`\\wsl.localhost\<distro>\`, lectura tolera el legado `\\wsl$\`, ida y vuelta, rechazo de traversal y rutas fuera del distro), montajes de contenedor con rutas absolutas, **política de puertos sólo loopback** (`execution_host_port_must_bind_loopback`, efímeros prohibidos) y **validación de planes**: `wsl --install/--uninstall/--update/--set-*/--manage`, `dism`/`wusa`/`netsh`, habilitaciones por PowerShell, verbos `pull/push/build/save/load/import/commit` y `run --privileged/--network host` se rechazan con `execution_host_plan_mutates_host`. Coherencia instalación↔host por estrategia (`execution_host_reference_missing`/`execution_host_strategy_mismatch`). 6 pruebas.
- **Persistencia** (`migrations/0015_p081_execution_host.sql`, `crates/larm-db/src/execution_hosts.rs`): tabla `execution_host` STRICT con triggers de esquema y `ALTER TABLE runtime_installation ADD COLUMN execution_host_id` (FK). Repositorio con upsert idempotente de observación (row_version+1), journal `runtime.execution_host` + evento `runtime.execution_host.observed` + outbox en una transacción, listado con filtro q/estado/cursor. **Sin columna ni ruta de promoción: el registro no habilita ejecución**. 5 pruebas SQLite.
- **Admisión** (`crates/larm-runtime/src/install_service.rs`): `resolve_plan_inputs` resuelve el host referenciado y verifica la coherencia estrategia↔tipo antes de planificar. `InstallRequest.execution_host_id` (obligatorio-nullable en el esquema) valida la coherencia estructural en el dominio.
- **API** (`crates/larm-api/src/runtimes.rs`): `GET /api/v1/execution-hosts` (scope `read`, operación `P081_get__execution_hosts` de la revisión 3.15.0) con filtros q/estado y paginación por cursor. El catálogo de operaciones llega a **155** (gates openapi/auth actualizados: 154→155).
- **UI** (`apps/desktop/src/runtimes.tsx`): panel «Hosts de ejecución observados» en la pantalla Runtimes — lista los motores con tipo (WSL2/Motor de contenedores), versión del motor, por-defecto y estado, con la advertencia de que el registro **no acredita compatibilidad ni habilita ejecución**. Vitest 3 nuevos (panel con ambos motores, estado vacío, fallo aislado); **suite completa 21 archivos / 164 pruebas PASS**; `tsc -b` y build limpios.

## Revisión contractual 3.15.0 (autorizada por el operador)

El DTO sellado `runtime_installation` y el catálogo exigían revisión: documento `LARM_prompts_v3/contracts/REVISION_3_15_0_P081.md` (problema, DTO nuevo `runtime_execution_host`, campo `execution_host_id`, seis reglas normativas), entrada `P081_get__execution_hosts` en el catálogo del paquete, `domain.schema.json` con el DTO y el campo, `larm.schema.json` con refs y `InstallRequest` actualizado (requerido-nullable), esquema del método `runtime.install_plan` con el campo, manifiesto **3.15.0** con hash nuevo y entrada en `contract_revision_log`. **Autorización expresa del operador** («Autorizar revisión e implementación») concedida vía pregunta directa y registrada aquí. `package_copy.py sync` → `check` **PASS 81 copias idénticas al paquete 3.15.0**.

## Aceptación real (PC IA, 2026-11-14)

`apps/daemon/tests/execution_hosts.rs::windows_sondeo_real_wsl2_y_docker_registro_y_lectura_publica` **PASS** (0.58 s): sondeo real de este equipo → `wsl --status` Ubuntu (WSL 2), `wsl --list --verbose` con Ubuntu (v2, default) y docker-desktop (v2), `docker --version` **29.7.2 build a7dcaa6**, daemon no vivo (observado, no inferido); registro de ambos hosts con journal/evento/outbox; lectura `GET /api/v1/execution-hosts` con token reader (200, dos items, WSL2 con `wsl_version=2` y CONTAINER con la versión real), filtro por nombre (1 item), filtro inválido (400), anónimo (401), MCP sin scope read (403) y **POST rechazado por método no permitido** (el registro es read-only por la API). Salida real capturada en `P081_runs/real-probe-output.txt`.

## Defectos encontrados durante la fase (corregidos)

1. La heurística UTF-16LE inicial comprobaba los índices pares en vez de los impares (los bytes altos nulos van en las posiciones impares) — corregido con prueba de las tres formas (BOM, heurístico, UTF-8).
2. El `json!` macro de Rust no acepta encadenar `.as_object()` sobre un literal — los `metadata` del test se construyen con `serde_json::from_value`.
3. El esquema del método `runtime.install_plan` vive en `docs/adapters/method-schemas/` del repo y se compila dentro del SDK (`include_str!`): añadir el campo al `larm.schema.json` NO basta — el worker fixture (`fake_runtime_worker.exe`, binario pre-compilado como `larm-source-p048`) hay que reconstruirlo explícitamente y el corpus (`tests/fixtures/adapters/method-schema-corpus.json`, 9135 casos) regenerarlo con `p026_schema_corpus.py write`; el contador del oráculo del SDK pasó 1000→1001.
4. `enroll` de Principals exige `PrincipalKind::Mcp` para llevar `mcp_scopes` y el orden de argumentos es (kind, name, …).
5. El join del outbox es por `event_record_id` (no `event_id`) contra `event.id`.

## Puerta formal completa

`tools/check.ps1 -Prompt P081 -Real` → **STATUS PASS (11 checks, 0 fallos)** (recibo `P081_runs/check_P081_real.json`): fmt, clippy workspace, fixtures, cargo test del workspace completo (`-j1 --test-threads=1`), arquitectura, copias 3.15.0, suites Python (8), shell Tauri (fmt/clippy/test), frontend y `p081_evidence.py` (P081: BLOCKED_REAL — 7 PASS, 0 FAIL, 2 procedimientos reales bloqueados por locks sin cualificación). La puerta destapó y cerró cinco defectos de integración: el scratch del fixture del paquete no cubría `TMPDIR` (Python lo prefiere sobre `TEMP/TMP` — Olive volvía a caer en la ruta unicode de `target/larm-test-tmp`), el fixture `reference-runtime.requests.json` necesitaba `execution_host_id` (ahora requerido por el esquema del método), el contador Python del corpus (1000→1001), los fixtures JSON de `RuntimeInstallation`/`InstallRequest` en core/domain/db/daemon (campo nuevo requerido) y las listas de migraciones esperadas de los tests de DB (0015).

## Pendientes (bloqueos reales, no retirados del alcance)

- **Cualificación real por versión upstream** (P099): ningún lock UNQUALIFIED acredita compatibilidad de un runtime WSL/contenedor concreto; la cualificación exige los fixtures y probes completos por runtime.
- **Instalación real con estrategia WSL/CONTAINER** (P031/P099): la referencia `execution_host_id` queda probada estructuralmente; la primera instalación real que la use exigirá un lock cualificado.
- **Backend probes dentro del huésped** (P061): los backends de un runtime instalado en WSL2/contenedor se observarán con su adaptador propio.

## Verificación

- Tests: dominio+hardware 11 (virtualización 5, hosts 6), runtime 18 (6 de execution_host + install 12), DB 19 (execution_hosts 5 + runtime_installation 14), API 38, SDK contracts 2, daemon 1 REAL — todas PASS.
- Gates: clippy workspace `-D warnings` limpio; `cargo fmt --all --check` limpio; UI 164/164 + lint + tsc + build; 81 copias contractuales al paquete 3.15.0; arquitectura sin violaciones nuevas (módulos con dependencias hacia larm-domain).
