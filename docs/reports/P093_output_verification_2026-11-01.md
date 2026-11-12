# P093 — límites y verificación física de salidas

Fecha: 2026-11-01. Contrato público: 3.13.0, cuya revisión e implementación ya están autorizadas. Objetivo global activo. Incremento de implementación verificado; P093 conserva `BLOCKED_REAL` como estado de aceptación, sin `CODE_PASS` integral ni `REAL_PASS`.

El proceso cualificado entrega ahora un manifiesto interno ligado al claim concreto, su nonce/generación/intento, instalación, Run, perfil y plan del worker. El objeto se obtiene de la RPC observada por el host; no se deserializa desde una petición HTTP. Sigue siendo una declaración: Core calcula independientemente los hashes antes de producir un propietario de archivos verificados.

El plan interno puede sellar una política de salida con volúmenes de trabajo y CAS, límites total/por archivo/número de archivos, control retenido del worker y presupuesto de control de Core. El hash incluye todos esos campos. La fase de registro cuenta la coexistencia de salida privada, copia CAS y control de ambos participantes; se combina con las demás fases mediante el estimador común. No presupone deduplicación. Las cotas conservan confianza `UNKNOWN`, en lugar de presentarse como tamaños exactos, y siguen sujetas al presupuesto y admisión existentes. Una estimación privada del worker que no cubre la coexistencia no puede sellarse como plan de publicación. El job conserva la política y su historial, y un reinicio no puede eliminarla mediante un plan heredado. Los planes anteriores sin política siguen siendo legibles, pero no autorizan capturar/publicar salidas con la nueva API. El DTO público de Plan conserva sus ocho campos.

La captura exige cierre y drenaje del propietario real del proceso antes de abrir archivos, y revalida la cualificación antes y después. Un sondeo reciente comprueba el volumen de trabajo, y el acceso local se resuelve por GUID. La raíz y cada archivo conservan handles de Windows. Core exige manifiesto completo, tamaño y SHA conocidos, al menos un artefacto requerido, cotas respetadas, rutas inequívocas dentro del prefijo esperado y hash canónico del manifiesto correcto. Lee todo el archivo, comprueba EOF y SHA, y retiene simultáneamente todos los archivos, incluidos los opcionales declarados. Rechaza escritor previo, corrupción de igual longitud, truncado, archivos ausentes, junctions, escapes, ADS, duplicados y sustitución de carpetas. Un consumidor posterior lee esos mismos handles. Esta prueba física no concede permisos de catálogo ni de escritura CAS.

Se añade un registro de validadores de formato gestionado por el servidor. Un hook registrado debe completar carga, salud y smoke; cualquier error impide obtener el resultado validado. La selección no distingue mayúsculas del formato. Sin hook, el resultado es explícitamente `NotRegistered`. Las pruebas del hook son controladas y no acreditan carga real de GGUF/ONNX, capacidades ni estado `TECH_VALIDATED`/`AVAILABLE`.

La receta nativa conserva P026 original (176 casos funcionales, 16 wire, 17 observaciones y 9 casos físicos de cualificación) y recorre después Core/SQLite con procesos y almacenamiento reales. El catálogo inicial y la composición del preview global son fixtures explícitas. El harness aporta la raíz/prefijo del output de referencia; la resolución autorizada de ese ámbito desde un perfil registrado sigue siendo responsabilidad del futuro productor público.

En el segundo intento se obtiene primero una captura completa, después de cerrar el worker: DATA de 16.777.224 bytes y procedencia de 792 bytes. Se comprueban lecturas por handle y rechazo de sobrescrituras mientras los propietarios siguen vivos. No hay validador Base64 registrado, por lo que queda `NOT_REGISTERED`. Tras liberar la captura, se altera exactamente un byte de DATA. Una nueva captura devuelve realmente `transformation_output_hash_changed`; ese error se persiste al cerrar el intento. El original de 12.582.917 bytes queda intacto. Se mantienen un job, dos intentos y ocho eventos; al final hay cero reservas, cero locks y una sola variante, la original. No se publica un derivado.

Verificación congelada, intento 1: **17/17 comprobaciones PASS, 284 pruebas PASS, 0 FAIL, 0 ignoradas**. El wrapper terminó con código 0. No se modificaron fuentes durante la ejecución. La auditoría independiente contrasta 852 entradas, 17 logs/recibos, Run/perfil RFC8785, los nuevos sellos de Core y las cotas por volumen, Base64, procedencia y archivos retenidos, incluidos los 16.778.016 bytes de salida sana y la copia con un byte corrupto.

| Grupo | Pruebas correctas |
|---|---:|
| Dominio | 178 |
| Recuperación heredada | 4 |
| HTTP / OpenAPI | 6 / 10 |
| Cliente tipado / receta nativa completa | 3 / 1 |
| Catálogo de entradas / planes, admisión y reinicio SQL | 3 / 43 |
| Captura de salidas y hooks, incluido inventario del crate | 9 |
| Entradas físicas / archivos y directorios del host / fences de rutas | 8 / 6 / 3 |
| Entorno de almacenamiento nativo | 10 |

Clippy de dominio, DB, transform, host y daemon, todos los targets y warnings denegados: PASS. Formato: PASS. Arquitectura: 17 crates PASS. Copias contractuales: 79 PASS. Una revisión preliminar detectó la colocación del módulo de pruebas antes del código de producción; se corrigió antes del conjunto congelado. No se ejecutó aceptación GUI en este incremento, que no modifica la UI.

Evidencia reproducible y retenida:

- [Resumen congelado](P093_runs/output-verification-20261101/attempt1/verification-synopsis.json).
- [Auditoría independiente](P093_runs/output-verification-20261101/attempt1/post-run-audit.json).
- [Recibo nativo y referencias a archivos](P093_runs/output-verification-20261101/attempt1/native-transformation.json).
- [Wrapper](P093_runs/output-verification-20261101/verify.py) y [auditor](P093_runs/output-verification-20261101/audit.py). Cada ejecución nueva exige un nombre de intento nuevo; no sobrescriben evidencia anterior.

Falta implementar el journal de publicación y la copia/promoción CAS recuperable, verificar el tamaño real del control antes de escribirlo y registrar atómicamente variante, artefactos, ubicaciones, procedencia y finalización del job. Después siguen el productor/consumidor público con ámbito resuelto desde el perfil autorizado, ownership persistido y recuperación tras reinicio real del daemon, seguimiento de reservas por fase y límites FAT32 por archivo, validadores de ejecución reales y UI. El servicio público continúa respondiendo `worker_unavailable` para crear/ejecutar/reiniciar: estas pruebas no acreditan todavía el recorrido integral de P093 ni completan el desarrollo global de LARM.
