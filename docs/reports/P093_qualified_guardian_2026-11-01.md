# P093 — RPC cualificado dentro del supervisor recuperable

Fecha: 2026-11-01. Contrato público 3.13.0 autorizado e intacto. Objetivo global de completar LARM activo. Integración interna de cualificación y transporte verificada; P093 continúa en preparación/`BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

## Implementación

El helper real puede reconstruir una sesión de transformación desde la cualificación persistida. El bootstrap de este modo lleva ruta de base, actor/correlación e identidad esperada; código, raíces, aprobación y receta salen del perfil registrado. Abre SQLite en modo de solo lectura y `query_only`, sin crear ni migrar una base. Comprueba la identidad esperada antes del handshake y conserva la cualificación física, sus pins y el proceso/IO existente del Worker.

Se transportan `describe`, `plan`, `execute` y `verify` por el canal privado. Se reutiliza el cliente tipado del host: valida peticiones/respuestas, comprueba el acceso declarado y remide la cualificación antes y después de la operación. El helper consulta además el recibo/journal y el perfil actuales de SQLite en ambos extremos. Un cambio válido del perfil se rechaza aunque sus archivos sigan fijados por la cualificación anterior. La limpieza sigue disponible al perder cualificación y se conserva el primer error de operación.

La cualificación incluye la identidad del ejecutable del host. Se conserva ese vínculo: el nuevo caso cualifica mediante el daemon HTTP real y el helper usa **el mismo binario**, sin sustituir el hash de un recibo creado por el ejecutable de tests.

El descriptor interno 2 añade la identidad instalación/run/perfil. El descriptor público permite observar/cerrar para recuperar; no reconstruye el cliente de ejecución. Cada RPC exige una firma privada de control, con dominio distinto al de la respuesta, que vincula descriptor, nonce, acción, parámetros y acceso. La respuesta firma también resultado/error. Semilla y entorno solo pasan por stdin heredado, sin argumentos, logs o archivos con claves privadas.

Tras comprobar la firma, el helper consume el nonce UUIDv4 una sola vez. Conserva un conjunto acotado de 4096 nonces, sin desalojo; al llenarse rechaza nuevos RPC y mantiene la observación/limpieza. Una firma inválida no ocupa el nonce. El control es por vida de helper; la composición futura debe generar una sesión/clave nueva por lanzamiento admitido.

Los frames están acotados a 512 KiB y hay un máximo de dieciséis conexiones. Una llamada pendiente no impide observar/cerrar por otra conexión. El cierre exige cero llamadas activas, IO real del Worker drenado y pertenencia del objeto nativo reducida al supervisor. El plazo del observador no abandona las comprobaciones físicas del backend: se mantienen contadas hasta acabar. Esto evita declarar cierre mientras una verificación todavía retiene pins o puede ejecutar un sondeo de identidad del sistema. El helper del daemon usa su runtime dedicado de un solo hilo.

La decisión se amplía en [ADR-0015](../adr/0015-supervisor-nativo-recuperable.md). No se añade `unsafe`, dependencia nueva ni excepción de seguridad.

## Prueba nativa nueva

Una fixture local registra instalación, aprobación y receta compilada. Un principal temporal del propio caso invoca `POST /adapters/{id}/probe` sobre el daemon HTTP real y espera su resultado persistido. Se reconstruye esa identidad desde SQLite y se lanza el helper real, conservando su propietario de prueba como proceso separado.

- Con el primer Worker ya dueño del lock nativo de su estado privado, otro helper con identidad incorrecta sale con código 1 y `transformation_guardian_identity_changed`, antes de alcanzar el handshake que competiría por ese lock.
- Un RPC bien formado sin firma no crea planes en el ledger privado.
- Un paquete con firma inválida no consume su nonce. El paquete válido crea exactamente un plan; repetir sus mismos bytes firmados devuelve `guardian_control_replayed` y no crea otro. La auditoría contrasta los tres recuentos de planes.
- El acceso a un archivo fuera de las raíces aprobadas devuelve `read_denied`.
- Las cuatro operaciones cualificadas convierten realmente el input de 12.582.917 bytes en un archivo Base64 de 16.777.224 bytes y una procedencia JSON de 792 bytes. Se conservan y verifican bytes, tamaños, hashes y manifiesto.
- Una modificación estructuralmente válida del perfil persistido, con su hash actualizado, bloquea la siguiente llamada. Tras restaurar exactamente el perfil de fixture, vuelve a funcionar.
- Se retiene el writer del ledger privado y se observa una llamada realmente activa desde otra conexión. Se mata el propietario de prueba por su handle. Un nuevo observador carga solo el descriptor guardado, comprueba cierre nativo y cero llamadas activas; nuevos RPC quedan rechazados. Los pins de entrada se conservan hasta retirar el testigo.
- El principal temporal se revoca. El caso no cambia jobs, variantes, reservas o locks de Core: los cuatro recuentos empiezan y terminan en cero.

La aceptación optativa se ejecutó explícitamente; no se atribuye PASS a un test ignorado sin ejecutar. Se trata del adaptador de referencia Base64 y de un propietario de prueba Python. **El daemon HTTP cualifica el adaptador; el proceso que se mata en este caso es el propietario de prueba del helper. No es H07 integral de un job Core al caer el servidor API.** No hay admisión ni liberación de recursos Core en este recorrido.

## Resultado congelado

**27/27 comprobaciones PASS, 393 pruebas PASS, 0 FAIL, 0 ignoradas.** Wrapper y auditor independiente terminan con código 0. Se verifican 869 entradas de código/contrato y 27 logs/recibos, sin cambios de fuente durante la ejecución.

| Grupo | Pruebas correctas |
|---|---:|
| Firmas, bootstrap, RPC y consumo único de nonces | 12 |
| Supervisor básico y propietario reales | 3 |
| Daemon HTTP P026 y supervisor cualificado | 1 |
| Worker del host | 18 |
| Regresión anterior de dominio, daemon, API, SQL, transformación y CAS | 359 |
| Total | 393 |

Clippy de dominio, DB, transform, host y daemon, todos los targets y warnings denegados; formato; política de `unsafe`; arquitectura de 17 crates y 79 copias contractuales: PASS. La auditoría independiente reconstruye SHA, bytes Base64, procedencia y vínculos de perfil/run/instalación, además de las evidencias previas de Core/CAS.

El intento 1 y su auditoría se conservan como baseline anterior al refuerzo de consumo único y espera de verificaciones. Registró 392 ejecuciones correctas, incluyendo una unitaria repetida por un filtro que también alcanzaba el módulo RPC anidado. El intento final amplía cobertura y elimina esa repetición; el filtro tipado ejecuta sus tres pruebas propias. No se presenta la ampliación como un fallo nativo reproducido en el baseline.

La regresión conserva P026 original (176 casos funcionales, 16 wire, 17 observaciones y 9 gates físicos) y el recorrido directo Core/SQLite/CAS de tres intentos, trece eventos y diecisiete negativos: un derivado, original intacto y cero reservas/locks finales. Ese recorrido anterior tampoco acredita caída real del daemon con transformación. No hubo cambios ni aceptación GUI en este incremento.

- [Resumen final, intento 2](P093_runs/qualified-guardian-20261101/attempt2/verification-synopsis.json).
- [Auditoría independiente final](P093_runs/qualified-guardian-20261101/attempt2/post-run-audit.json).
- [Recibo del supervisor cualificado](P093_runs/qualified-guardian-20261101/attempt2/native-qualified-guardian.json) y [log del caso nativo](P093_runs/qualified-guardian-20261101/attempt2/verify-qualified-guardian.log).
- [Regresión de transformación/CAS](P093_runs/qualified-guardian-20261101/attempt2/native-transformation.json) y [propiedad del daemon](P093_runs/qualified-guardian-20261101/attempt2/native-daemon-owner.json).
- [Baseline](P093_runs/qualified-guardian-20261101/attempt1/verification-synopsis.json) y [su auditoría](P093_runs/qualified-guardian-20261101/attempt1/post-run-audit.json).
- [Wrapper congelado](P093_runs/qualified-guardian-20261101/attempt2/frozen-verify.py) y [auditor congelado](P093_runs/qualified-guardian-20261101/attempt2/frozen-audit.py).

## Pendiente para el flujo completo

Faltan el registro servidor del descriptor/clave con el journal y el fence del intento antes de ejecutar; el tratamiento del intervalo entre claim, lanzamiento y activación; `TransformationAttemptGuard` para esa autoridad recuperada; transporte de progreso/cancelación por id; productor/consumidor público y H07 con un trabajo Core, caída/reinicio del daemon y conservación/liberación exacta de recursos. El descriptor de esta prueba contiene un fence de fixture, no una admisión Core; no debe aceptarse un descriptor proporcionado por un cliente como autoridad del intento.

El servicio público conserva `worker_unavailable`. Continúan reservas por fase/volumen, retención privada, reconstrucción de procedencia, política tras pérdida del supervisor/reinicio del sistema, FAT32 completo, conversores/hooks GGUF/ONNX reales y UI. Las demás fases y la integración completa de LARM con runtimes, fuentes, Broker y Agora siguen dentro del objetivo global.
