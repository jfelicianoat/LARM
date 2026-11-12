# Familia `runtime` — RuntimeAdapterPlugin

Paquetes de adaptador fuera de proceso (JSON-RPC/stdio, spec §12, §129) de la familia `runtime`.
Cada paquete vive en `adapters/runtime/<id>/` con su manifiesto conforme a
[contrato 03](../../docs/contracts/03_ADAPTERS_SECURITY.md) (`family: runtime`, `protocol_version: 1.0`).

Estructura creada en P001; los adaptadores llegan con sus prompts propietarios (P020–P025, P060–P083).
- [`generic/`](generic/README.md): worker de los adaptadores declarativos (P023). No anuncia soporte de ningún runtime concreto.
