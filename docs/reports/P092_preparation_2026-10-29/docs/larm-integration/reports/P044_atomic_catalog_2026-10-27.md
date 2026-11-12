# P044/P052 — persistencia atómica del catálogo remoto

Fecha: 2026-10-27. Estado: incremento de persistencia verificado; recorrido productivo global en curso, `BLOCKED_REAL`.

Este incremento continúa la implementación autorizada de 3.12.0 y el [binding source 3.12.1](P044_source_binding_3_12_1_2026-10-27.md). No cambia contratos, migraciones ni DTO públicos. Añade un escritor SQLite interno para un grafo inicial cuyas identidades centrales ya haya resuelto P041. El registro productivo P041 todavía no lo invoca: falta el preparador que conecte ambas piezas, incluidas las anotaciones de identidad.

`SqlCatalogAcquisitions::record` escribe modelo, distribución, revisión, variante, artefactos, snapshot y eventos/outbox en una transacción. Toma el bloqueo de escritura antes de leer identidades. Reutiliza exactamente las filas actuales, rechaza cambios concurrentes y no remapea UUIDs ni fusiona por nombre. Los métodos anteriores de alta reutilizan las mismas funciones de escritura internas y conservan sus transacciones habituales.

El preparador debe leer `resolution_revision()` antes de resolver candidatos. Antes de crear un modelo canónico, el escritor comprueba ese watermark durable de eventos. Un cambio requiere resolver otra vez; podar eventos entregados no devuelve el contador a un valor antiguo. El control es conservador: eventos de otras operaciones pueden exigir una repetición de la resolución. Un replay exacto con todas las filas presentes no duplica entidades ni eventos. Un UUID distinto para la misma observación/identidad de variante falla; una nueva observación conserva el snapshot anterior.

La entrada sólo admite el alta inicial DISCOVERED/UNKNOWN sin hash de bundle físico ni tamaños observados localmente. Comprueba enlaces y cada descriptor, incluido rol y certeza de tamaño. No crea bundle físico, ubicación, procedencia, job ni reserva. Un guard de cualificación actual se conserva hasta recheck y COMMIT; perderlo revierte el grafo completo.

## Fallo reproducido y corrección

La prueba [role-red](P044_runs/atomic-catalog-20261027/role-red.log) falla 0/1: la persistencia de snapshots no cotejaba el rol del artefacto contra el catálogo. Se añadió ese cotejo sin inferir un rol conocido cuando falta: la ausencia sigue siendo `other`. El fixture compartido declara ahora explícitamente `weights`, que era el rol de su snapshot. La prueba corregida pasa y no publica snapshot/evento/outbox ante un rol diferente.

## Verificación

Pasan **124 pruebas únicas**: snapshots 7, grafo atómico 8, DB lib 77, integración DB 18 y regresión de modelos/bundles 14. Las nuevas pruebas de grafo verifican cuatro certezas, intervalo sin punto, UNKNOWN sin cero inventado, replay, SQLite tras reinicio, concurrencia y UUID competidor, cambios de filas y del watermark, rechazo de descriptores/estados físicos falsos, historial y rollback completo ante fallo del último outbox o guard caducado. Al terminar, los contadores de bundle/ubicación/procedencia/jobs/reservas siguen en cero.

Las identidades preparadas y los recibos/guards de cualificación son fixtures CODE_PASS explícitos. SQLite y las transacciones son reales; estos ensayos no ejecutan el resolvedor P041 ni un worker/P022/P026. No son aceptación de primera adquisición por daemon ni de GUI.

Se conserva el [primer intento](P044_runs/atomic-catalog-20261027/catalog-first.log): 13 PASS y 1 FAIL porque el fixture HIGH omitía su cota superior obligatoria. Se corrigió el fixture a punto/cota 11, manteniendo HIGH y el guard de producción. La integración DB restringida terminó 15 PASS/3 FAIL al publicar backups temporales con error Windows 5; la ejecución Windows aprobada pasa 18/18 con el mismo código, sin modificar backup/restore. No se cuentan los intentos fallidos como PASS.

Clippy de DB/modelos/motor/daemon con todos los targets y `-D warnings`, formato, arquitectura y 78/78 copias 3.12.1 pasan. No se repiten la suite workspace completa, GUI ni ensayos nativos del daemon después de este delta DB; los dos nativos de la fase source permanecen como evidencia de aquella versión sellada. [Recibos agregados](P044_runs/atomic-catalog-20261027/increment.checks.json) · [checkpoint al cierre](P044_runs/atomic-catalog-20261027/implementation.at-close.sha256.json).

## Siguiente conexión

Conectar el preparador P041 al escritor atómico, conservando evidencia de identidad, IDs y certezas del manifiesto observado. Completar la búsqueda cualificada y el binding source_id/perfil; enlazar snapshot con preflight, confirmación, consumer, recuperación y bundle verificado después de recibir bytes. Mantener la comprobación real de permisos/licencia/auth, cambios/revocación, SHA/CAS/procedencia, fallos/reinicio y GUI. El objetivo de la aplicación completa sigue activo.
