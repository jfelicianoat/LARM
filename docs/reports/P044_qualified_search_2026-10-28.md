# P044 — búsqueda HTTP cualificada e ingesta P041

Fecha: 2026-10-28. Estado: incremento funcional verificado, regresión focal y ensayo nativo PASS. P044/P052 y la aplicación completa siguen `BLOCKED_REAL`; rendimiento con defaults pendiente.

## Implementación

El daemon compone `QualifiedSourceSearch` en sus rutas de búsqueda. Cada fuente debe estar habilitada, conservar su instalación cualificada actual, declarar los métodos de metadatos y corresponder al ID y URL de su perfil público aprobado. Se comprueba también el contexto físico de cualificación. Las consultas usan el actor backend del Run vigente; no amplían permisos del cliente HTTP ni convierten metadatos en aprobación de ejecución.

El proveedor resuelve únicamente las raíces y orígenes declarados. Este incremento admite perfiles de fuente compilados compatibles y autenticación NONE sin cuenta; otras modalidades de autenticación fallan explícitamente, sin recuperar secretos ni conceder permisos de proceso. Un catálogo local/import-only o una fuente ausente no simula resultados remotos. Una fuente fallida puede coexistir con resultados de otra, con aviso público redactado.

Cada página obtiene `source.search`, la inspección pública y el manifiesto fijado. Tras cerrar y liberar el worker, el preparador P041 resuelve las identidades y registra el grafo remoto y snapshot mediante el writer atómico. No consulta IDs del ledger privado ni prepara modelos por SQL. La proyección incluye IDs centrales, binding del snapshot y estimaciones tipadas. Suma sólo puntos explícitos de archivos requeridos, conserva HIGH/ESTIMATED/UNKNOWN y la precisión decimal; intervalos sin punto siguen sin punto. Los opcionales no se seleccionan automáticamente. No inventa scores, compatibilidad, certificación, disponibilidad ejecutable ni recomendación.

La configuración de la fuente se revalida después del RPC y su `row_version` se coteja de nuevo bajo el lock de escritura SQLite. Si cambió o fue deshabilitada, las observaciones antiguas no se reintentan como si fueran actuales. Esta condición se incorpora al DTO interno de observación; no cambia contratos públicos ni migraciones.

Las cachés incluyen una huella de configuración, instalación y Run. Cada página global, incluso la primera, vuelve a contrastarla. Un cambio durante la consulta, una fuente deshabilitada o la pérdida del contexto físico impiden reutilizar un resultado positivo anterior. Se invalidan las consultas de las fuentes afectadas. Una consulta correcta sin resultados y con limitaciones declaradas sigue siendo un resultado vacío válido; un fallo real de fuente no se confunde con ese caso.

La exclusión por instalación se conserva durante arranque, RPC, cierre e ingesta. Una tarea propietaria termina el worker aunque la cancelación o timeout del observador descarte el future de búsqueda. El permiso no se libera antes del cierre físico ni se pierde entre cierre e ingesta.

## Regresión focal

Pasan **170 pruebas Rust únicas**: biblioteca modelos 45 (incluidas 13 de búsqueda), ingesta 12, bundles 8, registro 6, proveedor daemon 6, DB lib 77, snapshots 7, grafo atómico 8 y frontera API 1. Cubren revisión de fuente cambiada tras RPC sin escrituras parciales, caché/cursor revocados, cambio durante fetch, resultados restantes de otra fuente y vacío válido con limitaciones. La base recién migrada incluye su fuente local automática: la prueba respeta esa configuración y verifica que no abre un worker remoto ni publica modelos.

Pasan Clippy de DB/modelos/daemon en todos los targets con `-D warnings`, formato, arquitectura (17 crates), OpenAPI y **78 copias** del paquete **3.12.1**. Los avisos del sistema de archivos sobre hard links de la caché incremental no impiden las comprobaciones. No se ejecuta aquí la suite workspace completa ni frontend/GUI.

Se conservan las ejecuciones iniciales fallidas: dos expectativas de los nuevos tests de búsqueda (número de llamadas por paginación y motivo público redactado) y otra que suponía que la base migrada no contenía la fuente local automática. Se corrigieron los oráculos; no se alteró el comportamiento productivo para satisfacerlos. La regresión final pasa.

El primer ensayo nativo terminó **0/1 en 399,41 s** por la última expectativa del test: esperaba HTTP 502 al deshabilitar la fuente, mientras el catálogo contractual asigna **503** a `UPSTREAM_UNAVAILABLE`. El cuerpo observado fue `all_sources_unavailable`. Todas las aserciones previas habían pasado, pero ese intento permanece FAIL y no emitió recibo PASS. Las 2.016 entradas congeladas coincidieron al terminar. Se corrigió exclusivamente ese oráculo, se añadió medición de tiempos al test, se recompiló y pasó de nuevo Clippy. La repetición completa pasa; no se modificó código productivo entre ambos ensayos.

## Ensayo nativo

El [ensayo completo](P044_runs/qualified-search-20261028/native-search-http-status.log) pasa **1/1 en 285,52 s**. Usa el paquete original de referencia, la receta completa P026 (**99 casos y 21 observaciones**), una fuente creada por Core antes de cualificar y el router HTTP real autenticado dentro del proceso. Los **2.016 inputs públicos y dos ejecutables** del [checkpoint](P044_runs/qualified-search-20261028/implementation.http-status-before-native.sha256.json) coinciden antes y después. No hubo compilación ni cambios de esos inputs durante el ensayo.

Las tres páginas globales responden HTTP 201 y presentan seis modelos distintos, con variantes centrales distintas de los UUID privados del worker. La primera consulta registra **6 modelos, 6 variantes, 6 artefactos y 6 snapshots**. El replay idempotente y las siguientes páginas no añaden escrituras/eventos. Alterar el sentinel inmutable de la cualificación después de cerrar el worker hace que el cursor anterior responda **409 `search_source_snapshot_changed`**. Restaurarlo permite una consulta nueva filtrada (201), reutilizando las entidades centrales y creando sólo otro snapshot. Deshabilitar la fuente mediante Core provoca **503 `all_sources_unavailable`** en una consulta nueva. Al final hay 7 snapshots y 44 eventos/outbox frente a los 42 posteriores a la primera consulta: el nuevo snapshot y el cambio de configuración explican las dos escrituras adicionales.

Bundles físicos, ubicaciones, jobs de descarga, reservas y procedencia Core permanecen en **cero**. La traza HTTP del intervalo de búsqueda queda **28→28**, sin lecturas `/artifact/`; las transferencias privadas anteriores pertenecen a P026. Los tamaños escalares sin certeza tipada siguen UNKNOWN. El recibo no acredita daemon TCP, GUI, autenticación privada, upstreams generales ni primera descarga Core.

El oráculo de ausencia de lectura se limita a las peticiones HTTP `/artifact/`. Las comprobaciones físicas de cualificación sí releen sus inputs inmutables, incluidas las copias fixture fuera de los grants; no se presenta esa traza como un seguimiento de todo el acceso al sistema de archivos.

[Resultado y traza](P044_runs/native-qualified-search/01a18123-3fdd-74e2-95d7-0a9c1fb245a4/result.json) · [Resumen y recibo P026](P044_runs/native-qualified-search.json) · [Comprobaciones agregadas](P044_runs/qualified-search-20261028/increment.checks.json). Run: `01a18122-a34c-75d9-9ffa-b41e79fc9f72`.

El harness configura timeout de fuente **300 s** y caché **600 s** para medir el recorrido con las comprobaciones físicas nativas. El servicio habitual conserva **30 s** en ambos valores. La primera consulta del ensayo PASS tarda **169.891 ms**; el ejercicio HTTP completo, **222.401 ms**. Se ejecutó un binario debug sin optimización; no se extrapola esa latencia a un binario de producción optimizado. Este ensayo no valida el rendimiento ni la primera búsqueda con los defaults productivos. Es necesario medir el recorrido optimizado y, si procede, reducir el coste de medición/ingesta; también comprobar la cancelación nativa dentro del plazo habitual sin debilitar la cualificación. No se cambian silenciosamente los defaults para obtener un PASS.

## Límites y continuidad

Faltan la preparación productiva de perfiles para todas las fuentes, autenticación de cuenta en las consultas y pruebas con upstreams adicionales. El camino de ingesta actual crea/reutiliza variantes remotas iniciales; no reconcilia todavía variantes con bundle físico verificado. El contrato de `Page` no tiene un lugar para avisos informativos cuando la página está vacía; se conservan internamente y no se añade un campo público fuera de contrato.

El preflight/confirmación/consumer de descarga todavía debe consumir este snapshot remoto en su recorrido completo y reconciliar archivos realmente verificados, sin confundir hashes upstream, snapshot y bundle. Permanecen pendientes la aceptación GUI y el resto de fuentes, runtimes y dependencias de LARM. Este incremento no cierra P044/P052 ni el objetivo global.
