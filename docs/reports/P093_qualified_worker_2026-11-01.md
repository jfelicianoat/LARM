# P093 — cliente de transformación cualificado y regresión de almacenamiento

Fecha: 2026-11-01. Contrato: 3.13.0. Objetivo global de la app activo.

El host dispone de un cliente tipado para `transformation.describe`, `plan`, `execute` y `verify`. Su constructor de producción exige una instalación de la familia correcta, un Run P026 vigente y el perfil registrado. Conserva el guard físico, las raíces y los permisos medidos durante handshake, llamadas y cierre. Cada RPC comprueba de nuevo la cualificación antes y después; un error original de la llamada se conserva si también falla la comprobación posterior.

El plan conserva la distinción entre el ID/hash privados del worker y el plan de Core que confirma el operador. El binding incorpora instalación, Run, digest de Run y hash del perfil obtenidos por el host. Se comprueban recursos, pasos, ID/hash y caducidad de la respuesta; no se convierte un estimate UNKNOWN ni una elegibilidad privada en permiso global. El cliente expone progreso y cancelación y permite cerrar incluso tras perder cualificación. El cierre espera al proceso y al drenaje de IO; no concede autoridad para liberar reservas de Core por sí solo.

## Verificación

Resultado final: **12/12 comprobaciones PASS; 63 pruebas PASS, 0 FAIL, 0 ignoradas**. Son las 59 pruebas del catálogo/archivos/almacenamiento, tres pruebas del nuevo cliente y una aceptación nativa del consumidor cualificado. Los casos filtrados por estas ejecuciones focales no se cuentan como aprobados.

La aceptación nativa vuelve a ejecutar la receta P026 original completa: **176 casos funcionales, 16 de transporte y 17 observaciones**. Conserva además los nueve casos del gate físico original. Después consume esa cualificación con el nuevo cliente y comprueba:

- Las cuatro llamadas tipadas y el binding con el perfil/Run persistidos.
- Exclusión de otra sesión, rechazo de un ID RPC inválido, identidad de adaptador ajena y lectura fuera de los permisos.
- Conversión binaria/Base64 de una entrada real de **12.582.917 bytes**; el artefacto codificado ocupa **16.777.224 bytes**.
- Replay del mismo plan sin duplicar el recibo, lectura del manifiesto y medida independiente de cada archivo de salida.
- Original intacto, cambio de inventario del paquete rechazado durante la sesión y cierre correcto incluso después de perder cualificación.
- IO drenado y handles liberados, comprobado al volver a abrir el original para escritura idéntica.
- Cero trabajos de transformación, reservas o locks publicados por esta prueba en Core; el ledger y las salidas son privados del adaptador de referencia.

La auditoría independiente reconstruye con RFC8785 los hashes del Run, recibo, observaciones, perfil y manifiesto, además de comprobar los bytes Base64, SHA-256, tamaños y procedencia de los archivos conservados. Valida los cuatro outputs de la receta original y los dos archivos de salida retenidos del cliente tipado. Se conservan también los bytes de entrada de la fixture para repetir el contraste.

Clippy para DB, transformación, host y daemon con todos sus targets y avisos como errores, formato, arquitectura de 17 crates y 79 copias contractuales pasan. El wrapper termina con código 0. **843 entradas, 12 logs y 12 recibos** coinciden por SHA-256 y permanecieron sin cambios durante la ejecución.

[Resultados y fuentes congeladas](P093_runs/qualified-worker-20261101/attempt1/verification-synopsis.json) · [auditoría independiente](P093_runs/qualified-worker-20261101/attempt1/post-run-audit.json) · [resumen nativo conservado](P093_runs/qualified-worker-20261101/attempt1/native-transformation.json) · [ejecutor](P093_runs/qualified-worker-20261101/verify.py).

## Alcance y trabajo restante

El [incremento anterior del catálogo y almacenamiento](P093_catalogue_storage_2026-11-01.md) conserva sus fallos reproducidos y recibos históricos; su regresión completa vuelve a pasar con este cliente. No se ha ampliado ningún schema público ni se necesita otra autorización contractual para este incremento.

La conversión anterior prueba un adaptador de referencia controlado y su consumidor nativo. **No acredita GGUF, ONNX ni el pipeline público de P093.** El servicio compuesto sigue devolviendo `worker_unavailable` hasta integrar productor/consumidor de Core, claim e intentos, supervisión/cierre y recuperación de recursos, publicación atómica de outputs completos, smoke y reinicio confirmado en UI. El supervisor debe mantener la propiedad física hasta cerrar IO y resolver el journal antes de liberar los recursos; no basta un estado terminal en SQLite.

P093 sigue en preparación/BLOCKED_REAL, sin CODE_PASS integral ni REAL_PASS. Se mantienen las puertas reales de P016/P040/P051 y el objetivo completo de LARM. La revisión 3.13.0 y su implementación están autorizadas; no se vuelve a pedir esa decisión.
