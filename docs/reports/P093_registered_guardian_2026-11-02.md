# P093 — propietario nativo registrado y recuperación del trabajo

Fecha de cierre del informe: 2026-11-02. Ejecución congelada: 2026-11-01, 21:38–22:02 UTC. Contrato público 3.13.0 autorizado e intacto. Objetivo global de completar LARM activo. Componente interno verificado; P093 conserva preparación/`BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

## Cambio y comportamiento

El repositorio propone un claim sin mutar el trabajo ni conceder ejecución. El helper preparado comprueba esa propuesta en SQLite de solo lectura y conserva los archivos originales tras verificar SHA, tamaño y EOF antes del handshake. El descriptor interno 3 vincula sesión, clave pública, revisión QUEUED, fence y binding de instalación/run/perfil/plan privado. Preparar P022 no autoriza ejecutar una transformación.

`claim_with_native_owner` compara de nuevo la revisión y la propuesta, exige disponibilidad nativa antes y dentro de la transacción y confirma juntos RUNNING, propietario, journal versión2 y evento/outbox. La lectura de postcondiciones impide confirmar una escritura parcial. Los RPC de transformación se rechazan hasta encontrar el registro exacto confirmado. Después solo se permite Execute del plan privado fijado, una vez por lanzamiento, y Verify del job privado que devolvió esa ejecución.

`NativeTransformationAttempt` reconstruye el observador desde los metadatos validados del job/journal y exige respuestas nuevas firmadas del supervisor vivo. Un registro persistido, PID, flag terminal, timeout o pipe ausente no acredita cierre. El guard de un propietario antiguo o sin el propietario registrado no puede liberar recursos. La recuperación de arranque conserva propietario, error y reservas; el cierre transaccional requiere árbol/IO realmente cerrados y cero llamadas activas. El reinicio SQL archiva el propietario anterior y elimina el actual antes de admitir otro intento.

La admisión de llamadas, stop y retirada comparten una barrera atómica. Si entra una comprobación física después de observar cierre, Release no confirma la retirada mientras conserve recursos. Se corrige además la conexión simultánea: solo se espera ante `PIPE_BUSY`, antes de enviar bytes y bajo el plazo existente. No se repite una operación tras un resultado incierto de escritura/lectura.

No se añaden dependencias, migraciones, `unsafe` ni excepciones de seguridad. [ADR-0015](../adr/0015-supervisor-nativo-recuperable.md).

## Prueba nativa con un trabajo Core

El caso reutiliza la cualificación P026 obtenida mediante el daemon HTTP real y el mismo ejecutable del helper. Catálogo inicial, mapeo del preview a volúmenes y controlador son fixtures explícitas; los archivos, volúmenes, cualificación física, transacciones, procesos y reinicio del servidor son reales.

1. Admite un plan global con input inmutable y espacio observado: un trabajo, una variante original, una reserva y cuatro locks. La preparación del helper todavía no permite Execute.
2. Registra el propietario junto con el claim. Rechaza la verificación de un job ajeno. Dos solicitudes firmadas con nonces distintos compiten: una ejecución queda activa y la otra devuelve `transformation_native_execute_already_started`.
3. Retiene un writer del ledger privado para mantener un RPC real en vuelo. Mata y reinicia el daemon API. El arranque pasa el mismo trabajo a FAILED_RECOVERABLE y conserva propietario y causa `WORKER_INTERRUPTED`. La respuesta HTTP se deserializa como el `JobState` público y coincide con el registro.
4. El propietario de prueba del helper permanece vivo deliberadamente. Otro observador se conecta usando solo los metadatos públicos persistidos y un reto nuevo. El cierre del intento se rechaza mientras vive el proceso; los recursos permanecen retenidos.
5. Solicita el cierre nativo, comprueba cero llamadas activas, IO drenado y árbol cerrado. Solo entonces el repositorio libera exactamente la reserva y los cuatro locks. Conserva el error original; repetir el cierre no cambia versión ni añade eventos. El archivo original queda intacto y no se publica una variante derivada en este caso.

El trabajo termina con versión4 y cuatro eventos: `queued`, `claimed`, `interrupted_recovered`, `attempt_closed`. Queda un trabajo y una variante original, cero reservas y cero locks. El testigo se retira después del cierre durable; el principal temporal se revoca.

Esta interrupción sucede con un RPC pendiente del ledger privado; **no prueba una caída durante bytes parciales de conversión ni el recorrido público de reinicio hasta publicación**. La propiedad del stdin del helper sigue siendo del controlador de prueba. El caso anterior de caída del propietario Python y cero efectos Core también vuelve a pasar, por separado.

## Pruebas y fallos conservados

**27/27 comprobaciones PASS; 405 pruebas PASS, 0 FAIL y 0 ignoradas en el intento final3.** Wrapper y auditor terminan con código0. Se contrastan 874 entradas y 27 logs/recibos sin cambios de fuente durante la ejecución.

| Grupo | Pruebas correctas |
|---|---:|
| Protocolo, identidad, concurrencia, alcance y conexión del supervisor | 18 |
| Supervisor básico real | 3 |
| Daemon P026, RPC privado y propietario registrado/recuperado | 1 |
| Worker del host | 18 |
| SQL de admisión/reinicio, incluidas seis pruebas nuevas de propietario | 49 |
| Resto de dominio, daemon, API, almacenamiento y regresión nativa | 316 |
| Total | 405 |

Las seis pruebas SQL nuevas usan dobles explícitos de observación para omisión de escrituras, disponibilidad perdida, revisión obsoleta, corrupción, cierre y reinicio/historial. No se presentan como autoridad nativa. El caso de procesos reales aporta la prueba de cierre/recuperación descrita arriba. Las aceptaciones optativas se ejecutaron expresamente.

Clippy de cinco crates/todos los targets con warnings denegados, formato, política de `unsafe`, arquitectura17 y copias79: PASS. La regresión conserva 176 casos funcionales P026, 16 wire, 17 observaciones y 9 gates físicos. El caso Core/CAS anterior conserva tres intentos, trece eventos, diecisiete negativos de publicación, un derivado, original intacto y cero reservas/locks finales; esos casos internos no se suman otra vez al recuento de pruebas Rust.

La auditoría reconstruye SHA/bytes, procedencia, vínculos de cualificación y sellos RFC8785. Para el nuevo propietario contrasta registro/journal, versión de recuperación/cierre, respuesta API, cuatro eventos/outbox, límites del plan e input conservado. La inspección final no encuentra helpers, propietarios de prueba o fake-adapters de este workspace abiertos; esa inspección es limpieza y no autoridad de cierre.

Se conservan dos intentos fallidos:

- Intento1: 20 pruebas correctas y una fallida. La conexión concurrente devolvía `guardian_io_unavailable` antes de enviar la petición. Se reprodujo `PIPE_BUSY` con un pipe real y se añadió espera acotada solo durante la conexión inicial. Se conservaron las fuentes exactas previas a la corrección.
- Intento2: 21 pruebas correctas y una fallida. Ya pasaban la competencia de Execute y el reinicio del servidor, pero el comprobador esperaba una envoltura `data` inexistente. Se corrigió para usar el tipo público `JobState`; también se conservan las fuentes anteriores. Se corrigió antes una consulta auxiliar del comprobador que usaba `data` en lugar de la columna `payload` de eventos.

## Evidencias

- [Resumen final3](P093_runs/registered-guardian-20261101/attempt3/verification-synopsis.json), [auditoría independiente](P093_runs/registered-guardian-20261101/attempt3/post-run-audit.json) y [limpieza de procesos](P093_runs/registered-guardian-20261101/attempt3/post-run-process-check.json).
- [Recibo del caso cualificado y registrado](P093_runs/registered-guardian-20261101/attempt3/native-qualified-guardian.json), [log nativo](P093_runs/registered-guardian-20261101/attempt3/verify-qualified-guardian.log) e [informe del propietario registrado](P093_runs/qualified-guardian/01a1990e-0a62-725d-b211-9bc2068787e1/registered-owner/report.json).
- [Regresión Core/CAS](P093_runs/registered-guardian-20261101/attempt3/native-transformation.json).
- [Wrapper congelado](P093_runs/registered-guardian-20261101/attempt3/frozen-verify.py) y [auditor congelado](P093_runs/registered-guardian-20261101/attempt3/frozen-audit.py).
- [Intento1](P093_runs/registered-guardian-20261101/attempt1/verification-synopsis.json) e [intento2](P093_runs/registered-guardian-20261101/attempt2/verification-synopsis.json).

## Continuación necesaria

La siguiente integración debe resolver el lanzamiento todavía no registrado mediante intención durable y llevar el ownership al productor/consumidor público del daemon. Faltan el transporte de salida observada por el supervisor hacia el verificador de publicación, progreso/cancelación por id y H07 completo: caída durante conversión, plan nuevo confirmado, reinicio del mismo trabajo y publicación final por la API.

El motor público conserva `worker_unavailable`. Siguen pendientes las reservas por fase/volumen, retención privada, reconstrucción de procedencia, política tras pérdida del supervisor/reinicio del sistema, FAT32 completo, conversores/hooks GGUF/ONNX reales y UI. El resto de LARM, incluidos runtimes, fuentes, Broker y Agora, permanece en el objetivo global activo.
