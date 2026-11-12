# P024 — Plugin Registry, instalación y actualización de adaptadores

**Estado calculado: BLOCKED_REAL.** La batería propia registra **17 PASS y 1 BLOCKED**; las doce puertas generales y el runner pasan (**13/13**, cero fallos). P020/P022 mantienen sus bloqueos y la cualificación real de una candidata antes de promoverla pertenece a P026. No se declara REAL_PASS.

[Evidencia del contrato](P024_plugin-registry-instalacion-y-actualizacion-de-adaptadores.evidence.json) · [Quality gate completo](check_P024_real.json) · [operación y límites](../adapters/registry.md)

## Resultado

El daemon instala actualizaciones ZIP side-by-side mediante `POST /api/v1/adapters/{id}/update` o la CLI local. La aceptación devuelve 202 y persiste journal, receipt idempotente y evento/outbox juntos. Un servicio supervisado ejecuta un único worker por raíz; el resto permanece QUEUED en SQLite. Consulta de jobs para propietario/admin, recuperación al arrancar y apagado que espera el IO están conectados al servicio HTTP.

La nueva versión queda **INSTALLED**, con confianza derivada de P022; no se sobrescribe ni promueve la activa. Selección, habilitación y rollback se guardan con CAS en una tabla auxiliar tipada y sólo admiten VALIDATED no revocadas de la misma identidad/familia. Las fixtures VALIDATED de selección comprueban ese mecanismo, no una suite P026 ejecutada.

## Implementación y compatibilidad

- ZIP: hashes separados de archivo/contenido, snapshot privado, revalidación antes de publicar y bloqueo físico de versión. Se rechazan zip-slip, ADS, dispositivos Windows, colisiones, duplicados sin distinguir mayúsculas, enlaces y reparse points. Límites: 1 GiB, 100000 entradas, ratio 1000, índice central 64 MiB antes de asignarlo y manifiesto 1 MiB también en las relecturas. Se limitan los bytes realmente expandidos.
- Registro: `RegistrySource` y carpeta local determinista; versiones ambiguas rechazadas. Consultas GET con scope read, filtros, paginación y cursor firmado ligado a filtros. OpenAPI registra POST update como implementado.
- API de actualización: UUIDv7, scope adapters:write, Idempotency-Key y DTO cerrado con los tres campos obligatorios (approval_id nullable). Actor y correlación autenticados. Replay durante 30 días conserva la respuesta original QUEUED aunque el job haya terminado; otra combinación de cuerpo/destino rechaza y otro actor tiene su propio espacio de claves.
- Coordinador: un hilo conserva mutex y lock de raíz durante todo el IO. Usa el profiler nativo P012 y admisión P015; reserva ZIP privado + expansión + max(1 GiB, ceil 5 %). Rechaza espacio/identidad desconocidos, volumen offline/read-only y snapshots antiguos. Cada bloque comprueba fencing y contabiliza los bytes escritos.
- Transacciones: preparar confirma plan, revisión, locks, reserva y consumo de aprobación exacta juntos. Completar exige PUBLISHING, fencing y reserva; inserta candidata, confirma job/evento y libera sus recursos juntos. Fallar libera sólo recursos propios después de que termine el IO. Digest ligado a actor/correlación/destino/revisión/solicitud/plan detecta journals alterados.
- Confianza: el servicio usa las claves de settings.security.publisher_keys y bindings explícitos de publicador/official/revocación en data_root/security/adapter-trust.json. Clave ausente o diferente rechaza; retirar una clave de settings no la reactiva desde metadata antigua. CLI admite además un TrustStore explícito. No se toma confianza del ZIP ni se autoriza ejecución por instalarlo.
- Permisos: P022 calcula delta/digest de elevación; se consume una aprobación de un solo uso ligada al mismo actor, digest, recursos y hash antes de copiar. No se transfiere una aprobación entre principales.
- Recuperación: antes de publicar limpia sólo el workspace propio y registra fallo por interrupción. Tras publicar conserva bytes ante commit/ack incierto, reconstruye rutas, verifica hash/manifiesto/confianza/fencing y termina el mismo job. Copia corrupta/revocada o destino modificado se conserva para inspección sin registrarse. No libera por tiempo ni borra paquetes posiblemente confirmados.
- Servicio HTTP: aceptación independiente del futuro del request; desconexión no abandona un commit. Se activa tras abrir el listener. Shutdown cierra admisión, cancela y espera IO. El arranque reconcilia ya iniciados y luego procesa la cola durable; un paquete que falla de forma terminal no bloquea los siguientes.

La migración `0002_adapter_selection.sql` conserva 0001 y sus checksums/filas, añade selección e identidad/versión única NOCASE. Duplicados impiden migrar sin fusionar ni borrar. Los estados del schema/DDL prevalecen sobre los nombres distintos del prompt. No se añaden rutas alternativas de import/export; P028 conserva su propiedad. Registro remoto firmado es P027 y la pantalla de adaptadores es P037.

## Pruebas ejecutadas

Comando de cierre: `tools/check.ps1 -Prompt P024 -Real`. Las transcripciones de cada prueba, comando, expected/observed, fixture/input hashes, host, fecha y environment lock están en PromptEvidence y `P024_runs/`.

| Grupo | Resultado |
|---|---|
| Paquetes/registro | 9 pruebas PASS: determinismo, side-by-side, schema/protocolo/hash, rutas hostiles, límites, staging cambiado, cancel/timeout/IO |
| API catálogo/alta y actualización | 4 + 4 PASS: guard real, DTO/cursor, 202 y delegación, propiedad/admin; negativos sin invocar mutación |
| Persistencia | 69 PASS, incluidas 7 selección, 10 journal y 14 integración/migración, con regresiones de reservas/guard |
| Coordinador/servicio | 18 PASS con archivos y SQLite reales; espacio controlado para negativos y barreras para cancelación/ack/fencing/cola/shutdown |
| DTO y puertos de espacio/confianza | 1 + 4 PASS |
| Windows, catálogo | 9 pasos PASS; worker real en el ZIP, inspección y API, sin ejecutarlo |
| Windows, selección | 13 pasos PASS en procesos independientes, CAS concurrente, fallo real de outbox, reinicio y bytes conservados |
| Windows, CLI update | 8 pasos PASS con profiler/NTFS/SQLite; GUID contrastado con mountvol; muerte real durante EXTRACTING y recuperación |
| Windows, HTTP update | 14 pasos PASS: concurrencia, propiedad, replay, aprobaciones, shutdown con copia y muerte/reinicio tras publicar |
| Migración, OpenAPI, arquitectura, lock y secretos | PASS; 45 tablas/reaplicación vacía, 46 dependencias coherentes y dos credenciales recuperables sin valores en los archivos inspeccionados |

Hay **73 pruebas focales** (además de las regresiones completas) y **44 pasos nativos**, más la prueba de migración. El oráculo Python compara directamente hashes, SQLite, journal/outbox, locks y reservas. Las pruebas de selección usan VALIDATED como fixture explícita. Los adaptadores copiados no se ejecutan ni se cualifican en este flujo.

La [prueba HTTP](P024_runs/api-real.json) envía CTRL_BREAK durante EXTRACTING y comprueba salida 0 tras cleanup y FAILED por cancelación. Para probar el otro lado del commit, una conexión SQLite externa bloquea únicamente el commit cuando ya existe el paquete final; se termina por su handle el daemon propio. Se conservan PUBLISHING, bytes y recursos. Un nuevo servidor reconcilia antes de escuchar, completa el mismo job, deja intactos origen/default y reproduce el receipt original sin duplicados. La [CLI](P024_runs/update-real.json) comprueba también interrupción anterior a la publicación; la [selección](P024_runs/selection-real.json) comprueba rollback durable.

## Bloqueos conservados

1. **P026:** ejecutar una suite real sobre una candidata nueva antes de promoverla. Aquí se rechaza promover INSTALLED; no se confunde integridad o handshake con conformidad.
2. **Dependencias P020/P022 sin REAL_PASS:** los bloqueos heredados, incluida la prueba elevada/AppContainer de P130, siguen vigentes. La preparación independiente no los acredita.

El siguiente incremento según execution_order es **P043**. El recibo de preparación anterior se conserva como registro histórico; PromptEvidence y el quality gate enlazados arriba son la acreditación de esta ejecución.
