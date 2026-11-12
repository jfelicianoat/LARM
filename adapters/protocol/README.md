# Familia `protocol` — ProtocolAdapterPlugin

Paquetes de adaptador fuera de proceso (JSON-RPC/stdio, spec §12, §129) de la familia `protocol`.
Cada paquete vive en `adapters/protocol/<id>/` con su manifiesto conforme a
[contrato 03](../../docs/contracts/03_ADAPTERS_SECURITY.md) (`family: protocol`, `protocol_version: 1.0`).

[`reference`](reference/README.md) implementa las cuatro firmas de la familia contra
un servidor HTTP echo real y controlado. Incluye invocación, cualificación del
transporte, cancelación y un descriptor de capacidad con schemas/estimador.
P103 conserva los adaptadores de protocolo de producto y la inferencia real.
