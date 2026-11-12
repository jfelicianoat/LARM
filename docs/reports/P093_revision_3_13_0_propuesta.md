# Propuesta 3.13.0 — reinicio explícito de transformaciones

Fecha: 2026-10-30. Estado: PROPUESTA; no publicada ni implementada como contrato vigente.

Resolución posterior (2026-10-30): el operador respondió «Autorizar revisión e implementación». Se publicó el [contrato3.13.0](../contracts/REVISION_3_13_0_P093.md) y se implementaron/verificaron las capas HTTP/dominio/SQLite; [resultado y recorrido nativo pendiente](P093_restart_3_13_0_2026-10-30.md). El texto siguiente conserva la propuesta revisada. La autorización ya está resuelta y no debe repetirse.

## Problema comprobado

El requisito H07 exige reiniciar el mismo trabajo FAILED/FAILED_RECOVERABLE con el input original íntegro, nuevo preflight, guards/reservas y `attempt` incrementado. `TransformationJob.retry_with_preflight` ya expresa esa transición en dominio.

El contrato de API vigente sólo define crear un plan, ejecutar un plan y listar transformaciones. `Transform` y `ExecutePlan` son objetos cerrados; ninguno identifica el trabajo anterior. Por tanto, la API no permite escoger explícitamente qué trabajo reiniciar. Inferirlo buscando trabajos con el mismo input sería ambiguo; colocarlo en parámetros de herramienta cambiaría su significado.

Fuentes: `contracts/11_CONFORMIDAD_V3.md`, H07; `contracts/01_DOMAIN_STATES.md`, jobs de transformación; `contracts/api-operations.json`, las tres operaciones P093; `schemas/larm.schema.json`, `Transform` y `ExecutePlan`. El contrato00 exige resolver con el operador las contradicciones de alcance y contrato, conservando los requisitos.

## Cambio concreto propuesto

Añadir una operación, reutilizando los DTOs existentes:

```json
{
  "id": "P093_post__transformations_id_restart",
  "method": "POST",
  "path": "/api/v1/transformations/{id}/restart",
  "scope": "jobs:write",
  "request_schema": "larm.schema.json#/$defs/ExecutePlan",
  "response_schema": "larm.schema.json#/$defs/JobReceipt",
  "owner": "P093",
  "mode": "job",
  "success_status": 202,
  "path_parameters": {
    "id": {
      "type": "string",
      "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"
    }
  },
  "required_headers": ["Idempotency-Key", "If-Match"]
}
```

`id` identifica el `TransformationJob`, `If-Match` su `row_version`. El cuerpo conserva `plan_id`, `plan_hash` y `approval_id` nullable obligatorio. El cliente prepara primero un plan nuevo con `POST /transform-plans`, lo revisa y después confirma este reinicio. Ejecutar un plan nuevo por `POST /transformations` continúa creando un trabajo nuevo; no selecciona implícitamente un fallo anterior.

Reglas:

1. Sólo el propietario puede reiniciar el trabajo, con scope `jobs:write`; no se delega autoridad desde el contenido del plan.
2. Sólo FAILED o FAILED_RECOVERABLE y sin output publicado. COMPLETED/CANCELLED permanecen terminales. Estado incorrecto devuelve INVALID_STATE_TRANSITION; versión obsoleta, STALE_REVISION.
3. Plan sellado vigente, no consumido, del actor solicitante, con el mismo `input_variant_id` y exactamente los hashes originales. Se vuelven a leer los archivos físicos. Un hash distinto falla INTEGRITY_FAILED antes de iniciar herramienta o cambiar el trabajo.
4. Herramienta/versión/parámetros quedan fijados por el plan nuevo. Cualificación, permisos, aprobación, ownership, paths, presupuesto, espacio observado y límites se revalidan; no se hereda una aprobación del intento anterior.
5. Nuevo guard/reserva y actualización CAS del mismo job en una transacción con consumo de plan, journal, evento/outbox e idempotencia. Incrementa `attempt` una sola vez, entra QUEUED y conserva íntegros error/plan/historial del intento anterior. Un fallo de persistencia revierte todas estas mutaciones.
6. Idempotencia liga actor/operación/clave al ID del job, cuerpo y `If-Match`. Un replay exacto devuelve el recibo original aunque la versión del job haya avanzado; distinto contenido o destino con la misma clave devuelve CONFLICT. No inicia dos workers.
7. Los temporales del intento anterior no se reutilizan como output verificado. Se conservan o limpian sólo con journal y ownership comprobados; nunca se toca el original por este reinicio.
8. Se emite `transformation.state_changed`, action `restarted`, con job/attempt/plan hash y correlación; sin rutas personales, credenciales ni contenido del input. La UI usa esta operación para Reiniciar tras presentar el plan nuevo.

La misma regla de contador se mantiene en el ciclo: una ejecución inicial se reclama como intento1; el reinicio aumenta al siguiente intento al aceptarse y el claim de ese reinicio no vuelve a aumentarlo. El historial conserva los intentos previos y sus eventos. No se presenta una caída como una ejecución completada.

## Publicación e implementación si se autoriza

- Publicar esta decisión en `contracts/REVISION_3_13_0_P093.md`, contrato00, contrato07, H07/contrato11, tabla de operaciones y manifiesto del paquete; regenerar sus copias y OpenAPI.
- Mantener los schemas `Transform`, `ExecutePlan`, `Plan` y `TransformationJob` actuales; el almacenamiento ya admite ambos estados fallidos y el historial, por lo que esta operación no exige una columna nueva.
- Conectar servicio de planes, admisión/reservas/guards, worker externo y verificación/registro de outputs. El reinicio no se anunciará operativo antes de sus pruebas.
- Conservar los bloqueos reales de P016/P040/P051 y la cualificación concreta de cada herramienta. La conversión GGUF real P094 seguirá pendiente hasta probar una instalación/versiones llama.cpp y sus outputs; una transformación binaria controlada no acredita GGUF.

## Oráculos de aceptación

- API: sin scope, otro actor, ID/header/cuerpo inválido, If-Match obsoleto, estados terminales, plan viejo/consumido, hash cambiado y aprobación insuficiente.
- SQLite: concurrencia y replay exacto/replay conflictivo; fallo de outbox/journal/reserva revierte job, intento e historial sin consumo parcial.
- Proceso real: interrumpir worker no reanudable, arrancar daemon, consultar FAILED_RECOVERABLE, comprobar hash del original, crear y confirmar un plan nuevo y finalizar el mismo job con intento siguiente; sin procesos ni reservas huérfanos.
- Verificar output completo con hashes y smoke cuando exista validador runtime; no registrar salida parcial ni borrar el original antes de verificación/policy autorizada.

Se probarán las rutas iniciales existentes y esta operación. No se usará un mock o una constante de esquema como prueba del recorrido nativo completo.
