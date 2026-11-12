# Propuesta 3.12.0 — Del catálogo a la primera descarga

Estado: **AUTORIZADA por el operador el 2026-10-27; revisión 3.12.0 publicada; implementación en curso**.

La resolución del operador autoriza separar manifiesto remoto y bundle físico. La primera adquisición todavía requiere publicar e implementar esta revisión con sus pruebas.

## Problema comprobado

La búsqueda del daemon está conectada a `UnqualifiedSourceSearch`, que devuelve siempre `source_search_backend_unavailable`. Los adaptadores sí ofrecen búsqueda, inspección y manifiestos, pero falta la composición que publique sus resultados como modelos centrales.

Además, la primera descarga está bloqueada por una contradicción de contratos:

- [Contrato 01, línea 5](../contracts/01_DOMAIN_STATES.md): «Un shard faltante impide VERIFIED». La línea 7 ordena `DOWNLOAD_PLANNED → DOWNLOADING → DOWNLOADED → VERIFIED` y define VERIFIED como todos los hashes y bundle comprobados.
- [Contrato 02, línea 27](../contracts/02_STORAGE_DOWNLOAD.md) exige artefactos del «bundle verificado» para preparar el plan. El planner, activación y ejecutor exigen `BundleState::Verified`.
- [Construcción del registro](../../crates/larm-models/src/bundle.rs) deja el bundle INCOMPLETE hasta comprobar bytes locales; sin hash/tamaño de un artefacto, todavía no crea bundle físico. Por tanto, un modelo remoto nuevo no puede alcanzar su primera descarga siguiendo ambas reglas. Los ensayos anteriores preparaban el catálogo como fixture con bundle VERIFIED; su prueba de transferencia sigue siendo válida y no demuestra esta primera adquisición.

El [contrato 00](../contracts/00_PACKAGE.md) exige: «Ante contradicción real registrar BLOCKED con los dos textos y solicitar resolución». La respuesta explícita del operador resuelve la contradicción a favor de la separación propuesta; no se relajan guards por interpretación local.

## Cambio propuesto

Separar el manifiesto de adquisición del estado físico del bundle. Un manifiesto describe qué debe descargarse y qué revisión se fijó; un bundle VERIFIED acredita los bytes que ya se comprobaron. Planificar no marca archivos como descargados, verificados ni ejecutables.

1. Incorporar `SourceCatalogReference`, con los cuatro campos ya definidos por `RevisionRef`: `source_id`, `upstream_namespace`, `upstream_model_id`, `revision_key`. Un adaptador puede incluirlo en `metadata.source_v1.normalized.catalog_reference` de cada variante de búsqueda. Las variantes sin referencia continúan siendo respuestas de protocolo válidas; el registro devuelve una limitación explícita y no inventa una referencia a partir de UUIDs privados.
2. Añadir el schema cerrado `ModelAcquisitionManifest` y una tabla `model_acquisition_manifest` en la migración 0014. Guardar IDs centrales de variante/revisión/fuente, referencia source, hash del manifiesto upstream, hash canónico del snapshot, instante observado, IDs requeridos/opcionales y descriptores con `ArtifactSizeEstimate`. Conservar también instalación y ejecución P026 que originaron el snapshot. La identidad y el contenido son inmutables por snapshot; una actualización crea otro.
3. La ingesta cualificada resuelve identidad P041 y conserva la revisión, los tamaños exactos/estimados/desconocidos y su evidencia. No crea localizaciones de archivos ni eleva confianza física. Publica el catálogo con eventos/outbox y transacción; las referencias y IDs son del Core, separados de los IDs privados del plugin.
4. Preview y ejecución se ligan al snapshot de adquisición y vuelven a comprobar fuente, revisión, artefactos, opcionales, política y cualificación. El plan sella el hash del snapshot y el hash upstream por separado. Dejan de exigir bundle físico VERIFIED antes de descargar. Un bundle CORRUPT no se considera reparado por preparar otro plan.
5. Se mantienen preflight físico, reserva, presupuesto duro para UNKNOWN, confirmación expresa, idempotencia, guards, hash final, procedencia y recuperación. Al terminar los bytes, se construye/reconcilia el bundle físico con hashes/tamaños observados; verificarlo y habilitar ejecución siguen sus gates. Un manifiesto remoto nunca equivale a disponibilidad ejecutable.
6. La ausencia de credenciales, licencia o referencia suficiente conserva un error explícito. La primera implementación no inventa soporte privado y no usa secretos en metadata o formularios.

## Archivos normativos previstos

Contratos 01/02/03/07/11, schemas canónicos y de persistencia, migración 0014, descriptores de métodos source para la referencia opcional, prompts P040/P041/P043/P044/P051/P052/P122 y sus entradas en el manifiesto del paquete. Publicar ahora las copias y schemas derivados conforme a la autorización recibida.

## Verificación exigida para implementarla

- Un modelo nuevo sin bytes locales aparece con tamaño y procedencia disponibles, sin VERIFIED ni ubicación ficticios.
- Preview sin destino compara opciones; preparar/rechazar confirmación no transfiere bytes ni crea jobs/reservas.
- Aceptar crea una sola descarga; cambios de snapshot/revisión, opcionales ajenos, fuente revocada y hash contradictorio fallan antes de publicar.
- EXACT/HIGH/ESTIMATED/UNKNOWN conservan su semántica; UNKNOWN sólo se ejecuta con presupuesto explícito.
- Primera adquisición por daemon y worker cualificados, SHA/CAS/procedencia, pausa/reinicio, rollback de ingesta y concurrencia/idempotencia del catálogo.
- Una referencia source ausente, malformada o ajena no produce identidad inventada ni un catálogo vacío presentado como éxito remoto.
- UI → daemon: descubrir, elegir variante, comparar/preparar destino y confirmar descarga, con loading/error/offline y estados durables.

La aprobación recibida autoriza publicar la revisión contractual e implementarla con estas pruebas. P043/P044/P051/P052 y el producto completo seguirían sin REAL_PASS hasta superar todas sus aceptaciones y dependencias.

Publicación y primera fase verificadas: [informe 2026-10-27](P044_revision_3_12_0_2026-10-27.md). La implementación completa y sus aceptaciones continúan pendientes, sin repetir la autorización ya recibida.
