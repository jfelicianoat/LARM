# P043 — ModelSourceAdapter protocol y catálogo de fuentes

**Estado calculado: BLOCKED_REAL.** El protocolo, catálogo persistente, API CRUD y pantalla Fuentes están implementados y probados. Pasan **17 grupos de evidencia y las 13 puertas generales**, sin fallos. Las dependencias P020/P040/P041 no tienen REAL_PASS; el contrato 00 impide completar el consumidor. No se acredita una fuente remota ni se retiran sus propietarios.

[Protocolo y límites](../adapters/source-v1.md) · [Evidencia completa](P043_modelsourceadapter-protocol-y-catalogo-de-fuentes.evidence.json) · [Puertas generales y reales](check_P043_real.json) · [HTTP real](P043_runs/sources-real.json) · [Interfaz con daemon real](P043_runs/ui-real.json)

## Resultado implementado

SourceRequest/SourceReply materializan las nueve firmas de adapter-methods.json con los DTO exactos y sin un nuevo tag en el wire. Se validan campos cerrados y required nullable, UUIDv7, fechas UTC, hashes, referencias, rutas y duplicados. El roundtrip impide que tipos compartidos oculten campos faltantes como null. ModelInspection/ArtifactManifest conservan tamaños y hashes desconocidos sin sustituirlos por cero. SourceHandler valida antes del callback y vuelve a validar la respuesta; SourceWorker repite la comprobación al recibirla.

SearchSupport conserva Search completo de H05, filtros diferidos y limitaciones. Sólo transmite texto/capacidades anunciados, recibe el cursor upstream por separado y no lo confunde con el cursor federado. Digest, rangos decimales, toggles, datos desconocidos y contradicciones se mantienen. Filtros no anunciados e import-only devuelven UNSUPPORTED_CAPABILITY; no hay una página vacía que finja soporte. El postfiltrado y la búsqueda global mantienen su propietario P044.

SourceMetadata guarda original redactado, normalized, limitations y rate_limit en metadata.source_v1 sin borrar otros namespaces. Se conservan campos de modelo nuevos y null; se redactan claves sensibles y patrones conocidos, sin admitir headers/credenciales como metadata. RateLimit conserva el Retry-After observado; el delay local se acota a 60 s. Un 429 usa el Error publicado con original_error tipado, incluso sin items, y no un éxito vacío. Un límite desconocido sigue siendo null.

SourcePagination valida toda la página antes de avanzar y conserva digest/snapshot/cursores/IDs. Cambiar filtro, repetir cursor/ID o cambiar snapshot rechaza sin consumir el cursor. Terminar una fuente no acredita haber completado la búsqueda federada. list_revisions conserva la firma UpstreamRef sin inventar un cursor de request: RevisionPage puede expresar una continuación que esa firma no permite solicitar de forma general; se documenta la limitación.

El paso abierto Plan.steps[].request contiene SourceDownloadSpec con fuente/revisión, variante/destino, artefactos/tamaños/hashes, auth/licencia, reanudación, prioridad y presupuesto. El hash canónico cubre todo el plan excepto su propio campo. Se comprueba binding a la request, suma sin overflow y coherencia de confianza. Los helpers bloquean auth/licencia pendientes y UNKNOWN sin presupuesto. El estimate físico procede de su preflight; no se simulan reservas, journal, descarga, expiración ni checksum de P050.

SourceWorker conserva P021/P022: firma y hash del paquete comprobados por Policy, permisos Access antes de enviar, timeout, cancelación y lifecycle. Se corrigió un defecto del host común encontrado al probar IDs explícitos: pending.insert podía reemplazar el receptor de una llamada activa. Ahora un duplicado devuelve CONFLICT request_id_in_use antes de escribir, conserva el original y su cancelación y no deja una entrada si falla la codificación. Las regresiones del workspace pasan.

## Catálogo, API e interfaz

Las tablas canónicas de 0001 materializan model_source_adapter/model_source sin nueva migración. Registrar una instalación source INSTALLED/VALIDATED no revocada deriva las capacidades del manifiesto y negocia SearchSupport, sin ejecutarla ni promoverla. Binding, índice descriptivo metadata.source_bindings de GET /adapters, revisión de la instalación y eventos se publican en una transacción. Un replay idéntico no altera el catálogo. El índice sólo sirve al selector; FK tipado, estado y confianza siguen siendo la autoridad.

GET/POST /sources y GET/PATCH/DELETE /sources/{id} usan scopes del catálogo canónico, UUIDv7, DTO cerrado con account_id obligatorio nullable, filtros literales/capability y cursor firmado. POST conserva 30 días el 201 original por actor/clave/cuerpo exacto, incluso tras editar/borrar la fuente o su binding. PATCH/DELETE usan If-Match/CAS. Mutación, idempotencia, evento y outbox son atómicos; no requieren reserva/journal. Guardar configuración no lee ni descarga una fuente.

Se rechazan credenciales en URL/policy, autenticación HTTP no loopback, rutas relativas/traversal/ADS/dispositivos, cuentas ajenas, borrar o cambiar identidad con referencias, e intentar activar sin instalación VALIDATED. Se permite desactivar tras revocación. Quitar configuración no borra modelos, archivos ni credenciales. Los eventos no publican nombres, direcciones ni contenidos de la política.

Fuentes permite listar, buscar, paginar, añadir, editar, confirmar eliminación y actualizar. Carga todas las páginas de conectores y oculta UUID técnicos. Guarda desactivada mientras el conector espera conformidad; no simula P045. Conserva account_id/policy al editar, el borrador tras error y la clave de alta al reintentar el mismo contenido. Un CAS obsoleto no se reintenta automáticamente. Cancelar relee cambios de otro actor. Usa el transporte nativo existente: ningún token administrativo entra al frontend.

## Pruebas ejecutadas (2026-10-17)

`tools/p043_evidence.py`: **17 grupos PASS**; transcripciones, comandos y hashes en PromptEvidence. El recibo de preparación anterior se conserva como histórico y su entrada ahora redirige al runner completo.

| Batería | Resultado |
|---|---|
| Frontera SDK Search/SourceSearch | 7 PASS |
| Protocolo SDK | 11 PASS: nueve firmas, requests/replies cerradas y nullable required, metadata parcial, 429, paginación, rutas hostiles, plan/hash/presupuesto/confianza/overflow |
| Search completo | 5 PASS, incluido rechazo sistemático de cada clave ausente |
| Catálogo de dominio y normalización Core | 2 + 2 PASS |
| SQLite real | 8 PASS: concurrencia, CAS, replay 30 días y original, actores, cuentas/referencias, cualificación/revocación, filtros/reapertura, outbox/idempotencia/binding e índice atómicos |
| Host y worker reales | 6 PASS en Windows, con paquetes firmados y comprobación real de elevación |
| HTTP/CLI/SQLite con daemon real | 17 pasos PASS: permisos/MCP/DTO sin efectos, binding visible, altas y CAS simultáneos, reinicio, replay tras borrados y triggers de fallo con rollback total |
| Formularios React | 8 PASS: reintentos/clave, borrador/CAS, política/cuenta, conectores/páginas, eliminación y permisos |
| Navegador → daemon/SQLite real | alta 201, edición 200 y eliminación 204; revisiones/eventos/outbox 1-2-3; principal revocado y daemon propio cerrado |
| Schemas canónicos independientes | 18 documentos válidos y 102 negativos PASS |
| Fixture/hashes independientes | Reproducción Python PASS; hash del plan contrastado contra el SDK |
| Puertas generales y runner | 13/13 PASS: formato, clippy estricto, workspace, arquitectura/copias, escritorio, frontend y evidencia real |
| Lock | 46 dependencias coherentes |
| Secretos | ambos recuperables de Credential Manager; ausentes de los 1.572 archivos inspeccionados |

Son **41 pruebas focales Rust y 8 de interfaz**, además de los 17 pasos HTTP y las regresiones completas. Las seis pruebas del host ejecutan fake_source en procesos separados: dos páginas y metadata desconocida, gates de filtro/fuente/permisos antes del callback (contador observado), import-only, 429/auth/plan y respuesta incorrecta detectada en ambos extremos, timeout y cancelación cooperativos, crash y arranque de otra generación, cancel desatendido con kill tras gracia e ID RPC duplicado sin perder la respuesta original. Después de stop se rechaza una llamada sin nuevo IO.

La fixture es de protocolo. source.download devuelve explícitamente UNSUPPORTED_CAPABILITY fixture_read_only; el JobReceipt válido de la fixture sólo se usa para comprobar representación. No hay descarga exitosa simulada, cualificación de un upstream, resolución real de credenciales, licencia aceptada ni reserva de disco acreditada por estos casos. El volumen del estimate es una fixture, no un volumen observado por el SO.

La revisión CUA abrió el bundle compilado, creó «Fuente QA real», cambió su nombre y confirmó su eliminación, observando cada estado en la UI. Un puente QA aislado mantuvo su token sólo en el servidor y remitió las llamadas al daemon real; SQLite confirmó resultados tras cada mutación. El recibo se liga a hashes de fuentes/bundle/daemon y se vuelve a comprobar contra SQLite/outbox. Esa comprobación valida el recibo, no reproduce las interacciones. **No acredita interacción en el WebView nativo.** La instalación inicial es INSTALLED/UNTRUSTED sembrada como fixture porque importar ZIP es P028; no implica P026.

Se revisaron lista/formulario y direcciones largas, teclado/foco y ventanas de 375/800 px: sin desplazamiento horizontal observado. Las capturas visuales con fixtures se etiquetan explícitamente QA. A 800 px el texto secundario observado fue rgb(196,202,211) sobre rgb(27,31,36), tamaño 18 px, separación de formulario 9 px y foco amarillo de 3 px. El detector sólo señaló el borde lateral ya existente de `.device` (styles.css:153), preservado como parte del shell.

Capturas: [alta real](P043_runs/ui-real-created.png), [eliminación real](P043_runs/ui-real-deleted.png), [formulario 375 px](P043_runs/ui-form-375.png), [formulario 800 px](P043_runs/ui-form-800.png).

## Bloqueos y siguiente incremento

Sólo las dependencias P020/P040/P041 impiden REAL_PASS de este prompt. La búsqueda federada mantiene P044, cuentas/credenciales P045 y descarga real P050; las fuentes concretas conservan sus propietarios. La activación VALIDATED usada como precondición en un test SQL tampoco acredita P026. El siguiente incremento independiente en execution_order es **P025**, SDK Rust/Python y adaptadores de referencia, conservando estos bloqueos y los de sus propias dependencias.

Avance P044/P041 de ingesta 2026-10-27: preparador P041 compartido y writer atómico conectados; duplicados recíprocos incluidos en el commit y evidencia de hashes requeridos incompletos/históricos rechazada para auto-merge. 117 pruebas focales y nativo original P022/P026 1/1 (178,60 s) con 99 casos/21 observaciones. Fuente creada deshabilitada por Core, perfil fixture ligado a ese UUID antes de cualificar, habilitación posterior y seis modelos/variantes/artefactos/snapshots desde RPC público, sin seed SQL de modelos ni estado físico ficticio. 2.014 inputs/ejecutables coinciden. La preparación de perfiles es todavía explícita en el harness; falta componer búsqueda HTTP y primera adquisición Core/GUI. [Informe](P044_P041_ingestion_2026-10-27.md).

Avance P044/P041/P043 (2026-10-28), `BLOCKED_REAL`: el daemon compone el proveedor cualificado de búsqueda con RPC público e ingesta P041 atómica; huellas de fuente/cualificación invalidan cachés y cursores, y la transacción rechaza observaciones de una configuración antigua. Conserva certezas/intervalos e IDs centrales sin inventar elegibilidad. Pasan 170 pruebas Rust focales únicas y nativo HTTP autenticado dentro del proceso 1/1 en 285,52 s: P026 original 99 casos/21 observaciones, tres páginas de seis modelos, replay sin escrituras, revocación física 409 y fuente deshabilitada 503, sin bundles/descargas ni nuevas peticiones HTTP /artifact/ durante búsqueda. Coinciden 2.016 inputs/ejecutables; Clippy/formato/arquitectura/OpenAPI y 78 copias PASS. El ensayo usa timeout300s/caché600s; primera consulta169,891s. El default productivo sigue30s y su rendimiento/cancelación nativa NO están validados. Pendientes optimización, perfiles de todas las fuentes, autenticación de cuenta, primera adquisición Core completa, GUI y restantes fuentes/runtimes; el objetivo global sigue activo. [Informe y recibos](P044_qualified_search_2026-10-28.md).
