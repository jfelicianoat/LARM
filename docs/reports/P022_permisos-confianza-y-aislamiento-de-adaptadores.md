# P022 — Permisos, confianza y aislamiento de adaptadores

**Fecha**: 2026-10-12 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Pasan 13 pruebas (9 reales, con procesos y ficheros reales en Windows) y quedan 2 bloqueadas. Además, sus dependencias P020 y P021 no tienen REAL_PASS. [evidence.json](P022_permisos-confianza-y-aislamiento-de-adaptadores.evidence.json) · [operación](../adapters/security.md)

## Qué se construyó

**SDK** (`crates/larm-adapter-sdk/src/permissions.rs`)

- **`Grants`**: resuelve los cinco permisos del manifiesto y comprueba cada `Access`.
  - Rutas reales, canonicalizadas, sin `..` y comparadas por componentes.
  - Origen de red exacto (esquema, host y puerto), sin usuario en la URL.
  - Ejecutable y secreto exactos.
- **`PermissionDelta`**: calcula qué pide de más una versión nueva frente a lo ya aceptado.
- **Firma**: Ed25519 (`ed25519-dalek` 3.0.0, fijado en `environment.lock.json`) sobre el RFC 8785 del manifiesto con `signature: null`.
  - **`TrustStore`**: claves fijadas por el operador y paquetes revocados.
  - **Estados de firma (§131)**: `OFFICIAL`, `SIGNED_THIRD_PARTY`, `UNSIGNED` y `CUSTOM`.
  - **Confianza (columna `adapter_installation.trust`)**: `UNTRUSTED`, `VERIFIED` y `REVOKED`.
- **`package_hash`**: hash de los ficheros del paquete; rechaza los enlaces simbólicos.
- **Digests de aprobación**: `execution_digest` y `elevation_digest`.

**Host** (`crates/larm-adapter-host/src/policy.rs`)

- **`Policy::authorize`** comprueba, en este orden:
  1. que LARM no está elevado (`whoami /groups`);
  2. que el hash del paquete coincide;
  3. la confianza:
     - `REVOKED` se bloquea;
     - `VERIFIED` se ejecuta;
     - `UNTRUSTED` solo con un `TrustedOnlyGrant`.
- **`TrustedOnlyGrant::from_approval`**: la aprobación tiene que estar ligada a:
  - el digest;
  - el recurso `adapter:<id>@<version>`;
  - el `code_hash` = `package_sha256`;
  - el actor;
  - el publicador;
  - los permisos exactos;
  - una `expires_at` futura;
  - los `secret_refs` concedidos de forma explícita.
- **`Policy::review_upgrade`**: devuelve `ACCEPTED` o `APPROVAL_REQUIRED` con el delta.
- **`GuardedWorker`**:
  - solo arranca con el manifiesto autorizado;
  - comprueba el `Access` antes de cada llamada;
  - no expone el `Worker` interno.

## Defectos y huecos encontrados

Las mutaciones encontraron cuatro casos que las primeras pruebas no cubrían. Los cuatro tienen ahora prueba:

1. **Raíces por prefijo de texto**: `models2` frente a `models`.
2. **Rutas con `..`** que, una vez normalizadas, caen dentro de una raíz.
3. **Origen declarado con usuario** (`https://user@huggingface.co`).
4. **Delta de permisos**: la prueba solo comprobaba red y secretos; ahora comprueba cada una de las cinco listas.

Además, una mutación no compilaba y se rehízo. Al escribir el código volvió a aparecer la trampa de los heredocs que colapsan las barras invertidas.

## Decisiones

- **TRUSTED_ONLY explícito**: AppContainer y las ACL por SID de paquete son de P130, así que toda ejecución es TRUSTED_ONLY y lo dice el modo de ejecución (`ExecutionMode::TrustedOnly`). La frontera es la comprobación del host, no el sistema operativo. La documentación lo deja claro.
- **`VERIFIED` se ejecuta sin aprobación**: la clave del publicador la fijó el operador. Contrato 03: «Oficial: firma verificada y key pinned local».
- **La caducidad de la concesión TRUSTED_ONLY** va en `permissions.expires_at` de la aprobación, porque el contrato 03 pide «expires_at» propio. La aprobación sigue siendo de un solo uso y de 900 s. Lo que se guarda y reutiliza es la concesión derivada.
- **Elevación indeterminada**: si no se puede saber si LARM está elevado, se trata como elevado (`elevation_unknown`).

## Pruebas

| Prueba del prompt | Prueba |
|---|---|
| Permission denied | `permiso_denegado_en_cada_operacion`: un adaptador firmado y real, con lecturas, escrituras, red, procesos y secretos fuera de permisos → `FORBIDDEN` sin llegar al worker, que sigue `READY` |
| Unsigned restrictions | `sin_firma_solo_con_concesion_trusted_only`: `UNSIGNED` y `CUSTOM` |
| Permission upgrade requires approval state | `elevacion_de_permisos_requiere_aprobacion` |

En `sin_firma_solo_con_concesion_trusted_only`:

- se rechazan siete aprobaciones incorrectas;
- con la concesión, el adaptador se ejecuta, pero sin el secreto declarado;
- con el secreto concedido de forma explícita, sí lo recibe;
- se rechazan la concesión caducada, la de otra versión y la de un paquete manipulado.

Además:

- `revocado_firma_invalida_y_elevado_bloquean`;
- `nivel_de_integridad_real_de_este_proceso`: `whoami` real devuelve integridad media, `S-1-16-8192`;
- tres pruebas unitarias del SDK: firma y manipulación, delta y comprobación de accesos.

**Comprobaciones**:

- `cargo fmt --check`, `clippy -D warnings` y `cargo test --workspace` en verde;
- arquitectura y lock de dependencias PASS;
- regresión de P021 PASS.

**Mutaciones**: 17. Todas hacen fallar las pruebas:

1. ignorar la elevación;
2. ignorar el hash del paquete;
3. tratar `UNTRUSTED` como `VERIFIED`;
4. dar secretos a `UNTRUSTED`;
5. concesión sin caducidad;
6. aprobación sin comprobar los permisos;
7. aprobar un secreto que el manifiesto no declara;
8. elevar permisos sin aprobación;
9. no comprobar la operación;
10. arrancar sin comprobar el digest;
11. raíz por prefijo de texto;
12. aceptar `..`;
13. origen con usuario;
14. no verificar la firma;
15. ignorar una clave revocada;
16. ignorar el publicador;
17. delta que solo mira la red.

## Bloqueado y pendiente

- **`P022-REAL-elevado-real`**: no se ha observado el rechazo con LARM elevado de verdad. Para observarlo, el operador tiene que ejecutar `cargo test -p larm-adapter-host --test policy` en una consola de administrador con `LARM_EXPECT_ELEVATED=1`.
- **`P022-REAL-appcontainer`**: la frontera del SO (AppContainer, ACL y Job Object con límites) es de P130.
- **Persistencia de `adapter_installation`, pantalla del delta, estado de firma en la GUI y `POST /approvals` para adaptadores**: los prompts de instalación de adaptadores y de la GUI.

## Compatibilidad

`Permissions` gana `Default`, que es aditivo. Ni el protocolo v1 ni el manifiesto cambian.
