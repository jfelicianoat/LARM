# P141 — Autenticación privada OAuth, SSH, cookies y Git LFS

**Estado: `BLOCKED_REAL`.** Este avance implementa el vínculo de cuenta requerido para resolver credenciales dentro de un worker y completa el recorrido Basic en el escritorio con el perfil HTTP P048. No cierra P141: API_TOKEN, OAuth, SSH, cookies y Git LFS siguen pendientes, y no se ejecutó la prueba con una fuente privada y una cuenta autorizadas.

## Implementado

- La revisión contractual 3.8.0 añade `account_id` nullable y obligatorio a `AuthProbe`. Para `NONE`, la cuenta y la referencia deben ser null. Para Basic, el host vuelve a resolver fuente, cuenta y `CredentialReference` en SQLite, valida sus vínculos, tipo de secreto y permisos exactos antes de iniciar el worker.
- El componente `AuthProbe` de `docs/api/openapi.json` también refleja el campo obligatorio nullable del schema canónico; el generador OpenAPI comprueba que el artefacto coincide.
- El worker P048 recibe sólo los identificadores. En el proceso worker, `keyring` solicita la entrada de Credential Manager con `service=larm/source/{source_id}` y `username=account_id`. El probe Basic exige HTTPS salvo loopback de prueba, no sigue redirects y clasifica 401/403 sin incluir credenciales en RPC, respuestas o logs.
- La pantalla Fuentes lista las cuentas de la fuente, permite crear y vincular una cuenta con `If-Match`, y guarda Basic por la ruta protegida del contrato. El formulario nunca carga credenciales existentes y vacía usuario y contraseña tras cada intento.
- Se regeneraron los schemas de métodos y el índice de capacidades, y se sincronizaron las copias normativas de `docs/spec` y `docs/contracts` desde el paquete contractual 3.8.0.

## Verificación

- `cargo test --workspace -j 1 -- --test-threads=1`: 1.148/1.148 PASS en la revalidación del 2026-10-24; `cargo clippy --workspace --all-targets -- -D warnings` y `cargo fmt --all -- --check`: PASS en la validación previa. La repetición fue necesaria porque el sandbox inicial denegó sockets locales `127.0.0.1` usados por fixtures; con loopback habilitado, el test completo pasó.
- La puerta oficial `tools/check.ps1 -Prompt P141 -Real` terminó 14/14 PASS: workspace, arquitectura, 63 copias contractuales, Tauri, lint, TypeScript, 92 pruebas del escritorio y build.
- `cargo run -p larm-daemon -- openapi --out docs/api/openapi.json --check` pasa. El SDK valida el schema cerrado, UUIDv7, vínculo `account_id`/referencia y roundtrip; el host valida los vínculos antes de iniciar el worker.
- El test focal de Fuentes pasó 12/12. Verifica la creación/vínculo de cuentas, la revisión `If-Match`, el guardado Basic por la ruta protegida y la limpieza de los campos; P048 pasó 15/15 y los tests declarativos de procesos/health 7/7.
- El runner P141 registra 9 PASS, 0 FAIL y 2 BLOCKED; `validate_evidence.py` valida el JSON. API_TOKEN/OAuth/SSH/cookies/Git LFS y la prueba de fuente privada real quedan explícitamente bloqueados.
- `p026_schemas.py check`: 40 firmas / 80 schemas PASS; `p026_capability_schemas.py check`: 18 capacidades / 36 schemas PASS; `p026_schema_corpus.py check`: 8.915 casos PASS; pruebas Python contractuales 3/3; smoke empaquetado P048 PASS.
- `tools/verify_pack.py`: 3.105 checks y conformidad v3 197 checks PASS. `tools/package_copy.py sync` actualizó 63 copias y el `check` posterior pasó con paquete 3.8.0.
- La revisión de `AuthProbe` en el paquete 3.8.0 añade `account_id` nullable obligatorio; la BD conserva sólo la referencia protegida. El registro de evidencia mantiene P141 en `BLOCKED_REAL` hasta implementar y cualificar los mecanismos restantes y completar la fuente privada autorizada.

## Pendiente

- Implementar credenciales API_TOKEN con cabecera fijada por perfil; flujos OAuth device y authorization code con PKCE, refresh/revoke; SSH agent y verificación de host key; cookies acotadas por dominio/path/caducidad; y autenticación Git LFS por OID/tamaño.
- El paquete 3.8.0 exige `auth_flow_mapping`, pero no define su estructura ni la operación para asociar tokens a una cuenta existente. La [propuesta de revisión contractual](P141_revision-contractual-oauth-propuesta.md) documenta el hueco y un posible cierre; permanece no normativa hasta su revisión.
- Completar integración Windows verificable por cada mecanismo implementado, incluidos los negativos de 401/gated 403, renovación única, token caducado, backoff OAuth, state/PKCE, host key distinta, fuga entre origins, pointer LFS, identidad UNC y redacción.
- Ejecutar el probe y la renovación/revocación contra una fuente privada real autorizada. No se reutilizan tokens de AI Broker o Agora como credenciales de fuente.
- Las dependencias P045, P046, P047, P048 y P049 permanecen `BLOCKED_REAL`; P141 no puede pasar a `REAL_PASS` mientras falten.

La evidencia estructurada está en [P141_autenticacion-privada-oauth-ssh-cookies-y-git-lfs.evidence.json](P141_autenticacion-privada-oauth-ssh-cookies-y-git-lfs.evidence.json). La puerta oficial actual está en [check_P141_real.json](check_P141_real.json) y termina PASS; P141 sigue `BLOCKED_REAL` por alcance de implementación y aceptación real pendiente.
