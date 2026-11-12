# P070 — Inspección física de almacenes Ollama

Actualización 2026-10-30. Estado del módulo completo: **BLOCKED_REAL**; objetivo global activo. Los recibos conservan sus timestamps UTC originales.

El nuevo worker de `adapters/runtime/ollama` implementa `runtime.inspect_model` mediante el SDK NDJSON y devuelve el `ModelInspection` publicado. Lee un almacén expresamente configurado; no copia blobs ni crea identidades, bundles o promociones en Core. El paquete no pide escritura, red, ejecución de procesos ni secretos. Los demás métodos obligatorios fallan con un error normalizado y los opcionales pendientes no se anuncian.

La inspección recorre manifiestos locales y anidados, verifica tamaño/SHA256 de config/capas y deduplica archivos sin perder roles. Rechaza hashes/tamaños inconsistentes, modelos remotos, reparse points, rutas fuera de la raíz y ADS, overflow, JSON duplicado/no finito o excesivamente anidado y cancelación. No usa `from` ni `urls` para abrir rutas o hacer peticiones. Si el config indica GGUF, contrasta su cabecera v2/v3.

En Windows mantiene handles de ancestros sin compartir delete y del archivo sin compartir write/delete mientras se lee; los tests físicos de rename/escritura y junction pasan. Son pins por lectura, no un snapshot global atómico ni aislamiento del SO. El host protegido y Core deben volver a validar sus permisos y el conjunto antes de ejecutarlo. La semántica de los flags usados se documenta en [CreateFileW de Microsoft](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew).

`architecture=amd64` del config pertenece a la imagen contenedora: la arquitectura del modelo permanece null. No se infieren capacidades ni contexto. La licencia sigue null/no aceptada aunque su archivo se conserve como artefacto; quantization procede del config. Ni una cabecera GGUF ni un SHA correcto acreditan carga, backend o compatibilidad.

## Evidencia ejecutada

- **22 tests de inspección**, **9 del SDK** y **4 de empaquetado** pasan; 35 en total, sin omisiones. Se conservan las cinco rutas de familia y los 586 archivos/11.666.934 bytes del payload original de stdlib, con DLLs y versiones fijadas. Arquitectura17 y copias contractuales78/3.12.1 pasan. [Recibos finales](P070_runs/store-inspection-20261029/final2/store-tests.command.json).
- El negativo de JSON profundo falló primero: Python aceptaba 2.000 niveles. El límite explícito64 y la prueba que distingue corchetes/quotes escapadas dentro de strings corrigen ese caso. El intento 20 PASS/1 FAIL se conserva en `final/`; no se reemplaza por el resultado final.
- El worker autónomo se ejecuta contra `granite-embedding:30m`: **11/11 comprobaciones PASS**, 15,431s de recorrido. Cinco archivos y **62.536.337 bytes físicos observados**, incluyendo manifiestos/config/licencia; pesos 62.523.136 bytes. SHA y tamaños se contrastan con una lectura independiente de cinco rutas fijadas, antes y después. No se cargan modelos ni se invoca inferencia. [Inspección](P070_runs/store-inspection-20261029/final2/inspection.json) · [recibo completo](P070_runs/store-inspection-20261029/final2/native.command.json).
- Se verifican handshake/nonce, schema estricto, modelo ausente, acceso fuera de la raíz y rechazo explícito de métodos pendientes. El worker sale0. Sus 388 archivos de paquete, perfil local y 2.047 inputs/ejecutable del ámbito seleccionado coinciden; Ollama y sus modelos cargados permanecen iguales. Hash del paquete `9a07a626e2abfc6fea7aaa85256ea9a256d6a9cb55d712125879bc5a8363f09c`. [Auditoría de recibos/hashes/schema](P070_runs/store-inspection-20261029/audited.json).

La [evidencia formal actual](P070_ollama-runtime-host-adapter.evidence.json) contiene seis checks PASS y una aceptación completa BLOCKED. La evidencia anterior de guard local quedó archivada byte a byte. No se repiten Rust/frontend porque esta fase cambia Python y su paquete; sus pruebas anteriores conservan su alcance histórico. El gate integral P070 no se declara ejecutado.

## Trabajo pendiente

Faltan el resto del worker runtime, su cualificación protegida P022/P026, profile lock P099 exacto, inventario central HOST_MANAGED, capacidades/carga/backend reales y leases durables/Broker, además de REAL_PASS de dependencias. La prueba actual es una transcripción directa del SDK, no aceptación del host protegido. No se han aceptado licencias ni modificado la instalación externa.

El operador confirmó ahora Windows desbloqueado y con sesión local activa. Se retoma la aceptación de GUI de LARM con binarios actualizados; esa confirmación resuelve la pregunta de entorno y no convierte los intentos GUI anteriores en PASS.
