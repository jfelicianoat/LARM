# Revisión de contrato LARM 3.9.1 — sincronización HTTPS de registro P028

Fecha: 2026-10-24  
Base: paquete 3.9.0  
Estado: revisión preparada tras aprobación del operador.

La revisión 3.9.0 definió el índice firmado y el checkpoint, pero no publicaba una operación para consultar el registro ni una respuesta de estado. Se añaden `GET /api/v1/adapters/registry` y `POST /api/v1/adapters/registry/sync`, junto con los schemas cerrados `RegistryStatus`, `RegistrySyncRequest` y `RegistrySyncReceipt`. El GET requiere `read`; el POST requiere `adapters:write`, cuerpo `{}` e `Idempotency-Key`.

El cliente HTTPS usa el almacén TLS del sistema, valida el certificado, sólo acepta HTTP 200 y contenido JSON, no sigue redirecciones, no reenvía Authorization/cookies/credenciales y limita la respuesta a 16 MiB. Tiene timeout de conexión de 5 segundos y total de 20 segundos. `registry_url=null` devuelve estado deshabilitado y rechaza sincronizaciones.

La aceptación persiste el envelope firmado, el checkpoint, las revocaciones nuevas y `adapter.registry.synced` en una sola transacción. Replay de la misma secuencia y hash devuelve el estado previo sin mutación ni evento; secuencia menor, misma secuencia con contenido distinto o cambio de `registry_id` para la misma URL son conflictos. Las revocaciones siguen siendo acumulativas.

El índice único `adapter_registry_checkpoint_url_unique` enlaza una URL con un único `registry_id`. La migración local es `0010_p028_registry_url_binding.sql`; el paquete publica el equivalente `contracts/migrations/0008_p028_registry_url_binding.sql` y actualiza `contracts/persistence.sql`.

Las pruebas locales y simuladas acreditan el transporte y la persistencia, pero no sustituyen la aceptación HTTPS real con certificado válido y registry de prueba. P024 y P026 permanecen `BLOCKED_REAL`; P028 no pasa a `REAL_PASS`.
