# Conformidad de la revisión 3 con la especificación original

Este contrato concreta las correcciones H01–H09 de la auditoría de la revisión 2. Sus campos están materializados en los schemas y DDL del paquete; no autoriza a reducir requisitos de la especificación. Las reglas siguientes se implementan y se prueban en los prompts propietarios indicados en CORRECCIONES_V3.json.

## H01 — Canales y política por runtime (§49)

RuntimeChannel usa stable, prerelease, nightly, commit y custom. Los nombres STABLE/PRERELEASE/NIGHTLY/COMMIT/CUSTOM de la especificación se convierten a minúsculas en la frontera, sin fusionar categorías. experimental deja de ser un canal persistente: una observación histórica experimental requiere inspección del upstream para decidir prerelease/nightly/commit/custom; no inferir una equivalencia. Un valor upstream no interpretable se conserva como dato original, se registra custom con motivo UNKNOWN y no se auto-promueve. P000/P099 aportan la evidencia.

Runtime.channel_policy = {allowed_channels:[RuntimeChannel]} no vacío, sin duplicados, default {allowed_channels:[stable]}. POST /runtimes recibe esa política y PATCH /runtimes/{id}/channel-policy la cambia con If-Match. Se persiste como JSON validado en runtime.channel_policy. La política filtra descubrimiento/selección automática y actualizaciones de ese runtime; no oculta inventario ya instalado ni modifica pins, defaults o leases existentes. El operador puede revisar un candidato excluido, pero no adoptarlo automáticamente contra la política.

Aceptación: roundtrip y presentación de los cinco canales; cambio de política por runtime con CAS, persistencia y filtro distinto para prerelease/nightly/commit; no interpretar canal desconocido como estable.

## H02 — Estrategias declarables (§52–§53)

InstallRequest.strategy y runtime_installation.strategy admiten las doce estrategias de §52: PREBUILT_BINARY, GITHUB_RELEASE, SOURCE_BUILD, WINGET, PIP, UV, CONDA, MAMBA, NUGET, WSL, CONTAINER y CUSTOM. Se conservan MSI_MANAGED, PYTHON_VENV y EXTERNAL_ADOPTION como extensiones distintas. Para importar datos históricos: ARCHIVE corresponde a PREBUILT_BINARY, BUILD_FROM_SOURCE a SOURCE_BUILD y WSL2 a WSL; esa conversión se audita. No convertir UV/CONDA/MAMBA en PYTHON_VENV.

AdapterManifest.installation_strategies es una lista del mismo enum; una familia que no instala declara lista vacía. Los perfiles bloqueados fijan mecanismos realmente anunciados, commands/SDK, prerequisites, dependencies_lock, permisos y pruebas por estrategia. Una solicitud no anunciada devuelve UNSUPPORTED_CAPABILITY antes de ejecutar o descargar. Cada runtime implementa las estrategias aplicables, sin obligación de implementar todas. CUSTOM sigue el worker con permisos, allowlists y aprobación existentes, sin scripts de shell arbitrarios introducidos por un formulario.

PIP/UV usan entornos propios de la combinación runtime/version/backend y lock de paquetes; PYTHON_VENV expresa la preparación de un venv gestionado. CONDA/MAMBA mantienen prefijo gestionado y lock de paquetes/canales; MAMBA puede usar micromamba si ese ejecutable está cualificado. Ninguno contamina Python/base global. WINGET/MSI con efecto global requieren autorización del operador; en su ausencia se informa bloqueo o adopción externa, no se cambia silenciosamente de estrategia. Versiones y comandos se obtienen en P099, sin asumir flags.

Aceptación: declaración, solicitud, plan y roundtrip de cada estrategia; negativas de estrategia no anunciada y efecto global no autorizado; flujos UV y CONDA/MAMBA aislados en las pruebas reales de los adaptadores que los anuncien.

## H03–H04 — Configuración reproducible y entorno (§13, §56)

LaunchParameters añade memory_limit_bytes (string decimal positiva o null), runtime_arguments:string[], environment:EnvironmentConfig y adapter_options:object. null expresa ausencia de límite adicional del perfil; nunca cero ni unknown como permiso. Los perfiles builtin materializan null, [], {allowlist:[],variables:[]} y {} hasta que el operador configure un valor. El resto de parámetros sigue el contrato 04.

EnvironmentConfig = {allowlist:string[], variables:EnvironmentBinding[]}. Cada binding contiene name y exactamente value:string (no secreto) o credential_reference:string bajo larm/. Los nombres cumplen [A-Za-z_][A-Za-z0-9_]*; nombres duplicados y variantes de mayúsculas de un mismo nombre se rechazan en Windows. Cada nombre pertenece a allowlist y al environment_allowlist del perfil cualificado del adaptador. Credenciales se resuelven en el worker con los permisos del manifest, nunca en GUI, JSON de respuesta, exportación, hash de contenido secreto o logs. Exportar solo referencias y valores no secretos; una referencia no disponible en destino bloquea ejecución hasta enrolamiento.

DeclarativeAdapter.environment es la entrada del asistente para probe/start/stop, persistida y exportable. Se puede validar y simular sin ejecutar. La allowlist no incluye secretos como texto. Para arranque, la configuración base del adaptador se combina con la del perfil; el binding del perfil sustituye el del mismo nombre, y las variables efectivas deben estar autorizadas por ambas políticas y el perfil de runtime. No ampliar permisos por unir allowlists. Para probe/stop sin perfil se aplica solo la configuración del adaptador. No heredar variables adicionales del usuario; el entorno básico del worker está explícitamente fijado por el perfil cualificado.

runtime_arguments se valida contra runtime_arguments_schema del perfil bloqueado; adapter_options contra launch_options_schema. Opciones desconocidas se rechazan antes de IO. No admitir shell expansion, ni permitir que un argumento alternativo sustituya MODEL_PATH/PORT/roots/límites o desactive auth/sandbox establecidos por el Core. Si el runtime no puede respetar un límite solicitado, devolver UNSUPPORTED_CAPABILITY con evidencia; no etiquetar como respetado. Un memory_limit_bytes no null es positivo y se combina con Constraints.max_memory_bytes mediante el mínimo de los límites no null, además del budget de admission del contrato 04.

LaunchPlan devuelve effective_parameters, environment (valores no secretos y referencias), env_allowlist derivada y effective_memory_limit_bytes. Guardar el snapshot completo en la RuntimeInstance. El hash del perfil/fingerprint incluye argumentos, opciones, entorno no secreto y referencias; una rotación de credencial requiere nueva revisión técnica de entorno para invalidar evidencia previa sin publicar ni hashear el secreto. P013/P034/P033/P100/P117 conservan el snapshot efectivo en cada comparación.

Aceptación: edición/roundtrip/diff/hash de cada campo; args u opciones desconocidos, límites incompatibles y env fuera de allowlist rechazados; proceso real con entorno permitido y sin secretos en export/respuestas/logs; cambios exclusivos de configuración producen nueva huella.

## H05 — Search completo y paginación (§108)

Search conserva q, capabilities, source_ids y compatibility, y añade architectures, formats, quantizations, runtime_ids, licenses, parameter_count:{minimum,maximum}, size_bytes:{minimum,maximum}, trust_score:{minimum,maximum}, certification_statuses, only_executable, only_recommended, show_incompatible, cursor y limit. Arrays vacíos y límites null omiten ese filtro; q vacío omite texto. Todos los campos son explícitos. Rango decimal de parámetros/tamaño usa strings exactas; score usa enteros 0..100. minimum<=maximum si ambos están presentes. limit 1..200, default de cliente 50; cursor null inicia búsqueda.

Los elementos de cada lista son OR; dimensiones y toggles son AND. runtime_ids selecciona candidatos compatibles con al menos un runtime indicado. only_executable exige los hard gates de ejecución PASS para el PC y disponibilidad real; only_recommended exige recomendación vigente según contrato 05, no popularidad. show_incompatible=false excluye INCOMPATIBLE; si el operador pide compatibility=INCOMPATIBLE y mantiene ese toggle false, el resultado es vacío con la contradicción visible en UI, no se cambia el filtro. UNKNOWN no cuenta como ejecutable/recomendado.

certification_statuses distingue CERTIFIED (certificado externo vigente para ese fingerprint), UNCERTIFIED (sin certificado), EXPIRED y UNKNOWN (no se puede comprobar); un certificado inválido/revocado no satisface CERTIFIED. La confianza se filtra por TrustScore conocido y evidenciado; null no satisface un rango. Un dato ausente requerido por cualquier filtro se informa desconocido y no se hace pasar por coincidencia.

POST /models/search y /sources/search usan este DTO. Las listas CRUD mantienen sus filtros propios y no limitan Search. El source adapter puede soportar solo texto/capacidades: el Core aplica los demás filtros tras normalización sobre un snapshot federado o continúa obteniendo páginas upstream hasta completar la página filtrada. El cursor conserva cursores upstream y filterdigest de todos los filtros excepto cursor/limit; ordenar y filtrar globalmente antes de emitir una página. Si no puede garantizarse el resultado, informar fuente parcial/limitación, nunca declarar búsqueda completa sobre una sola página ni inventar metadatos. Un cambio de filtro obliga a cursor nuevo y un cursor con digest distinto devuelve error.

Aceptación: cada filtro y los tres toggles, AND/OR/rangos/unknowns, restauración de UI, dos páginas de dos fuentes y ausencia de duplicados/saltos; fuentes parciales se identifican y no producen recomendación falsamente completa.

## H06 — MCP con permisos separados (§92)

McpPrincipal tiene audiencia larm-mcp-admin, api_scopes y mcp_scopes. Estos últimos son exactamente read/search/download/install/start/stop/benchmark/move/delete/update/admin. [mcp-permissions.json](mcp-permissions.json) mapea cada herramienta a su permiso y a una operación API existente. Se exige el permiso MCP correspondiente Y el scope API. admin explícito permite herramientas MCP, pero no añade scopes API. Si falta cualquiera devolver FORBIDDEN antes del caso de uso. No conceder admin al bootstrap de MCP por defecto.

El transporte verifica credencial, audiencia y principal. La procedencia MCP se fija en el contexto autenticado, no por un header del cliente. Una credencial con audiencia MCP usada directamente ante API conserva las mismas restricciones de operación, o se rechaza si no existe mapeo; no permite convertir leases:manage en start+stop ni runtimes:write en install+update. El server invoca casos de uso con ese contexto; no usa un token de servicio amplio en nombre de un principal restringido. Un principal Broker conserva read+leases:manage y su propio contexto de cliente, sin confundirse con MCP.

Aprobaciones de movimiento/borrado siguen separadas y hashbound; permisos no eliminan licencias, ownership, leases ni guards. Para principales sin privilegios de administración, el getter de jobs solo expone los jobs visibles por su autorización, no secretos.

Aceptación: start permitido/stop denegado; install permitido/update denegado; read/search y move/delete separados; scope API ausente niega incluso con permiso MCP; intentos por API directa, headers simulados o credencial de otra audiencia no sortean estos controles.

## H07 — FAILED_RECOVERABLE persistente (§104)

TransformationJob.state y SQL admiten FAILED_RECOVERABLE. Reconciliación cambia un intento RUNNING/PAUSING/VERIFYING interrumpido a ese estado cuando no puede continuar de forma segura. Conserva error original redactado, input, plan y hashes, temporales y journal; nunca publica output parcial. Los eventos son transformation.state_changed y la GUI muestra fallido recuperable y Reiniciar.

Reiniciar requiere input original intacto y accesible, nuevo preflight/hash de plan, reservas y guards. Incrementa attempt y entra QUEUED en transacción; no reutiliza salida verificada incompleta ni retoma offsets de una herramienta no reanudable. Si input no es íntegro, mantiene el fallo con causa y bloquea el reinicio. Output provisional solo se limpia con ownership y journal verificados. COMPLETED/CANCELLED permanecen terminales; FAILED/FAILED_RECOVERABLE son fallos de intento reintentables según contrato 01.

Revisión3.13.0: reinicio explícito del mismo job mediante `POST /api/v1/transformations/{id}/restart`, con plan nuevo confirmado, `If-Match` e idempotencia ligados al trabajo. El primer claim inicial cuenta intento1; aceptar un reinicio incrementa al siguiente intento y su claim no vuelve a incrementarlo. Conservar el intento anterior y probar rollback/concurrencia/replay, además del crash/reinicio real del daemon. [Contrato completo](REVISION_3_13_0_P093.md).

Aceptación: crash injection, roundtrip del estado y evento, acción reiniciar con y sin input íntegro; hash del input preservado después del intento exitoso y del fallido.

## H08 — Descargas configurables (§44)

Settings.storage.download_bandwidth_limit_bytes_per_second es string decimal positiva o null (sin límite agregado de velocidad). Default null. Un límite por volumen o cuota diaria no lo sustituye. El DownloadManager limita conjuntamente todas las transferencias activas de LARM, incluyendo mirrors/reintentos, con budget compartido y ráfaga máxima equivalente a un segundo del límite; divide lecturas en bloques adecuados para no excederlo por tamaño de chunk. Cambios CAS en Settings afectan jobs activos a partir del siguiente bloque sin perder el offset durable. La reanudación/reinicio vuelve a leer el valor persistido. El reloj usado para el límite es monotónico.

DownloadPlanRequest.priority y DownloadJob.priority son BACKGROUND, NORMAL o HIGH; la UI y el plan automático envían NORMAL y BACKGROUND respectivamente por defecto. La prioridad queda en el plan aprobado y en el registro, no en metadata descriptiva. Cola ordenada HIGH>NORMAL>BACKGROUND y FIFO created_at,id dentro de cada grupo, conservando límites globales y por fuente. No interrumpir ni destruir una transferencia activa para adelantar otra; decidir en nuevas plazas/reanudaciones.

PATCH /downloads/{id}/priority {priority} con If-Match/jobs:write cambia QUEUED/PAUSED/PAUSING/RUNNING; no altera el plan físico ni las reservas. Registrar outbox y conservar la prioridad tras restart. VERIFYING o terminal devuelve INVALID_STATE_TRANSITION. Un cambio de prioridad en RUNNING no deshace progreso y rige su siguiente entrada a la cola. GUI Descargas expone prioridad y estado confirmado; Configuración expone el límite.

Aceptación: cola de prioridades y CAS stale; límite agregado con dos descargas contra servidor local controlado, cambio de configuración, pause/restart y verificación del hash final. Para medir el límite documentar tolerancia por tamaño de bloque y ventana de medición; mock o inspección de schema no acreditan throughput real.

## H09 — WARNING y CRITICAL configurables (§136)

Settings.storage.low_disk_thresholds admite {mode:PERCENT,warning_basis_points,critical_basis_points} o {mode:BYTES,warning_bytes,critical_bytes}; no mezclar modos. 100 basis points = 1 %. Exigir critical<warning, critical>=0 y límites de porcentaje <=10000. Bytes son strings exactas. Default de esta revisión PERCENT/1500/500 reproduce el ejemplo como elección editable del paquete, no como requisito obligatorio de la especificación. GUI admite porcentajes o cantidades absolutas y muestra unidades exactas.

Evaluar espacio libre físico observado del volumen, sin restar dos veces reservas o márgenes. Comparar estrictamente free/capacity contra el umbral elegido con aritmética comprobada; igualdad no cruza. ERROR/OFFLINE/capacidad desconocida no inventan nivel. Niveles NORMAL, WARNING, CRITICAL: crítico prevalece, warning si solo cruza aviso. Deducir una única alerta por volume_key y transición; roots del mismo volumen no duplican. Emitir LOW_DISK con data.level=WARNING/CRITICAL, severidad warning/error respectivamente; recuperación emite info. El nivel se deriva y puede almacenarse en el snapshot/evento, sin reemplazar la identidad ni la reserva de StorageLocation.

WARNING por sí solo no invalida un preflight que cumple reserva. CRITICAL muestra LOW_SPACE y pausa nuevas asignaciones como el contrato 02; no borra modelos ni cancela trabajos activos automáticamente. Al modificar umbrales reevalúa y registra transición con la revisión de Settings. Alertas no equivalen a la reserva mínima de admisión.

Aceptación: configurar ambos modos, validación de orden y unidades, igualdad/cruce/recuperación, volumen offline, dos roots y deduplicación; cambios por Configuración producen los niveles correspondientes sin IO destructivo.

## Revisión 3.12.0 — primera adquisición

Probar snapshots íntegros sin bytes ficticios, IDs centrales, certezas, ingesta/outbox atómicos y primera descarga tras confirmación. Cambio/revocación invalida el plan. VERIFIED y disponibilidad se acreditan tras bytes y gates. Reglas completas: [revisión 3.12.0](REVISION_3_12_0_P044_P052.md).

## Revisión 3.12.1 — binding del plan source

La firma source.download_plan recibe SourceDownloadPlanRequest con acquisition opcional; referencia pública, hash upstream y mapa de IDs centrales permiten seleccionar una revisión sin consultar IDs privados. Selección y copia exacta source_acquisition_v1 quedan selladas. La petición HTTP del usuario se conserva. [Reglas y oráculos](REVISION_3_12_1_SOURCE_ACQUISITION.md).
