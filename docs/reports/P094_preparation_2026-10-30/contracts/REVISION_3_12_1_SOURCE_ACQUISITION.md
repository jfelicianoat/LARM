# Revisión contractual 3.12.1 — binding público del plan source

Fecha: 2026-10-27. Predecesora: 3.12.0. Concreta la implementación del recorrido catálogo→primera adquisición autorizado expresamente por el operador en 3.12.0. No modifica la petición HTTP del usuario ni las reglas de verificación física.

## Diferencia comprobada

El contrato anterior source.download_plan sólo llevaba variant_id central y opcionales. Un worker con varios modelos no podía deducir la revisión del Core a partir de un UUID que no creó. Consultar su ledger privado desde el Core o reemplazar el UUID central por uno del plugin violaría la separación aprobada en 3.12.0. Se proporciona ahora un binding público explícito.

## Petición y selección

La firma source.download_plan usa SourceDownloadPlanRequest. Conserva los campos existentes y añade acquisition opcional, no nullable, de tipo SourceAcquisitionBinding. La petición HTTP DownloadPlanRequest permanece intacta y no acepta este campo del cliente. storage_location_id es UUIDv7 obligatorio no nullable en la firma source, conforme al DTO ya existente.

SourceAcquisitionBinding = {revision,manifest_hash,artifacts}; revision tiene los cuatro campos de SourceCatalogReference. artifacts contiene de 1 a 4096 objetos cerrados {artifact_id,relative_path,required}. Son IDs del Core; no son IDs privados del worker ni conceden permisos. Rutas POSIX seguras, únicas case-insensitive, IDs UUIDv7 únicos, al menos un required y orden por relative_path/artifact_id. La lista cubre exactamente todos los archivos del manifiesto fijado, incluidos los opcionales no seleccionados, con el mismo required. source_id del request debe estar presente y coincidir con revision.source_id. Una revisión pending no permite esta adquisición.

El worker resuelve la referencia pública, obtiene el manifiesto de esa revisión y exige revision_key y manifest_hash iguales. Comprueba la biyección de rutas y required antes de producir el plan. Selecciona todos los required y sólo los opcionales cuyos IDs centrales aparecen en optional_artifact_ids; IDs repetidos, ajenos o required en esa selección fallan cerrados. Ausente o vacío selecciona cero opcionales. Preserva variant_id central, destino, presupuesto y prioridad; nunca cae a su búsqueda por UUID privado cuando acquisition está presente.

El descriptor completo, SHA y tamaños de cada archivo seleccionado proceden del manifiesto observado, no del mapa de IDs. El Core conserva adicionalmente su snapshot y vuelve a comparar sus descriptores al consumir el plan. No mezcla hashes upstream/snapshot/bundle.

## Sello y compatibilidad

El único paso de descarga conserva source_download_v1 y, si se recibió acquisition, también source_acquisition_v1 con copia exacta del binding recibido. Ambos quedan cubiertos por el SHA del Plan. La validación comprueba referencia y conjunto seleccionado, igualdad del binding y hash del plan. Un binding omitido, cambiado, añadido o ignorado invalida la respuesta. No se permite retry sin acquisition para sortear ese rechazo.

Sin acquisition se conserva el recorrido legado de cada adaptador, incluido su rechazo explícito de opcionales que no pueda resolver. Workers antiguos pueden seguir atendiendo peticiones antiguas; no se les supone soporte del recorrido nuevo. Un worker que rechaza o ignora el miembro nuevo falla explícitamente y no habilita descarga. No cambia el envelope NDJSON, los grants ni el major del protocolo. La aceptación del recorrido requiere las pruebas actuales y sus gates P022/P026.

El estimate de descarga usa sólo los archivos seleccionados. Un intervalo no inventa un punto; HIGH/ESTIMATED conservan certeza y cotas y UNKNOWN requiere presupuesto explícito. El presupuesto se compara con los archivos seleccionados, nunca con opcionales excluidos, y los guards Core siguen aplicándose durante cada bloque.

## Oráculos

Probar UUID central distinto al del worker con al menos dos variantes; revisión fijada aunque exista otra más reciente; mapa completo y opcionales independientes del orden; hash, referencia, fuente, ruta, required e IDs ajenos; replay/sello, binding omitido/ignorado y compatibilidad antigua. Preparar el plan no transfiere bytes ni crea un job/reserva Core. Probar adaptadores Rust/Python y worker protegido real antes de dar por cerrado el recorrido. Mantener ingesta, revalidación, SHA/CAS/procedencia, reinicio y GUI pendientes hasta ejecutarlos.
