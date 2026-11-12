# P050/P127 — Método e identidad de la sincronización cualificada

Estado global: `BLOCKED_REAL`. Incremento verificado contra el paquete 3.11.1: 70/70 pruebas focales y 1/1 ensayo nativo TCP (46,18 s).

## Fallos reproducidos

El ejecutor de vigilancia exigía `source.sync` dentro de `capabilities`, aunque el método sí figuraba en `manifest.methods`. P048 HTTP y otros paquetes válidos tienen capacidades opcionales vacías; el método de sincronización es obligatorio en el protocolo source. Se rechazaban antes del RPC.

Además usaba `system:source-watcher` como actor de la llamada al host. El perfil/grant P022/P026 está ligado al actor que lo aprobó, de modo que el host rechazaba la identidad distinta. La composición de descarga ya usa el actor de la ejecución cualificada para ese mismo guard.

El [ensayo previo](P127_runs/sync-binding-20261027/red.log) conserva 1 PASS/2 FAIL. La corrección comprueba el método declarado y conserva el actor del Run cualificado, con una correlación nueva. Los eventos del scheduler continúan identificando `system:source-watcher`; no se modifica aprobación, expiración de 900 s, permisos, hashes ni controles P022. Fuente privada/auth y método ausente siguen fallando cerrados.

## Ensayo nativo completado

El caso utiliza el daemon TCP y ejecutable original P048 con P022 y cualificación persistente reales, capacidades opcionales vacías y método `source.sync` declarado. Crea una vigilancia por API, exige replay sin duplicación y prepara su instante de vencimiento en SQLite para no esperar una hora. La instalación inicial y el binding de fuente son preparación explícita del test, no aceptación del registro P043.

Exige RPC real al origen loopback, snapshot UNKNOWN→HEALTHY y fecha observada, límite desconocido conservado como null, eventos/outbox, notificación de candidato y ausencia de modelos/artefactos/localizaciones/jobs/reservas creados automáticamente. Reinicia el daemon y exige preservar la observación y la vigilancia; después elimina la vigilancia y revoca el principal temporal propio.

## Alcance

No acredita una fuente externa/privada, ingesta completa, descarga automática ni GUI conjunta. La [propuesta 3.12.0](P044_P052_revision_3_12_0_propuesta.md) registra separadamente la contradicción entre exigir un bundle físico verificado y permitir su primera descarga; el operador autorizó publicar e implementar esa revisión el 2026-10-27. Este incremento se cierra antes de cambiar el paquete 3.11.1 y no elimina ese guard por sí solo.

## Resultados y evidencia

Pasan daemon 39/39, worker 8/8, persistencia 7/7, API 1/1 y host 15/15: 70 pruebas focales, sin doble contar las tres del binding incluidas en daemon. Clippy host/daemon con todos los targets y `-D warnings`, formato, AST/rutas de runners y 74/74 copias del paquete 3.11.1 pasan. No se repitió la suite global ni la GUI.

El [resultado nativo](P127_runs/source-watch-tcp/01a17e20-dc1d-73c2-9002-32c1318571f0/result.json) conserva 39 casos/16 observaciones SourceHTTP aprobados, un HEAD real sin GET al sincronizar, una notificación, cero modelos/artefactos/localizaciones/descargas/reservas y cero eventos sin outbox. Confirma replay, snapshot durable tras reinicio, eliminación de la vigilancia y revocación del principal temporal. Los 48 hashes de implementación congelados y ambos ejecutables coincidieron al cerrar la ejecución. [Comprobaciones](P127_runs/sync-binding-20261027/increment.checks.json) · [Entradas congeladas](P127_runs/source-watch-tcp/01a17e20-dc1d-73c2-9002-32c1318571f0/implementation.sha256.json).

## Intentos fallidos conservados

El primer ensayo nativo terminó FAIL antes de sincronizar: la cualificación pasó 32 casos hasta cancelación cooperativa y luego devolvió `conformance_file_unavailable`; no guardó el error original del sistema. Su evidencia está en [diagnóstico](P048_runs/daemon-tcp-failures/01a17e15-9187-75cd-9d67-7d2b08d6c449/source-http.json) y [transcripción](P127_runs/sync-binding-20261027/native.log). No se cuenta como PASS.

Se reprodujo por separado en Windows una carrera de sustitución atómica del fichero de control mientras un lector mantiene un handle transitorio: [rojo](P127_runs/sync-binding-20261027/control-red.log), 0/1. El host reintenta sólo los errores Windows 5/32/33 durante un máximo de 500 ms, conserva el fichero anterior hasta sustituirlo atómicamente y añade diagnóstico sin rutas ni secretos. El test y los 15 tests de host pasan. La carrera es compatible con el fallo del primer nativo, pero su registro no demuestra cuál fue la operación exacta. No se cambian oráculos, timeouts RPC, permisos ni el veredicto de la suite.
