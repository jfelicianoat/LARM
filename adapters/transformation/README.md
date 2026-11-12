# Familia `transformation` — TransformationPlugin

Paquetes de adaptador fuera de proceso (JSON-RPC/stdio, spec §12, §129) de la familia `transformation`.
Cada paquete vive en `adapters/transformation/<id>/` con su manifiesto conforme a
[contrato 03](../../docs/contracts/03_ADAPTERS_SECURITY.md) (`family: transformation`, `protocol_version: 1.0`).

[`reference`](reference/README.md) implementa describe/plan/execute/verify con
conversión streaming de un archivo binario propio a Base64, sin alterar el original.
Incluye hashes/procedencia, ledger privado de intentos, journal/outbox, reservas y
recuperación tras crash. P065/P093 conservan las transformaciones de modelos de
producto y su admisión global.
