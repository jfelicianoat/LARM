# ADR-0009 — Única excepción a la prohibición de `unsafe`: DACL de named pipes

**Estado**: aceptado · **Fecha**: 2026-10-11 · **Prompt**: P009

## Contexto

El contrato 03 exige «bootstrap usuario por named pipe ACL SID». Windows solo permite fijar la DACL de un pipe al crearlo, mediante un `SECURITY_ATTRIBUTES`, y eso exige FFI (`ConvertStringSecurityDescriptorToSecurityDescriptorW` y `ServerOptions::create_with_security_attributes_raw` de Tokio, ambas `unsafe`). P001 prohíbe `unsafe` «por defecto a nivel workspace salvo excepciones documentadas». Con `forbid` no había forma de crear la ACL: la rebanada P107 lo dejó como limitación, con la DACL por defecto de Windows.

## Decisión

- El lint del workspace pasa de `unsafe_code = "forbid"` a `"deny"`.
- Hay **una sola** excepción: `apps/daemon/src/winsec.rs` (`#![allow(unsafe_code)]`). Contiene la conversión SDDL → descriptor, la creación del pipe y la liberación del descriptor; cada bloque lleva su `// SAFETY:`.
- DACL: `D:P(A;;GA;;;<SID del usuario>)(A;;GA;;;SY)`. Es protegida, sin herencia, y concede control total solo al usuario que ejecuta el daemon y a SYSTEM. Se añade `PIPE_REJECT_REMOTE_CLIENTS`.
- La usan el pipe de bootstrap del escritorio (P009) y el canal de credenciales de endpoint (P107, contrato 04) mediante `credentials_pipe::PipeFactory`. Los crates no saben nada de ACL.
- `tools/verify_p001.py policies` falla si aparece `unsafe` o `allow(unsafe_code)` fuera de `winsec.rs`.

## Consecuencias

- Los administradores locales no pueden abrir los pipes, aunque podrían tomar posesión del objeto, cosa que el modelo de amenazas de un PC personal acepta.
- La prueba real lee la DACL efectiva del pipe con `GetSecurityInfo` y la compara con la esperada.
- La prueba con un segundo usuario de Windows necesita una cuenta que el operador autorice.
