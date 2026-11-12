# Adaptador falso (P021)

`manifest.json` es la plantilla de las pruebas del host. El ejecutable es el ejemplo `crates/larm-adapter-host/examples/fake_adapter.rs` (lo compila `cargo test`). Cada prueba copia el ejecutable a `bin/` dentro de un paquete temporal, ajusta `--mode` en `executable.args` y el `package_sha256` real. Los modos están documentados en el propio ejemplo.
