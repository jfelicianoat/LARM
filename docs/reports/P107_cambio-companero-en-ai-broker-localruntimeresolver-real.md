# P107 — Cambio compañero en AI Broker: LocalRuntimeResolver real

Fecha: 2026-10-10 · Host: PC IA (Windows 11 Pro 26200) · Paquete: LARM v3 (`Contexto_Broker_P107_v3`)

**Estado: BLOCKED_REAL.** El cambio del Broker está completo y probado de extremo a extremo contra un runtime real en este PC, pero el prompt no se puede cerrar como REAL_PASS: sus dependencias P099 y P101 no tienen REAL_PASS (LARM está en P003), la prueba real se hizo con el cliente en el mismo PC IA y no desde el PC principal, y `tools/check.ps1 -Prompt P101 -Real` no tiene pruebas registradas. Evidencia: [evidence.json](P107_cambio-companero-en-ai-broker-localruntimeresolver-real.evidence.json) · [evidencia bruta](P107_real/).

## Qué se hizo

### En AI Broker (`C:\Procesos\AI_Broker`)

- **Provider interno `larm` y `LocalRuntimeResolver`** (`app/larm.py`, `app/providers/larm.py`): `resolve_model`, `request_lease`, `release_lease`, `get_endpoint` y `get_capabilities` de la spec §63 sobre la API del contrato 1.0.
  - Catálogo `GET /api/v1/broker/catalog`, refrescado cada 30 s, que tolera un parpadeo y se da por perdido tras dos intervalos.
  - Lease con `POST /api/v1/leases` e `Idempotency-Key = task_id:invocation_id`. Admite 201 ACTIVE o 202 con sondeo de `GET /jobs/{id}`. Un fallo de transporte se reintenta con la misma clave, así que no se admite una segunda lease.
  - La lease se renueva según `renew_after_seconds`, con If-Match e Idempotency-Key. Si se pierde, la inferencia se corta.
  - La liberación es idempotente y sobrevive a la cancelación; un 409 por revisión vieja se resuelve releyendo la lease.
  - La credencial de endpoint solo llega por el named pipe del contrato 04 y vive en memoria.
  - El diario `larm_leases` se escribe antes de usar la lease y permite reconciliar al arrancar.
- **Target exacto** `{provider: larm, deployment: local, model: <variant_id>}`. Solo identificadores UUIDv7: un alias falla con `MODEL_UNAVAILABLE`. Las variantes LARM no entran en el ranking, en System-1 ni en el sondeo en sombra. Con fallback permitido, el fallback nunca va a otra variante LARM.
- **Errores del contrato 06**:
  - `RESOURCE_UNAVAILABLE` reintentable → `waiting_for_memory`;
  - falta absoluta de memoria → `VRAM_MODEL_TOO_LARGE`;
  - incompatibilidad o backend distinto → `MODEL_CAPABILITY_MISMATCH`;
  - timeout o caída → `PROVIDER_UNAVAILABLE` reintentable;
  - `fallback_allowed: false` nunca hace fallback.
  El error original viaja en `details.larm_error`.
- **Backend efectivo y huella**: si la lease se sirve con otro backend o huella distintos de los del catálogo y no estaba permitido, se rechaza antes de llamar al runtime. Los componentes con equivalente exacto se trasladan a la huella del broker; el resto va en la extensión versionada `extensions["larm.v1"]`, que entra en el hash. Las huellas sin extensión conservan su hash.
- **Capacidades aditivas** `local_runtime_resolver` y `larm_contract_version`, anunciadas solo con catálogo vivo. `contract_version` sigue en `"2.11"` y no cambia ningún schema de petición.
- **Frontera de datos**: `larm` se trata como proveedor local en `local_only` y `confidential`. Lo encontró la prueba real: antes, el validador rechazaba el target. Como contrapartida, un endpoint de lease fuera de loopback da `CLOUD_NOT_ALLOWED`, sin enviar el prompt.
- **Configuración** `larm` (`app/config.py`): apagada por defecto, solo loopback, plazos y TTL del contrato. `larm` queda reservado como id de proveedor custom.
- **Documentación**: `docs/Client_API.md` (§2, §8.2, §12 y la nueva §16), `docs/Configuration_Reference.md` y `docs/larm-integration/STATE.md`.
- **Runner de integración real**: `scripts/larm_p107_real.py`.

### En LARM (`larm/`)

Una **rebanada mínima con el contrato exacto** para que el Broker tenga un servidor real. No sustituye a P100 ni a P101 (ver Bloqueos).

- `larm-domain::bridge`: DTO estrictos del contrato 1.0 (`deny_unknown_fields`), error con la forma del contrato 07, fórmulas `renew_after_seconds` y bytes como cadena decimal acotada.
- `larm-core::bridge`: puertos `BrokerBridge`, `PrincipalAuthenticator` y `SecretStore`.
- `larm-runtime::external_adoption`: **EXTERNAL_ADOPTION** del Ollama local en loopback; LARM no lo instala, no lo lanza y no lo mata.
  - Solo publica variantes declaradas por el operador y con `accept_external_local_trusted` explícito.
  - La huella se obtiene de la API del runtime: digest, plantilla, versión y backend efectivo tras la carga (`/api/ps`). Lo no observable queda UNAVAILABLE.
  - Admisión con presupuesto duro del operador: pesos, KV si el runtime publica la geometría, overhead y margen; la memoria ocupada por modelos ajenos cuenta como presión.
  - La variante se fija con `keep_alive` infinito mientras hay lease y vuelve a 5 min sin leases.
  - TTL monotónico, EXPIRING con gracia de 30 s y luego EXPIRED con el token revocado.
  - Idempotencia de alta y renovación, If-Match, propietario y generación por arranque.
  - Gateway autenticado por lease: la lease fija el modelo y no se admite streaming.
- `larm-runtime::credentials_pipe`: named pipe `LARM-{sha256(SID)[0:16]}-credentials` con `reject_remote_clients`, nonce de 32 bytes de un solo uso, 60 s y 4 KiB.
- `larm-api::broker`: rutas de `api-operations.json` (catálogo, perfiles, leases, renew, release, jobs, salud) más el gateway. Bearer por principal y scope, Idempotency-Key, If-Match (428 si falta), ids UUIDv7 y cuerpo máximo de 1 MiB.
- `apps/daemon`: `--bridge-config` y `enroll-principal` (token por stdin al Credential Manager), autenticador que relee el almacén ante una rotación, servidor solo en loopback, barrido de caducidad y pipe.

**Correcciones de P001 encontradas aquí:** el `Cargo.toml` del workspace declaraba `reqwest` con la feature `rustls-tls` y `axum` con `sse`, que no existen en las versiones fijadas (0.13.5 y 0.8.9). Nunca se había compilado ninguno de los dos; ahora son `rustls` y `json`, con las mismas versiones bloqueadas. Se añadieron `keyring =4.2.0` y `rand =0.10.3`, ambos ya fijados en `environment.lock.json`.

**Efecto colateral:** `cargo fmt --all` reformateó (solo espacios) `crates/larm-db/src/{backup,dto,migrate,pool}.rs` y `tests/integration.rs`, que son trabajo de P004 en curso.

## Pruebas de código

| Comprobación | Resultado |
|---|---|
| Broker `ruff check .` | PASS |
| Broker `mypy app` | PASS |
| Broker `python -m compileall -q app tests` | PASS |
| Broker `pytest -q` (suite completa) | PASS: 1071 passed, 4 skipped, 0 failed |
| Broker contract tests del puente (`tests/test_larm_bridge.py`, `tests/test_larm_bridge_edges.py`) | PASS: LARM simulado validado contra `larm.schema.json`/`domain.schema.json` v3 (sha256 del manifiesto) en peticiones y respuestas, más un named pipe real de Windows |
| Broker cobertura | 88,97 % frente al gate del 90 %: **rojo, como ya lo estaba antes** (87,86 % en main). Los módulos nuevos están al 94 % (`app/larm.py`) y al 98 % (`app/providers/larm.py`) |
| LARM `cargo fmt --check` | PASS |
| LARM `cargo clippy --workspace --exclude larm-db --all-targets -D warnings` | PASS |
| LARM `cargo test --workspace --exclude larm-db` | PASS (66) |
| LARM `tools/check_architecture.py` y `--selftest` | PASS (16 crates, 4 negativos) |
| LARM `larm-db` (P004 en curso, no tocado) | FAIL previo: clippy `manual_async_fn` en `tx.rs` y test `enums_del_ddl_canales_estrategias_prioridad` (columna `status` ausente) |

Oráculos del prompt cubiertos por contract tests: LARM apagado o caído, scope insuficiente, variante desconocida o alias, lease expirada o perdida, timeout de carga con liberación de la lease tardía, backend efectivo distinto, reintento idempotente con la misma clave, rotación del token, cancelación, reinicio del Broker (diario más reconciliación desde el lifespan) y ningún fallback con `false`.

## Prueba real

`python scripts/larm_p107_real.py --larm-repo <larm> --model gemma4:12b --out docs/reports/P107_real`. Usa un `larm-daemon` real, una instancia aislada del Broker (puerto 8766, base de datos propia y sin publicar token en el keyring de producción), Ollama 0.40.2 y `gemma4:12b`, ya presente en el disco.

| Id | Observado | Resultado |
|---|---|---|
| P107-REAL-01 | `local_runtime_resolver=true`, `larm_contract_version=1.0`, `contract_version=2.11` | PASS |
| P107-REAL-02 | catálogo LARM con la variante adoptada y huella verificada | PASS |
| P107-REAL-03 | tarea `completed`, `served_by=larm/local/<variant>`, `fallback_used=false` | PASS |
| P107-REAL-04 | un único artefacto final; SHA-256 de los bytes descargados = el declarado (`07924af1…d885`) | PASS |
| P107-REAL-05 | invocación contractual con `extensions["larm.v1"].source=lease` y backend `GPU` VERIFIED (de `/api/ps`); `seed_status=sent` | PASS |
| P107-REAL-06 | lease RELEASED con row_version 7 (alta, activación, 4 renovaciones y liberación) durante 47,5 s de inferencia | PASS |
| P107-REAL-07 | tarea cancelada a mitad de inferencia y su lease RELEASED | PASS |
| P107-REAL-08 | variante desconocida con `fallback_allowed:false` falla con `MODEL_UNAVAILABLE` | PASS |

Limpieza comprobada: la credencial del principal se borró del Credential Manager, no quedan credenciales de lease, los procesos terminaron y `gemma4:12b` volvió a su `keep_alive` de 5 min.

## Bloqueos y pendientes

1. **Dependencias sin REAL_PASS.** P099 (perfiles externos por versión) y P101 (catálogo Broker de LARM) no existen todavía; LARM está en P003. El servidor usado es la rebanada EXTERNAL_ADOPTION de esta entrega, no P100/P101. Por contrato, el consumidor no se puede completar.
2. **Cliente desde el PC principal.** La prueba real obligatoria pide el cliente en el PC principal; aquí corrió en el propio PC IA. Pendiente de repetir desde el PC principal contra el Broker del PC IA.
3. **`tools/check.ps1 -Prompt P101 -Real`**: sin pruebas reales registradas para P101 (FAIL, `docs/reports/check_P101_real.json`).
4. **ACL del pipe**: usa la DACL por defecto de Windows. Fijar los SID configurados exige `unsafe`, prohibido en el workspace. La autorización efectiva es el Bearer del principal más el propietario y la generación de la lease.
5. **Espera de admisión de 300 s**: la rebanada devuelve `RESOURCE_UNAVAILABLE` reintentable en el acto y el Broker espera (`waiting_for_memory`).
6. **Leases en memoria en LARM**: un reinicio de LARM las invalida con una generación nueva (sin journal/outbox: P004/P029).
7. **Hardware y drivers**: `hardware_fingerprint` y `driver_versions` quedan UNAVAILABLE hasta P011/P013.
8. **Gate de cobertura del Broker** al 88,97 %, rojo desde antes de este cambio (R1 del informe red team del 3 de octubre).

## Compatibilidad

- Clientes 2.11: sin cambios en peticiones. Las respuestas solo ganan campos opcionales (`capabilities.local_runtime_resolver`, `capabilities.larm_contract_version` y `execution_fingerprint.extensions`).
- Con `larm.enabled: false` (por defecto) el broker se comporta exactamente como antes; lo comprueba la suite completa.
- La tabla nueva `larm_leases` se crea con `CREATE TABLE IF NOT EXISTS`, sin migrar datos existentes.
