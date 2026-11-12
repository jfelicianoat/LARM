# Familia `benchmark` — BenchmarkPlugin

Paquetes de adaptador fuera de proceso (JSON-RPC/stdio, spec §12, §129) de la familia `benchmark`.
Cada paquete vive en `adapters/benchmark/<id>/` con su manifiesto conforme a
[contrato 03](../../docs/contracts/03_ADAPTERS_SECURITY.md) (`family: benchmark`, `protocol_version: 1.0`).

[`reference`](reference/README.md) implementa describe/run/validate con mediciones
HTTP reales contra el servidor echo Rust controlado, muestras/artefactos durables
y estadísticas verificadas desde Rust. P110–P112 conservan benchmarks de modelos,
comparaciones y evaluaciones de producto.
