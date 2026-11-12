# P070 — Ollama runtime/host adapter

Actualización 2026-10-30: worker autónomo de inspección física implementado; 35 tests y11/11 comprobaciones reales sobre cinco archivos/62.536.337bytes PASS, sin mutación/carga. Arquitectura17/copias78 y auditoría de paquetes/inputs/schema PASS. P070 sigue BLOCKED_REAL por el resto del runtime/host protegido/P099/leases. [Informe actual y límites](P070_store_inspection_2026-10-30.md). El resto de este documento describe el incremento anterior del guard de modelos remotos; sus recibos y congelación corresponden a esa fase.

Estado: **BLOCKED_REAL**, incremento parcial verificado del 2026-10-29. El objetivo global de LARM continúa activo.

## Hechos observados

La captura antigua P099, fijada a Ollama 0.40.6 y su ejecutable, rechazó correctamente el binario instalado porque su SHA256 cambió. No se modifica su allowlist ni se traslada su evidencia a otra versión.

La nueva observación independiente de sólo lectura obtiene Ollama **0.40.11**, 28 registros en `/api/tags` y tres modelos cargados en `/api/ps`. Dos registros declaran `remote_model`/`remote_host`: `beam:cloud` y `gemma4:31b-cloud`. El registro menor que no declara esos campos es `granite-embedding:30m`, 62.534.804 bytes según la API; esa cifra no demuestra el contenido físico ni una instalación verificada.

Se comprueban PID, creación del proceso, ruta, SHA256 del ejecutable, firma Authenticode y direcciones de escucha antes y después de cada petición. El proceso observado es 47416, creado el 2026-10-25T21:55:01.1641190Z; el ejecutable tiene SHA256 `cc00918e2d25577969c929a1004be054781b8bff25819c1be19ca2f7b7bb4012` y firma válida de Ollama Inc. Escucha en `::`; las tres peticiones se envían exclusivamente a `127.0.0.1:11434`. No se declara aislamiento loopback del servicio.

La observación realiza exactamente tres GET: versión, inventario y modelos cargados. No inicia ejecutables, carga/descarga modelos, copia blobs, crea perfiles o leases ni cambia configuración. Los datos son **UNQUALIFIED**. El primer intento falló por un carácter de sintaxis en el observador; los siguientes dos por el `PSModulePath` heredado, incompatible con el módulo Security de Windows PowerShell. El cuarto usa el descubrimiento nativo de módulos y pasa. Los tres fallos se conservan y no hicieron peticiones HTTP.

Evidencia: [recibo completo](P070_runs/discovery-20261029/observation.attempt4.json), [versión](P070_runs/discovery-20261029/version.response.json), [inventario](P070_runs/discovery-20261029/tags.response.json), [cargas](P070_runs/discovery-20261029/ps.response.json), [observador](P070_runs/discovery-20261029/observe.py).

## Cambio focal verificado

El puente existente seleccionaba una etiqueta por nombre/tamaño sin comprobar si delegaba a un modelo remoto. La primera corrida reproduce tres fallos: publicación de una etiqueta remota, aceptación de metadatos remotos en `/api/show` y envío de input después de que una etiqueta pase a remota. El caso de modelo ausente ya pasaba. [RED: 1 PASS/3 FAIL](P070_runs/discovery-20261029/red.command.json), sin cambios de inputs durante la corrida.

`external_adoption.rs` rechaza `remote_host`/`remote_model` no vacíos tanto en tags como en show. Un tipo inválido, incluido null explícito, falla cerrado; la ausencia de los campos o una cadena vacía conserva el caso local definido por la API. Se aplican los controles antes del catálogo, de admitir/cargar y al volver a consultar antes de dispatch o de restaurar keep_alive. La comprobación de token/estado ACTIVE se repite después del IO de metadatos para impedir enviar una petición si la lease se liberó mientras esperaba. El cliente tampoco sigue redirecciones HTTP, incluidas 307 que conservarían el cuerpo de una petición. No cambia ningún DTO, schema, política por defecto ni contrato publicado.

La relectura detecta cambios observables de aliases; el host ajeno puede cambiar entre comprobación y dispatch. No aporta aislamiento ni una operación atómica contra un host mutable. Tampoco demuestra por sí sola integridad del bundle, compatibilidad ni capacidades por modelo.

La fuente primaria de la versión observada declara los campos remotos tanto en `ListModelResponse` como en `ShowResponse`: [api/types.go, commit 675150d6ad2747df1e218ef59de9194bc4a7e538](https://github.com/ollama/ollama/blob/675150d6ad2747df1e218ef59de9194bc4a7e538/api/types.go). Se conserva [snapshot y hash](P070_runs/discovery-20261029/primary-source.json); es investigación documental y no un profile lock.

## Pruebas y límites

La corrida final pasa **121/121 pruebas unitarias de larm-runtime**, incluidas diez regresiones de adopción local: campos remotos/malformados, show remoto, modelo ausente, caso local, cambio de tags/show antes de dispatch, liberación concurrente, keep_alive y redirecciones GET/POST. La prueba real opt-in permanece ignorada en ese comando; se ejecuta separadamente. Pasan formato Rust, Clippy del crate para todos sus targets, arquitectura17 y copias contractuales78/3.12.1. Los [recibos finales](P070_runs/discovery-20261029/final/runtime-lib.command.json) conservan inputs antes/después; la corrida previa120/120 queda histórica por el control concurrente añadido después.

La prueba nativa opt-in pasa **1/1**, 0,24s en el binario / 8,405s de wrapper. Usa el probe de producción para leer `granite-embedding:30m`, rechazar los dos aliases remotos y detectar un modelo inexistente. No crea leases, jobs ni secretos, no acepta external_local_trusted y no solicita carga ni inferencia. PID/creación/ruta/hash/firma y los mismos tres modelos cargados coinciden antes/después. Se congelan y contrastan **2.041 archivos del ámbito seleccionado, incluido un ejecutable**; no se recompila ni cambia el código durante esa aceptación. [Recibo nativo](P070_runs/discovery-20261029/native.command.json) · [log](P070_runs/discovery-20261029/native.log) · [auditoría independiente de hashes/recibos/schema](P070_runs/discovery-20261029/audited.json).

La [evidencia formal de esta fase, archivada](P070_runs/store-inspection-20261029/prior.evidence.json), contiene siete comprobaciones PASS y una aceptación completa BLOCKED. Las dos pruebas REAL sólo acreditan lectura y controles de metadatos, sin promover P070 a REAL_PASS. No se repitieron frontend ni workspace completo porque no se modificaron sus interfaces; el gate integral solicitado por P070 siguió pendiente.

## Pendientes de aceptación

Faltan el worker runtime completo de `adapters/runtime/ollama`, su empaquetado/cualificación P022/P025/P026 y profile lock P099 de la versión actual, la ingesta central de artefactos HOST_MANAGED sin copiar blobs, capacidades probadas por modelo, gestión limitada por ownership/policy y aceptación durable completa de leases. Las dependencias P025/P030/P036/P099/P090 no acreditan aquí REAL_PASS. No se habilita la adopción confiada del Ollama del usuario.

El contexto oficial de P070 se selecciona con `prepare_prompt.py`: 92 archivos, paquete 3.12.1, sin afirmar que los informes de dependencias equivalgan a aceptación. La comprobación integral `tools/check.ps1 -Prompt P070 -Real` y los smokes de carga/inferencia no se han ejecutado en este incremento. La GUI continúa pendiente de acceso a la sesión Windows.
