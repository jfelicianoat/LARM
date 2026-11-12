# Protocolo de fuentes v1 — P043

Están implementados los tipos y la validación de las nueve firmas source obligatorias y la firma opcional `source.download_chunk`, la frontera Search/SourceSearch, metadatos, paginación, planes, cliente/handler con procesos reales y catálogo persistente con sus cinco rutas CRUD y pantalla Fuentes. Esto no acredita soporte de un repositorio remoto. La búsqueda federada y el postfiltrado son P044; los adaptadores concretos conservan sus propietarios. El estado formal y sus dependencias se calculan en la [evidencia P043](../reports/P043_modelsourceadapter-protocol-y-catalogo-de-fuentes.evidence.json).

## Frontera de búsqueda implementada

`source.search` conserva la firma publicada: request `SourceSearch`, response `ModelPage`. La request tiene exactamente `source_id` (UUIDv7), `q` (hasta 500 caracteres), `capabilities`, `cursor` (string no vacío o null) y `limit` (1..200). Los cinco campos son obligatorios, incluido cursor null. Propiedades extra, tipos incorrectos, ID inválido o límite fuera de rango se rechazan antes de IO.

La API de búsqueda utiliza `Search` completo de §108/H05; no se sustituye por SourceSearch. Sus rangos conservan strings decimales exactas y todos sus campos son explícitos. También son obligatorios los extremos nullable de cada rango y el cursor: omitir una clave no equivale a enviar null.

`SearchSupport` es una declaración local de negociación para el catálogo, sin añadir campos al manifiesto ni a los DTO publicados. Declara `search_applicable`, `supported_filters` (conjunto de identificadores namespaced) y `limitations`. Los bindings conocidos de esta firma son `source.search.q` y `source.search.capabilities`. Afirmar esos bindings requiere respetar su semántica, incluida OR dentro de la lista de capacidades. No se infiere soporte a partir del nombre de la fuente.

Identificadores de vendor desconocidos se conservan sin interpretarlos ni transmitir campos adicionales. No se acepta declarar `core.*` o supuestos `source.search.formats`/`architectures`: esta firma no puede representarlos upstream. Una fuente import-only declara `search_applicable=false`, sin filtros de búsqueda; `require` devuelve `UNSUPPORTED_CAPABILITY`, incluso con query vacío. Una petición directa con texto/capacidades no anunciados devuelve el mismo error, sin fingir una página vacía.

`SearchSupport.project` prepara trabajo interno para Core. Envía texto/capacidades sólo si están anunciados; los demás quedan en `deferred_filters`. Conserva la request completa y las limitaciones, sin convertir la proyección en un resultado filtrado. Incluye source_ids, arquitectura, formato, cuantización, runtime, licencia, rangos de parámetros/tamaño/confianza, certificación, compatibilidad y los tres toggles cuando están activos. Un requisito de metadato desconocido no puede darse por satisfecho.

El cursor upstream se recibe aparte y sólo él entra en SourceSearch. Nunca se transmite el cursor federado de Search a un plugin. El digest es el de todos los filtros originales salvo cursor/limit; el orden de listas OR no lo cambia, cualquier filtro sí. P044 debe conservar esos datos al firmar su cursor y continuar obteniendo páginas o usar un snapshot federado antes de producir una página global. Una sola página upstream no acredita completitud. Una contradicción como INCOMPATIBLE con show_incompatible=false se conserva para que Core/UI la representen.

## Nueve firmas tipadas

| Método | Request | Response |
|---|---|---|
| source.search | SourceSearch | ModelPage |
| source.get_model | UpstreamRef | ModelInspection |
| source.list_revisions | UpstreamRef | RevisionPage |
| source.get_manifest | RevisionRef | ArtifactManifest |
| source.estimate_size | RevisionRef | StorageEstimate |
| source.download_plan | DownloadPlanRequest | Plan |
| source.download | ExecutePlan | JobReceipt |
| source.download_chunk (optional) | SourceDownloadChunkRequest | SourceDownloadChunk |
| source.sync | SyncRequest | SyncResult |
| source.auth_probe | AuthProbe | AuthResult |

Los tipos siguen los schemas publicados, incluidos AuthMode, DownloadPriority y campos nullable obligatorios. `source.download_chunk` exige base64 canónico, offset idéntico al solicitado, límite de 512 KiB, suma comprobada y coherencia entre EOF y total. SourceRequest/SourceReply son uniones internas; no introducen un tag en el wire. Parsear y volver a serializar comprueba que tipos compartidos no oculten claves required ausentes como null. Metadata y limits abiertos conservan claves desconocidas. IDs, fechas UTC exactas, hashes, referencias, rutas relativas y colisiones de artefactos se validan; un manifest de otra revisión se rechaza.

SourceHandler adapta ModelSourceAdapter a la sesión P020. Verifica familia, binding de source.search, método anunciado, request, negociación y reply; no hay implementaciones predeterminadas que simulen éxito. Una request inválida devuelve -32602 con LarmError, filtro no soportado un error de dominio y reply inválida PROTOCOL_ERROR. El cliente SourceWorker vuelve a validar la respuesta y liga las consultas y planes a su source_id. Usa GuardedWorker P022 con Access, timeout y cancel por UUID. La cancelación es un control interno del host; la autorización de un endpoint de jobs corresponde a su propietario.

Se corrigió el host común: un segundo RPC con el mismo id aún pendiente devuelve CONFLICT request_id_in_use antes de escribir, conservando el receptor y la cancelación del original. Codificar un frame demasiado grande tampoco deja una entrada pendiente. El resto del lifecycle, cola, heartbeat y gracia procede de P021.

## Metadatos y rate limit

SourceMetadata se adjunta al namespace abierto metadata.source_v1, con original, normalized, limitations y rate_limit. Conserva campos de modelo desconocidos, null y otros namespaces. Antes de adjuntar se redactan claves sensibles reconocidas y patrones de secretos; no se deben suministrar headers o credenciales como metadatos de modelo.

RateLimit conserva limit/remaining/retry_after_seconds como strings decimales nullable y reset_at como UTC nullable. Un retry_after de 900 s permanece 900 en la observación; retry_delay devuelve como máximo 60 s. Ausencia no significa cero. Un 429 se representa como UPSTREAM_UNAVAILABLE reintentable, reason source_rate_limited y original_error {system:source,code:429,message:JSON de RateLimit}; funciona incluso cuando no hay items. No añade campos a Error ni devuelve una página vacía como éxito. El ejecutor P050 conserva backoff, número de reintentos y presupuesto agregado.

## Paginación

SourcePagination es estado interno de Core, no un cursor cliente. Conserva digest completo, cursor upstream esperado, snapshot y IDs/cursores ya vistos. Valida toda la página antes de avanzar: petición cambiada, cursor/ID repetido o snapshot distinto se rechazan sin consumir el cursor. Un snapshot que cambia produce source_snapshot_changed; P044 debe informar parcialidad o reiniciar, no declarar completitud. La última página sólo termina esa fuente, no el filtrado federado.

SourceSearch y SyncRequest admiten cursor en sus requests. La firma publicada de list_revisions recibe UpstreamRef, sin cursor; no se le añaden campos. RevisionPage conserva next_cursor si la fuente lo devuelve, pero esa firma no ofrece una continuación general. No se acredita una enumeración completa fingiendo que una sola página la contiene; la limitación debe quedar visible al consumidor.

## Plan de descarga

Plan conserva su DTO exacto. Un paso request.source_download_v1 contiene SourceDownloadSpec: variante, destino, revisión/fuente, artefactos con ruta/tamaño/hash/required/role, requisitos auth/licencia, resumable, prioridad y presupuesto duro. El hash canónico del plan cubre todos sus campos salvo plan_hash, incluido ese payload. Validarlo comprueba hash, binding a la request, suma comprobada de bytes y coherencia de confianza: EXACT necesita tamaños y hashes; HIGH tamaños; ESTIMATED una cota. Unknown conserva null y no se transforma en cero.

Los helpers comprueban autenticación/licencia y necesidad de presupuesto explícito cuando el tamaño es UNKNOWN. El estimate físico se recibe del preflight de su propietario; el SDK no inventa libre de volumen ni sustituye reservas, guards, journal, aprobación, expiración o ejecución de P050. La fixture source.download rechaza con UNSUPPORTED_CAPABILITY fixture_read_only: no devuelve un receipt sin haber persistido una descarga.

La referencia externa P025 sí ejecuta fixtures HTTP con un ledger SQLite propio y devuelve un JobReceipt COMPLETED cuya URL file apunta a un JobState real del worker. JobReceipt es compartido: su schema no exige una ruta de Core. El SDK valida su URL no vacía sin inventar esa propiedad; los consumidores de producto no deben abrir URLs arbitrarias del worker. P050 conserva sus jobs, admisión y reservas globales. Ver [guía de autoría](authoring.md) e [informe P025](../reports/P025_sdk-de-adaptadores-y-adaptadores-de-referencia.md).

## Catálogo y API

`ModelSourceAdapter` enlaza una instalación de familia source con un tipo de fuente y su `metadata.source_protocol_v1` (SearchSupport). El registro local usa `larm-daemon --data-root <perfil> register-source-adapter --installation <UUIDv7> --source-type <tipo> --descriptor <JSON>`. Sólo acepta instalaciones INSTALLED/VALIDATED no revocadas, deriva capabilities del manifiesto almacenado y valida las nueve firmas obligatorias; `source.download_chunk` sólo se usa cuando aparece en el manifiesto. Repetir exactamente el binding no crea filas/eventos; cambiar su descriptor exige otro binding/instalación. No ejecuta código ni valida un upstream.

En la misma transacción se publica `adapter_installation.metadata.source_bindings:[{id,source_type}]`, un índice descriptivo para el selector de la UI en GET /adapters. Incrementa la revisión de la instalación y emite adapter.updated. El FK tipado de model_source_adapter, el manifiesto y el estado/confianza siguen siendo la autoridad de admisión. El índice no concede permisos ni conformidad y no sustituye esa relación.

Las tablas existentes de 0001 bastan; no hay migración nueva. `model_source` usa el DTO publicado exacto: nombre, binding, dirección, prioridad, enabled, auth_mode, account_id obligatorio nullable y policy abierta. Se preservan valores desconocidos públicos; se rechazan claves/patrones de credenciales. URL sin userinfo, fragmento ni query sensible; autenticación HTTP sólo en loopback. Raíces locales/UNC deben ser absolutas, sin traversal, ADS ni dispositivos. Guardar una raíz no comprueba conectividad, no escanea archivos y no inventa disponibilidad.

| Ruta | Comportamiento |
|---|---|
| GET /api/v1/sources | read; q literal sobre nombre/dirección, capability exacta, cursor firmado ligado al digest, orden estable, limit 1..200 |
| POST /api/v1/sources | models:write; Idempotency-Key y hash del cuerpo HTTP exacto; 201 original retenido 30 días por actor/operación/clave |
| GET /api/v1/sources/{id} | read; detalle publicado o 404 |
| PATCH /api/v1/sources/{id} | models:write; DTO de configuración completo e If-Match obligatorio; CAS o 409 |
| DELETE /api/v1/sources/{id} | models:write; sin body, If-Match; 204 sólo sin referencias |

Alta, replay, edición, borrado, eventos source.updated y outbox son transaccionales. Dos altas con la misma clave crean una fuente; dos cambios de la misma revisión tienen un ganador. Replay conserva el 201 original incluso tras editar/eliminar la fuente o su binding; cambiar el cuerpo genera IDEMPOTENCY_CONFLICT. Ausencia de If-Match produce 428, formato inválido 400 y revisión vieja 409. No se toman reservas ni se crean journals para guardar configuración.

Activar requiere que la instalación sea VALIDATED y no revocada. P026 es propietario de la cualificación; P043 no promueve instalaciones. Se permite guardar desactivada y desactivar incluso tras revocación. Una cuenta debe pertenecer a esa fuente. Referencias de distribución, procedencia, cuenta o seguimiento bloquean eliminar y cambiar dirección/tipo; cambiar de versión de conector sin cambiar identidad es posible. Eliminar configuración nunca elimina modelos, bytes ni credenciales.

## Pantalla Fuentes

Usa las rutas canónicas a través del transporte nativo existente; no recibe el token administrativo. El selector carga todas las páginas de adaptadores y envía el binding correcto sin pedir UUID manuales. Alta inicialmente desactivada; estado de conformidad visible. El formulario conserva cuenta/policy opacas al editar, no acepta secretos y no simula vincular una cuenta (P045). Un error conserva el borrador y una respuesta perdida conserva la clave idempotente para el mismo contenido; cambiarlo usa otra clave. No se reintenta un CAS obsoleto automáticamente. Cancelar/actualizar relee el catálogo; borrar exige confirmación dentro de la lista.

Se verificaron estados de carga, vacío, error/permisos, páginas y cambios de otro actor, además del recorrido navegador → puente QA con token sólo del lado servidor → daemon → SQLite reales. Este recorrido no acredita una interacción en el WebView nativo. La revisión visual a 375/800 px usa datos explícitos de prueba; capturas en P043_runs/ui-*.png. Se conserva el sistema visual del shell. El detector sólo señaló el borde lateral ya existente de `.device` en styles.css:153, fuera del cambio de estilo.

## Verificación y límites

Las pruebas del SDK comprueban la frontera y las nueve firmas, metadatos, 429, planes y paginación. Un oráculo Python valida 18 documentos y 102 negativos contra los schemas canónicos y calcula independientemente el hash de la fixture del plan. Seis pruebas de host usan un ejecutable real, paquetes con hash/firma verificados por P022 y detección real de elevación: paginación/metadata, gates antes del callback, import-only, 429/auth/plan/reply inválida, timeout/cancel cooperativos, crash/reinicio, cancel no atendido e id RPC duplicado.

El catálogo añade 2 pruebas de dominio, 2 de normalización Core, 8 SQLite con fallos y concurrencia, 17 pasos HTTP/CLI reales y 8 casos de interfaz. El recibo del recorrido real de navegador se liga a los hashes del bundle/fuentes/daemon y se contrasta contra SQLite/outbox; verificar el recibo no reproduce las interacciones. `tools/p043_evidence.py` es el runner completo y `tools/check.ps1 -Prompt P043 -Real` incluye la regresión general. preparation.checks.json queda como recibo histórico anterior al catálogo.

P020/P040/P041 conservan sus bloqueos reales. Las fuentes upstream concretas, vinculación de cuentas y descarga/reanudación/checksum reales mantienen sus propietarios. Las fixtures de protocolo no acreditan esas integraciones. La activación usada como fixture de un test SQL tampoco acredita P026.
