# Contratos normativos LARM 1.0

Este directorio cierra decisiones observables que la especificación dejaba sin valores. Son decisiones de diseño de esta revisión, no propiedades verificadas del PC IA. Los ejemplos de LARM_SPEC_V1.md son ilustrativos; estos contratos, schemas y defaults.json fijan su representación ejecutable.

Precedencia: instrucción expresa del usuario > especificación original > contratos de esta revisión > prompt individual > ADR de implementación. Un ADR puede explicar una implementación pero no alterar requisitos o gates. Ante contradicción real registrar BLOCKED con los dos textos y solicitar resolución; no reducir alcance. No consultar el paquete histórico como contrato vigente.

Cada prompt requiere el paquete completo: LARM_SPEC_V1.md, SHARED_CONTRACT.md, manifest.json, contratos y schemas que cita. P001 copia el paquete a docs/spec y docs/contracts, registra SHA256 y entrega herramienta para seleccionar un prompt con todos sus inputs. Si falta un input, no implementar por recuerdo.

Todos los 20 runtimes, todas las fuentes y las cinco familias de plugins forman parte de la entrega. Un backend inaplicable al hardware se informa INAPPLICABLE con evidencia; no elimina el adaptador. La ausencia del equipo o de credenciales es BLOCKED_REAL y no una autorización para marcar el producto completo.

El desarrollador elige funciones privadas y organización interna compatible. No elige los valores públicos, transiciones, algoritmos, contratos de red ni los criterios de promoción. Los valores configurables arrancan con defaults.json; un cambio produce nueva revisión y auditoría.

Reglas comunes: UTC RFC3339 con Z y milisegundos; UUIDv7 para entidades creadas por LARM; SHA256 hexadecimal minúsculo; JSON canónico RFC8785 antes de hashing; UTF-8; tamaños u64 internos limitados a 9223372036854775807 para SQLite y representados como string decimal en JSON. Cero no representa unknown: unknown es null con reason y evidence. No usar number JS para bytes. Rechazar overflow antes del IO.

Solicitudes propias son estrictas, additionalProperties:false; respuestas externas toleran campos aditivos sin perder el núcleo obligatorio. Un ID de fuente o runtime no cambia de significado. Nombres y aliases no son identidad.

State de implementación por prompt: NOT_STARTED, IN_PROGRESS, CODE_PASS, BLOCKED_REAL, BLOCKED, REAL_PASS. CODE_PASS requiere unit/contract/integration/fault injection y permite preparar trabajo independiente, pero no cerrar ni avanzar un módulo consumidor; REAL_PASS requiere además cada prueba real obligatoria. Las dependencias deben estar REAL_PASS para dar por completado un consumidor; mientras una dependencia esté BLOCKED_REAL sólo se permite preparación independiente sin afirmar readiness. P138 exige REAL_PASS en todos los módulos aplicables, evidencias de los 15 escenarios y cero pendientes. COMPILADO no significa EJECUTADO; mock no significa real.

Documentar autorización de pruebas que ya exista. Usar ubicaciones gestionadas y modelos pequeños legalmente disponibles. No modificar instalaciones externas, aceptar licencias ni abrir LAN sin autorización del operador. Una restricción del entorno deja la prueba BLOCKED_REAL y un procedimiento reproducible, nunca un PASS sustituto.

Revisión 3.1.0 (2026-10-22): modifica únicamente el flujo de planes de descarga P052 y su representación de tamaño por artefacto. La selección de opcionales, los intervalos y la confirmación explícita se fijan en 02_STORAGE_DOWNLOAD.md y en sus schemas; el resto de la revisión 3.0.0 se conserva. El manifiesto registra los hashes nuevos y la revisión previa.

Revisión 3.2.0 (2026-10-23): añade la operación P098 read-only `POST /api/v1/models/import/preview` y sus schemas. Permite revisar antes de importar los modelos detectados, tamaños exactos de archivos legibles y rutas relativas, sin registrar, copiar, mover, hashear ni reservar. El tamaño agregado excluye los elementos omitidos cuyo tamaño no se pudo medir. La importación existente permanece como operación de trabajo independiente y exige confirmación explícita en la interfaz, como requiere P123.

Revisión 3.5.0 (2026-10-23): declara `source.download_chunk` en el perfil ModelScope P047 con referencia fijada a `modelscope_hub` v0.4.0; conserva firma y transporte binario acotado de la revisión 3.3.0, sin cambiar selección ni confirmación explícita de P052.

Revisión 3.6.0 (2026-10-23): declara `source.download_chunk` para los perfiles GitHub Releases, HTTP, LOCAL y UNC_PRIVATE implementados por P048. Conserva la firma y el transporte acotado de la revisión 3.3.0; Git LFS no cambia y P052 mantiene selección, preflight y confirmación.

Revisión 3.6.1 (2026-10-24): incorpora `larm-source-p048` y sus dependencias permitidas en `crate-graph.json`, para que la puerta P002 valide el nuevo worker dentro del grafo contractual.

Revisión 3.7.0 (2026-10-24): cierra la conformidad entre P052 `DownloadPlanRequest`, el schema source compartido y el DTO tipado del SDK; establece fallo cerrado para adaptadores que no puedan ligar IDs opcionales al manifiesto fijado. Los intervalos siguen representados por `ArtifactManifest.artifacts[].size_estimate` según `ArtifactSizeEstimate`; no se duplican en el request.

Revisión 3.8.0 (2026-10-24): P141 añade `account_id` nullable y obligatorio al schema de `AuthProbe`. La vinculación se resuelve y comprueba en el host contra fuente, cuenta y `CredentialReference` antes del worker; el worker recibe identificadores no secretos y solicita sólo la entrada `larm/source/{source_id}` / `account_id` mediante Credential Manager, sujeta a `Access.secrets` exacto. La resolución sigue ocurriendo exclusivamente dentro del worker.

Revisión 3.9.0 (2026-10-24): P028 fija el envelope y payload cerrado del registro firmado, los pins Ed25519, orígenes de descarga, secuencia anti-rollback y revocaciones durables; añade el checkpoint SQL y el schema del journal recuperable de la importación inicial. P022 sigue siendo autoridad de firma/confianza del paquete: un índice firmado no concede confianza ni cualificación. P024/P026 conservan sus gates REAL_PASS.

Revisión 3.10.1 (2026-10-25): P052 separa la fuente principal de los mirrors alternativos. La lista de mirrors cualificados admite cero entradas, de modo que estimaciones ESTIMATED o UNKNOWN y descargas sin alternativas siguen usando la fuente principal. El hash liga ambas partes; la activación revalida los mirrors y la procedencia durable registra la fuente y revisión que entregó cada artefacto.

Revisión 3.11.0 (2026-10-25): P032 tipa el ciclo de vida de RuntimeVersion, la solicitud side-by-side, rollback con compare-and-swap, pins por variante/perfil y la política durable de retención. `benchmark_comparison` expone a P032 resultados P117 inmutables y consultables; desconocido/inconcluso no promociona. La limpieza respeta default, pins, benchmarks/comparaciones, certificaciones, leases y ownership externo.

Revisión 3.11.1 (2026-10-25): precisa la resolución de P032/P100: pin exacto de variante/perfil primero y `runtime.default_version_id` después; sin pin ni default la lease falla sin recurrir a la configuración inicial. La lease persiste la versión seleccionada y la vuelve a comprobar antes de activarse. No cambia los schemas wire ni la migración de P032.

Revisión 3.12.0 (2026-10-27), autorizada por el operador: separa manifiesto remoto y bundle físico VERIFIED, resolviendo la contradicción de primera descarga. Referencia source, snapshot, ingesta atómica y binding de planes: [revisión 3.12.0](REVISION_3_12_0_P044_P052.md). Conserva gates físicos, confirmación, reservas y promoción.

## Revisión 3.12.1 — binding del plan source

La firma source.download_plan recibe SourceDownloadPlanRequest con acquisition opcional; referencia pública, hash upstream y mapa de IDs centrales permiten seleccionar una revisión sin consultar IDs privados. Selección y copia exacta source_acquisition_v1 quedan selladas. La petición HTTP del usuario se conserva. [Reglas y oráculos](REVISION_3_12_1_SOURCE_ACQUISITION.md).

## Revisión 3.13.0 — reinicio explícito de transformaciones

Autorizada por el operador el2026-10-30. `POST /api/v1/transformations/{id}/restart` identifica el trabajo anterior, exige `jobs:write`, `Idempotency-Key` e `If-Match`, y reutiliza `ExecutePlan`/`JobReceipt` con respuesta202. Sólo FAILED/FAILED_RECOVERABLE sin output publicado; input original íntegro, plan nuevo confirmado, cualificación/espacio/guards/aprobación actuales, CAS y un único incremento de attempt. El replay liga actor, job, cuerpo y revisión y conserva el recibo original aunque el trabajo avance. Historial y temporales se conservan con ownership; COMPLETED/CANCELLED siguen terminales. La aceptación exige los oráculos H07 y proceso real, sin sustituirlos por esquemas o mocks. [Reglas y oráculos completos](REVISION_3_13_0_P093.md).
