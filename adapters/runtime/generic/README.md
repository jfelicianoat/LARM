# `generic` — worker de los adaptadores declarativos (P023)

`larm-generic-adapter.exe` (binario del crate `larm-adapter-sdk`, fuente [`main.rs`](main.rs)) ejecuta cualquier adaptador declarativo de la familia `runtime`. No es un adaptador por sí mismo: LARM lo copia en cada paquete creado por `POST /api/v1/adapters/declarative`, junto al documento `declarative.json` y al `manifest.json`.

- Schema publicado del asistente: [`wizard.schema.json`](wizard.schema.json).
- Reglas, placeholders, entorno y métodos: [docs/adapters/declarative.md](../../../docs/adapters/declarative.md).

No se anuncia soporte de ningún runtime concreto: un documento declarativo describe un ejecutable que el operador ya tiene; la cualificación por versión es de P099.
