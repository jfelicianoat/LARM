# P101 — Contrato LARM con AI Broker y LocalRuntimeResolver

Fecha: 2026-10-23  
Estado: `BLOCKED_REAL`

Se documentó y revalidó la rebanada actual del puente LARM–Broker. El Broker puede leer el catálogo bajo `read`, pedir/consultar/renovar/liberar leases bajo `leases:manage` y obtener la credencial de endpoint por el named pipe local. El catálogo P101 coincide entre el paquete 3.3.0, el espejo LARM, el schema `BrokerCatalog` y OpenAPI. El puente permanece desactivado por defecto.

## Comprobaciones

La evidencia focal registra **9 PASS, 0 FAIL y 1 REAL BLOCKED**. Pasan 5 pruebas de rutas/autorización/cabeceras del API, 16 de adopción y leases con fixture HTTP local, 4 del pipe de credenciales y 6 de DTOs. También pasan la verificación de contrato/OpenAPI, la generación OpenAPI, `cargo fmt`, Clippy de todo el workspace y las 57 copias del paquete 3.3.0.

La puerta oficial P101 terminó **7/7 comprobaciones PASS**, incluida la suite serial completa del workspace, arquitectura, copia contractual y esta evidencia focal. El gate se ejecutó con las pruebas frontend y Tauri excluidas; ambas conservan sus recibos de fase separados.

Las pruebas del runtime usan un servidor fixture en `127.0.0.1`; no validan consumo por un AI Broker real. No se usaron ni guardaron tokens de Broker o Agora.

## Pendiente para cerrar P101

- P100 ya persiste creación, renovación idempotente, vencimiento y liberación, y conecta la admisión con referencias SQLite cualificadas de `RuntimeInstance` e `InferenceEndpoint`; el flujo pasa pruebas locales con HTTP fixture. Falta aceptar el ciclo completo con el daemon Windows y un runtime externo real.
- P009 mantiene pendiente su aceptación con un segundo usuario de Windows y P069 aún no está cualificado; ambas dependencias deben alcanzar `REAL_PASS`.
- El gateway de endpoint que devuelve la adopción no tiene una operación en `api-operations.json` ni una ruta en OpenAPI. La rebanada actual permite `chat/completions` y `embeddings` sin streaming, pero se mantiene como implementación provisional hasta publicar el contrato propietario.
- Hace falta una prueba real desde el Broker configurado contra una variante y un runtime cualificados, cubriendo catálogo, lease, credencial, inferencia, cancelación, renovación y liberación.

El token administrativo de AI Broker y el token de endpoint de una lease son credenciales distintas. El primero no se copia a LARM; el segundo se entrega por el pipe de Windows y vive en memoria del Broker. Agora queda fuera de esta fase.

Evidencia: [PromptEvidence](P101_contrato-larm-con-ai-broker-y-localruntimeresolver.evidence.json). Contrato de uso: [ai-broker-contract.md](../api/ai-broker-contract.md).
