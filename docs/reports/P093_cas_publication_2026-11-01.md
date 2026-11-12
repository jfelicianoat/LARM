# P093 — publicación recuperable en CAS y cierre atómico

Fecha: 2026-11-01. Contrato público: 3.13.0, con autorización de implementación ya resuelta. Objetivo global activo. Incremento interno verificado; P093 mantiene su gate de aceptación `BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

Core puede preparar una salida verificada, copiarla al almacén gestionado y registrar juntos el derivado y la finalización del trabajo. La preparación y la publicación física siguen separadas del servicio público, que todavía no tiene un productor/consumidor compuesto.

La preparación toma el claim concreto y exige cierre/drenaje del proceso real, cualificación vigente, entrada física retenida y catálogo de origen intacto. Guarda transaccionalmente el grafo completo, su SHA canónico, journal, metadata del job y evento/outbox antes de emitir un ticket interno de escritura. El grafo contiene los snapshots originales, la salida medida, la variante prevista, bundle, artefactos, ubicaciones y procedencia. Repetir la preparación devuelve el mismo grafo; un escritor omitido o un presupuesto de control insuficiente no devuelve ticket ni crea un derivado. El presupuesto incluye el tamaño serializado del job y journal y una cota para manifiesto y recibos; el publicador comprueba también los bytes reales del manifiesto antes de escribirlo.

El publicador lee los mismos handles protegidos que produjeron la prueba de integridad. Usa `staging/<claim_id>/<artifact_id>.partial`, un marcador ligado al grafo y una lease exclusiva real de Windows. Otro publicador del mismo intento no puede acceder a sus parciales. Conserva pins de raíz y directorios CAS, escribe y sincroniza los temporales, reutiliza la promoción recuperable de CAS y vuelve a medir SHA/tamaño de cada blob retenido. Un recibo de otro plan se rechaza también cuando el blob ya existe. La recuperación de un blob compartido que ya es de solo lectura evita modificar sus atributos: esto permite mantener otros lectores protegidos sin perder la comprobación de bytes.

Antes de copiar se consulta el espacio reciente y se comprueban política, márgenes y reservas de otros trabajos. Esta comprobación aún exige conservadoramente el pico sellado completo; el descuento del espacio ya materializado por fase sigue pendiente. También se comprueba el tamaño real de cada archivo frente a FAT32 al publicar; falta corregir la admisión global por archivo y acreditar el caso real de ese sistema de archivos.

El manifiesto `manifests/<variant_id>.json` se publica sin reemplazar un destino existente. Conserva el formato de raíz v1 y añade el grafo completo en el campo abierto `model.evidence.transformation`. Los blobs y el manifiesto quedan protegidos hasta el commit. No se ha acreditado todavía que el reconstructor P018 restaure automáticamente toda esta nueva procedencia desde una base vacía: ese consumidor necesita ampliarse y probarse.

La finalización vuelve a comprobar fence, grafo, cualificación, origen, destino y prueba física. Una sola transacción inserta variante, artefactos, bundle, ubicaciones, procedencia y sus eventos/outbox; enlaza la salida al job, lo marca `COMPLETED` y retira exactamente sus guards/reservas. La lectura posterior de todas las filas obligatorias detecta escrituras omitidas. Un replay devuelve el mismo resultado sin insertar ni emitir eventos adicionales. Un intento anterior no puede publicar el actual. Solo un recibo opaco emitido por el repositorio tras confirmar el commit permite retirar los parciales, recibos de promoción y marcador propios; nunca se borran blobs compartidos ni el original.

La nueva variante conserva `VERIFIED`, disponibilidad `UNKNOWN`, capacidades vacías y límites desconocidos. La procedencia registra padres, herramienta/versión, parámetros, actor y evidencia, con confianza `DECLARED`; no inventa licencia ni procedencia oficial. No se heredan capacidades o límites como hechos de la salida. No hay validador Base64 registrado y no se acredita validación técnica GGUF/ONNX.

## Prueba nativa y límites de su alcance

La receta original P026 conserva sus 176 casos funcionales, 16 wire, 17 observaciones y 9 casos físicos de cualificación. Le sigue el cliente tipado y el recorrido directo Core/SQLite con procesos, archivos y almacenamiento reales. El catálogo inicial, el preview global y el ámbito de referencia son fixtures; el tercer intento comprueba además el prefijo específico `worker_job_id` + número de intento del adaptador de referencia.

El mismo job pasa por tres intentos y trece eventos de trabajo. Los dos primeros conservan las pruebas de interrupción/corrupción; el tercero publica DATA de **16.777.224 bytes** y procedencia de **792 bytes**. El manifiesto retenido mide 20.623 bytes y el grafo exportado, 16.654 bytes. El original de 12.582.917 bytes se conserva. Al terminar hay un job completado, dos variantes (original y derivada), cero reservas y cero locks.

El recorrido descarta los objetos del primer publicador y vuelve a leer el ticket desde SQLite y los recibos desde disco. Reentra con un lector CAS retenido y rechaza otro escritor concurrente. **Es reentrada de handles/repositorio en el mismo proceso; no es una caída y reinicio real del daemon ni H07 completo.**

Los 17 negativos registrados cubren dos omisiones de escritura durante preparación, presión de metadata sobre el presupuesto real de control, presión sintética de espacio, corrupción CAS de igual longitud, recibo de otro plan y omisiones en once puntos del commit: variante, artefacto, bundle, ubicación, procedencia, evento, outbox, job, journal, locks y reservas. Los triggers y la presión de espacio son fixtures explícitas. Cada rechazo conserva el snapshot del catálogo/job anterior; después se realiza un commit real y su replay, y se retiran los recibos únicamente tras confirmarlo.

## Verificación final

**18/18 comprobaciones PASS, 296 pruebas PASS, 0 FAIL, 0 ignoradas**, wrapper y auditor con código 0. La auditoría independiente contrasta 856 entradas, 18 logs/recibos, sellos de planes, grafo canónico, manifiesto, bytes Base64/procedencia, blobs retenidos y los dos daños controlados de un byte. Comprueba también el cambio de catálogo, los trece eventos y los siete eventos de la transacción final.

| Grupo | Pruebas correctas |
|---|---:|
| Dominio | 181 |
| Recuperación heredada | 4 |
| HTTP / OpenAPI | 6 / 10 |
| Cliente tipado / receta nativa completa | 3 / 1 |
| Catálogo de entradas / planes, admisión y reinicio SQL | 3 / 43 |
| Salidas y hooks | 9 |
| Entradas físicas / archivos del host / fences de rutas | 8 / 6 / 3 |
| Entorno de almacenamiento nativo / CAS | 10 / 9 |

Clippy de dominio, DB, transform, host y daemon, todos los targets y warnings denegados: PASS. Formato: PASS. Arquitectura: 17 crates PASS. Copias: 79 PASS. Sin cambios de fuentes durante cada conjunto congelado. No hay cambios ni aceptación GUI en este incremento.

El intento 1 también pasó las 296 pruebas y su auditoría. Se corrigieron después dos textos históricos del recibo que negaban la publicación y se endureció su comprobación; el intento 2 repitió el conjunto completo. Se conservan ambos resultados y los scripts congelados del primero. Las comprobaciones preliminares de compilación detectaron y corrigieron acceso a un conversor privado, construcción del tamaño, uso de una API temporal obsoleta y marcación de SQL dinámico de fixtures antes de los conjuntos finales.

- [Resumen final, intento 2](P093_runs/cas-publication-20261101/attempt2/verification-synopsis.json).
- [Auditoría independiente final](P093_runs/cas-publication-20261101/attempt2/post-run-audit.json).
- [Recibo nativo con referencias a grafo, manifiesto y archivos](P093_runs/cas-publication-20261101/attempt2/native-transformation.json).
- [Primer conjunto conservado](P093_runs/cas-publication-20261101/attempt1/verification-synopsis.json).
- [Wrapper](P093_runs/cas-publication-20261101/verify.py) y [auditor](P093_runs/cas-publication-20261101/audit.py). Cada ejecución exige un nombre de intento nuevo.

Siguen pendientes el ownership nativo persistido y la recuperación tras reinicio real del daemon, el productor/consumidor público con ámbito autorizado desde perfiles registrados, contabilidad completa por fase y por volumen del journal de Core, retención/limpieza del output privado del worker, restauración de la nueva procedencia desde manifiestos, FAT32 completo, conversores/validadores GGUF/ONNX reales y UI. El servicio público conserva `worker_unavailable` para crear/ejecutar/reiniciar hasta integrar esas garantías. El objetivo global de desarrollo de LARM continúa activo y no queda completado por este incremento.
