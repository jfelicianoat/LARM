# LARM — Local AI Runtime Manager

Administrador de capacidad de IA local en Windows (PC IA): hardware, almacenamiento
content-addressed, modelos desde fuentes federadas, 20 runtimes con instancias y leases,
transformaciones, benchmarks y protocolo con AI Broker.

- Spec: [docs/spec/LARM_SPEC_V1.md](docs/spec/LARM_SPEC_V1.md) (íntegra, copiada con hashes del paquete `LARM_prompts_v3`)
- Contratos vinculantes: [docs/contracts/](docs/contracts/)
- Estado de implementación: [docs/implementation/STATE.md](docs/implementation/STATE.md)
- Correcciones y pruebas de la última auditoría: [revalidación del 16 de octubre](docs/reports/AUDIT_FIXES_2026-10-16.md)
- Ejecutable con interfaz verificada: [compilación del escritorio](docs/operations/desktop-build.md)
- Paquete de prompts vigente: `../LARM_prompts_v3` (revisión 3)

## Desarrollo

En Windows, inicia el daemon y el escritorio con doble clic en `iniciar-larm.bat`. La primera ejecución puede compilar varios componentes; si faltan dependencias de Node, ejecuta `npm ci` desde esta carpeta.

```text
npm run daemon:dev    # daemon Axum en modo desarrollo
npm run tauri:dev     # shell desktop Tauri 2
tools/check.ps1 -Prompt P001 [-Real]   # quality gates locales
```

Stack: Rust/Tokio · Axum JSON/SSE · Tauri 2 + React + TypeScript · SQLite (SQLx, WAL) · reqwest/rustls · tracing.
Políticas: `unsafe` prohibido a nivel workspace; `clippy -D warnings`; estados CODE_PASS/REAL_PASS/BLOCKED_REAL.
