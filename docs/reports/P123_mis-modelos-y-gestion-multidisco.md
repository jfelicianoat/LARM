# P123 — Mis modelos y gestión multidisco

Estado: `BLOCKED_REAL`.

“Mis modelos” ya permite a técnicos locales mantener el inventario, comparar variantes y preparar un plan de descarga para una ubicación elegible, con estimaciones por archivo y vencimiento visible. La confirmación final permanece deshabilitada hasta que P051 pueda crear el trabajo de forma durable. La biblioteca muestra disponibilidad, propiedad, archivos, tamaño observado y ubicación registrada. También permite verificar, mover, importar, analizar y revisar operaciones de borrado.

Al comparar destinos, el preflight muestra licencia declarada, autenticación/cuenta configuradas y confianza/estado del conector. Aclara cuándo esos datos no están disponibles y que el plan no prueba credenciales ni acepta licencias.

El asistente de movimiento selecciona solo destinos online, muestra espacio libre/reservado/utilizable, solicita un plan en seco, bloquea sesiones activas y espera confirmación antes de ejecutar. Después consulta el trabajo y presenta fases y bytes copiados.

El análisis de almacenamiento clasifica uso por modelos, runtimes, cachés, parciales, temporales y huérfanos. La vista de limpieza enseña los archivos que propone retirar; no ejecuta limpieza. El borrado de una variante desglosa cada acción, conserva archivos externos y compartidos según el plan, y solo envía la petición tras confirmar y crear una aprobación ligada al hash.

La importación revisa ruta, destino y modo con el preview read-only de P098, que vuelve a recorrer la carpeta y devuelve archivos reconocidos, tamaños y variantes. Un cambio de parámetros invalida esa revisión; la pantalla exige confirmación antes de crear el trabajo idempotente. El worker vuelve a validar y canonicalizar la ruta y crea o reutiliza automáticamente una fuente por carpeta; las variantes importadas conservan esa procedencia.

## Comprobaciones

- Pruebas focales del módulo: 15/15 PASS. Incluyen movimiento sin ejecución previa a confirmación, bloqueo por lease, seguimiento del job, análisis por categorías, vista previa de limpieza sin endpoint de ejecución, importación confirmada, aprobación ligada al hash antes del borrado, lectura de confianza de la fuente local y preparación del plan por destino con confirmación de descarga bloqueada.
- Importación backend: PASS. Tres pruebas SQLite verifican creación por carpeta, reutilización idempotente y auditoría; el worker registra las cuatro variantes de prueba bajo la fuente correspondiente.
- Suite del escritorio actual: 9 archivos y 84/84 PASS.
- ESLint, TypeScript y build de producción: PASS.
- Las pruebas usan un transporte simulado; no movieron, copiaron ni borraron datos reales.

## Pendiente para cerrar P123

Tampoco se ejecutaron recorridos reales contra el daemon y discos controlados; P016, P018, P042 y P098 siguen sin `REAL_PASS`. La UI y sus pruebas demuestran los guards de código, pero no sustituyen esas validaciones.

La evidencia formal de P123 incluye las verificaciones de procedencia del importador y los avisos del preflight, junto con las 15 pruebas del módulo y las 84 de la suite actual de escritorio. El bloqueo `REAL` por recorridos multidisco y dependencias pendientes sigue vigente. Evidencia: [PromptEvidence](P123_mis-modelos-y-gestion-multidisco.evidence.json). La pantalla está en [models.tsx](../../apps/desktop/src/models.tsx), los asistentes en `move-model.tsx`, `model-import.tsx`, `model-delete.tsx` y `model-storage.tsx`, y las pruebas en [models.test.tsx](../../apps/desktop/src/models.test.tsx).

`python tools/p123_evidence.py` repite las pruebas del escritorio, calidad frontend, idempotencia de la fuente local y worker de importación, y valida el PromptEvidence. La ejecución actual registró 7 PASS, 0 FAIL y 1 prueba REAL bloqueada.
