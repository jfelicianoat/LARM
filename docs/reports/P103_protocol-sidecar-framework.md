# P103 — Protocol Sidecar framework

Fecha: 2026-10-21  
Estado: **BLOCKED_REAL**

## Resultado

Se implementó un primer proceso sidecar fuera del daemon, en `adapters/runtime/reference-sidecar`. Lee su configuración de startup desde stdin, enlaza exclusivamente `127.0.0.1` en un puerto efímero y publica un endpoint con token bearer por instancia. El registro de readiness no devuelve el token. El perfil y el binario CLI se identifican mediante SHA-256; las peticiones validan los envelopes `protocol.invoke`, `protocol.cancel` y `protocol.describe` con los schemas compartidos.

El subconjunto publicado es deliberadamente explícito: REST propio (`GET /v1/describe`, `GET /v1/metrics`, `POST /v1/invoke`, `POST /v1/cancel`), una sola capacidad `text.generate`, mensajes de texto etiquetados por rol, sin streaming, overrides de generación, tools ni media. Las funciones no soportadas devuelven `UNSUPPORTED_CAPABILITY`. El CLI se ejecuta como proceso hijo sin shell y con entorno heredado limpio; se limitan concurrencia y salida, se drenan stdout/stderr, y se propagan request/correlation/instance IDs. No se anuncia compatibilidad OpenAI.

## Pruebas

- `cargo test -p larm-protocol --all-targets -j 1 -- --test-threads=1`: **10 PASS, 0 FAIL** (4 unitarias y 6 de integración). El test principal inicia el binario sidecar externo, le entrega configuración por stdin, llama las rutas HTTP reales en loopback y comprueba el CLI echo, la respuesta de readiness sin token y la correlación.
- Las integraciones verifican rechazo de streaming/tools e input incompleto, autenticación obligatoria, cancelación que aborta el proceso hijo, terminación al vencer el timeout, rechazo de stdout superior al límite configurado y liberación del slot.
- `cargo clippy -p larm-protocol --all-targets -- -D warnings`: PASS.
- `tools/check.ps1 -Prompt P103 -Real`: **13 PASS, 1 FAIL** en la corrida final. Pasaron `cargo_fmt`, `cargo_clippy`, `test_fixtures`, `cargo_test` del workspace, arquitectura, formato/Clippy/tests del escritorio, lint, typecheck, tests y build del frontend; también pasó el runner formal de P103.
- El único fallo común es `package_copy` para `docs/contracts/adapter-methods.json`, `docs/contracts/api-operations.json` y `docs/contracts/schemas/larm.schema.json`, las tres diferencias locales ya documentadas frente al paquete canónico. Se conservaron; no se modificaron como parte de P103.
- Una corrida completa anterior falló de forma transitoria en `parada_no_espera_indefinidamente_un_escritor_bloqueado_en_stdin`; el test pasó aislado y la corrida completa repetida pasó `cargo_test`.
- El test de loopback necesitó ejecutar la suite con permisos de red local porque el sandbox de pruebas deniega la conexión TCP (`10013`). La ejecución autorizada de la suite focal pasó; esto no abre puertos externos ni instala runtimes.
- Evidencia focal real de producto: BLOCKED. El proceso echo es un fixture y P036 no tiene evidencia; P021 y P026 continúan `BLOCKED_REAL`, por lo que falta conectar/cualificar el lifecycle y el consumidor.

Recibos: [`check_P103_real.json`](check_P103_real.json), [`check_runs/20261021T2205485385079Z`](check_runs/20261021T2205485385079Z) y [evidencia formal P103](P103_protocol-sidecar-framework.evidence.json). La evidencia P103 queda `BLOCKED_REAL` con el test contractual PASS y la aceptación de runtime real BLOCKED.

## Límites y siguiente trabajo

Este incremento no crea una integración lista para modelos: la plantilla de mensajes role-tagged solo es un mapping de referencia que requiere cualificación por runtime; la cancelación explícita está cubierta, pero la desconexión del cliente aún no cancela el proceso; el CLI puede estar sujeto a procesos descendientes que requieren contención Job Object; y el sidecar aún no está iniciado/propiedad de una RuntimeInstance ni enlazado a leases. Tampoco hay un runtime real o un consumidor Broker en la prueba.

Para completar P103 se deben alcanzar `REAL_PASS` en P036, P021 y P026, integrar el lanzamiento/cancelación/cierre al lifecycle real de instancia y probar el subset contra un CLI runtime real y un consumidor real. El fixture no prueba inferencia.
