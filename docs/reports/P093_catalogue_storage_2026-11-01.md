# P093 — catálogo, archivos de entrada y almacenamiento observado

Fecha: 2026-11-01. Contrato vigente: 3.13.0. Objetivo global de LARM activo.

El lector de entradas ya resuelve el catálogo en una transacción de lectura, conserva la identidad del bundle y distingue las declaraciones de origen de los bytes adquiridos. Los tamaños o hashes de origen desconocidos requieren evidencia histórica sellada de una descarga completada. Un hash de `artifact_location` por sí solo no los convierte en hechos de origen. Las lecturas no crean trabajos, reservas, locks de ejecución ni eventos.

Los archivos compartidos necesitan `artifact_reference` explícita, comprobada también al confirmar el plan. Su papel requerido u opcional pertenece al bundle consumidor. Se verifican tanto el hash del manifiesto completo como el hash de los descriptores requeridos. La proyección de descargas mantiene sus restricciones originales de propietario y selección exacta. En referencias con declaración desconocida se buscan bundles históricos verificados del propietario y su evidencia de adquisición; evidencia ausente, alterada o contradictoria falla. Dos descriptores físicos distintos para el mismo ID desconocido conservan la ambigüedad: no se elige uno arbitrariamente. Las declaraciones de origen no se sobrescriben.

El selector físico prueba las copias elegibles en orden, comprueba tamaño, SHA-256 y EOF y retiene los archivos y sus ancestros durante la admisión. Puede elegir una copia sana cuando la preferida está corrupta. Dos archivos lógicos distintos pueden compartir el mismo blob CAS verificado; los nombres lógicos duplicados siguen rechazándose. Una selección ya confirmada no incorpora opcionales instalados después. El lector independiente mantiene las comprobaciones disponibles aunque estén ocupadas las conexiones normales de SQLite y exista un escritor.

El observador de espacio consulta el proceso real `probe-storage`, comprueba actualidad, identidad, estado y bytes libres por volumen y aplica la política vigente. Usa rutas Volume GUID para el acceso local y retiene las carpetas de destino y trabajo. Su prueba de escritura crea y elimina únicamente un temporal propio. La reserva definitiva sigue perteneciendo a la admisión transaccional existente. Un volumen ausente, ambiguo, de sólo lectura o con espacio desconocido no se transforma en un destino válido.

## Pruebas

Verificación final congelada: **10/10 comprobaciones PASS; 59 pruebas PASS, 0 FAIL, 0 ignoradas**.

| Grupo | Pruebas PASS | Alcance |
|---|---:|---|
| Catálogo SQLite | 3 | Evidencia sintética de adquisición, referencia y papel consumidor; corrupción, incompletitud y ambigüedad |
| Admisión y reinicio SQLite | 31 | Incluye 10 casos de reinicio y la referencia exigida en confirmación; guardias de observación de prueba |
| Entrada física | 8 | Archivos Windows, hashes, pins, escritor previo, aliases/CAS y recuperación de handles |
| Archivos y directorios del host | 4 | Lectura retenida, prueba de escritura acotada y liberación efectiva para renombrar |
| Fronteras de rutas | 3 | Negativos de rutas y junctions Windows |
| Entorno de almacenamiento | 10 | SQLite, archivos locales y proceso nativo de sondeo; topologías y fallos inyectados expresamente |

Clippy pasa para DB, transformación, host y daemon con todos sus targets y avisos como errores. Formato, arquitectura y copias contractuales pasan. Auditoría independiente: **477 entradas, 10 logs y 10 recibos** coinciden por SHA-256; el wrapper terminó con código 0 y no hubo cambios en sus fuentes durante la ejecución.

[Resultados finales](P093_runs/catalogue-storage-20261101/attempt1/verification-synopsis.json) · [auditoría](P093_runs/catalogue-storage-20261101/attempt1/post-run-audit.json) · [ejecutor reproducible](P093_runs/catalogue-storage-20261101/verify.py).

## Fallos conservados y correcciones

La reproducción CAS inicial rechazaba dos archivos lógicos que compartían un blob; se corrigió esa restricción y se añadió el negativo de descriptores incompatibles. La ejecución ampliada del entorno produjo **9 PASS / 1 FAIL**: el lector de descargas rechazaba el artefacto referenciado porque pertenecía a otro propietario. Se añadió la resolución de referencias anterior sin debilitar dicho lector. El fallo y sus salidas se conservan en [native-expanded.log](P093_runs/catalogue-storage-20261101/native-expanded.log).

La repetición ordinaria de la prueba de renombrado falla por permisos de Windows incluso después de soltar los handles; la misma fuente pasa con los permisos necesarios. La ejecución final de los grupos físicos usa esos permisos en carpetas temporales propias. Se conservan [el fallo ordinario](P093_runs/catalogue-storage-20261101/default-native.log) y [la repetición positiva](P093_runs/catalogue-storage-20261101/elevated-native.log). La primera revisión Clippy señaló una copia innecesaria en una aserción de prueba; se corrigió y el grupo final completo pasa.

## Límites y siguiente trabajo

Estos resultados prueban los componentes anteriores. La procedencia y el estado VERIFIED del catálogo nativo son fixtures; las pruebas de catálogo con adquisición usan evidencia sintética. El cambio de letra se inyecta en la topología mientras se accede a archivos reales por GUID: no se reasigna una unidad del usuario. Los casos de sólo lectura y observación obsoleta tampoco cambian la configuración física de su disco.

El servicio público compuesto sigue devolviendo `worker_unavailable`. Falta conectar el productor y consumidor de transformación cualificados, reclamar el job, supervisar y cerrar el proceso, recuperar sus recursos, verificar/publicar outputs con smoke cuando corresponda y ofrecer el reinicio confirmado en la UI. P093 conserva su estado de preparación/BLOCKED_REAL, sin CODE_PASS integral ni REAL_PASS. P016/P040/P051 y el objetivo completo de la app mantienen sus puertas reales pendientes. La autorización de revisión e implementación 3.13.0 está resuelta y no necesita repetirse.
