# Seguridad de adaptadores (P022)

Antes de ejecutar un adaptador, LARM comprueba sus permisos y su confianza. Esto se aplica en dos sitios:

- en el SDK: `crates/larm-adapter-sdk/src/permissions.rs`;
- en el host: `crates/larm-adapter-host/src/policy.rs`.

Las referencias normativas son la spec (§12, §97, §131) y el contrato 03.

## Permisos concretos

El manifiesto declara cinco listas en `permissions`. El host las resuelve (`Grants::resolve`) y comprueba **cada operación** antes de enviarla al worker (`GuardedWorker::call`). Si una operación excede lo concedido, no llega al adaptador.

| Permiso | Cómo se comprueba | Motivo si se deniega |
|---|---|---|
| `read_roots` | Ruta real (canónica, sin `..` ni enlaces) dentro de una raíz de lectura o de escritura. La comparación es por componentes y sin distinguir mayúsculas. | `read_denied` |
| `write_roots` | Ruta real dentro de una raíz de escritura. Si el fichero aún no existe, se resuelve por su carpeta padre. | `write_denied` |
| `network_origins` | El origen exacto: esquema, host y puerto, con el puerto por defecto añadido. No se admiten subdominios, usuario en la URL (`user@`) ni otro esquema. | `network_denied` |
| `process_allowlist` | Ruta relativa exacta (en `/` y minúsculas). | `process_denied` |
| `secret_refs` | Referencia exacta de Credential Manager. Nunca viaja el valor. | `secret_denied` |

Todos los rechazos son `FORBIDDEN`.

Las raíces simbólicas (`$models`, `$runtime`…) las resuelve el host con su `RootMap`. Una raíz que el host no conoce no concede nada.

## Firma y confianza

**Firma**: Ed25519 sobre el RFC 8785 del manifiesto con `signature: null` (`signing_payload`). Las claves las fija el operador en el `TrustStore`, con `key_id`, publicador, clave pública, si es oficial y si está revocada. El `TrustStore` también guarda los paquetes revocados.

| Estado de firma (§131) | Cuándo | `trust` (persistencia) |
|---|---|---|
| `OFFICIAL` | Firma válida con una clave fijada como oficial y del mismo publicador | `VERIFIED` |
| `SIGNED_THIRD_PARTY` | Firma válida con una clave fijada de un tercero | `VERIFIED` |
| `UNSIGNED` | Sin firma | `UNTRUSTED` |
| `CUSTOM` | Adaptador local del operador, sin firma | `UNTRUSTED` |
| — | La clave o el `package_sha256` están revocados | `REVOKED` |

Dos casos no se instalan (`FORBIDDEN` `signature_invalid`):

- una firma que no verifica;
- una firma hecha con una clave desconocida o de otro publicador.

Un adaptador `REVOKED` se puede inspeccionar, pero no se ejecuta. Importar un manifiesto nunca hereda la confianza del fichero.

## Antes de lanzar (`Policy::authorize`)

1. **Sin privilegios de administrador**. Si LARM corre elevado, no lanza ningún adaptador: `FORBIDDEN` `elevated_process`.
   - Lo comprueba `whoami /groups` desde `System32`: el nivel de integridad alto es `S-1-16-12288` y el de sistema, `S-1-16-16384`.
   - Si no se puede determinar, se trata como elevado: `elevation_unknown`.
2. **Hash del paquete**. El hash de los ficheros en disco tiene que dar el `package_sha256` del manifiesto. Si no: `package_hash_mismatch`.
   - Se calcula como sha256 de la lista ordenada de `ruta\0sha256\n`.
   - Se excluye `manifest.json`, que contiene ese mismo hash.
   - Un paquete con enlaces simbólicos se rechaza.
3. **Confianza**:
   - `REVOKED`: se bloquea (`revoked`).
   - `VERIFIED`: se ejecuta con los permisos del manifiesto.
   - `UNTRUSTED`: hace falta una concesión TRUSTED_ONLY. Sin ella, `APPROVAL_REQUIRED` `untrusted_requires_approval`.
4. **Manifiesto autorizado**. `GuardedWorker::start` comprueba que el manifiesto es el autorizado; si no, `authorization_mismatch`.

## TRUSTED_ONLY

Todavía no hay AppContainer ni ACL por SID de paquete (llegan en P130). Por eso **toda ejecución es TRUSTED_ONLY**: la frontera es la comprobación del host en cada operación, no el sistema operativo. El Job Object actual termina el árbol asignado al cerrarse, pero no aísla el disco ni la red.

Para ejecutar un adaptador `UNTRUSTED`, el operador crea una aprobación (`POST /approvals`, scope `admin`) con estos campos:

| Campo | Valor |
|---|---|
| `operation_digest` | `execution_digest(manifest)`: sha256 del RFC 8785 de `{operation: "adapter.execute", adapter_id, version, package_sha256, publisher, permissions}` |
| `resource_ids` | `["adapter:<id>@<version>"]` |
| `code_hash` | `package_sha256` |
| `permissions` | `publisher`, `permissions` (copia exacta de la del manifiesto), `expires_at` (RFC 3339, en el futuro) y `secret_refs` (lo que se concede de verdad, por defecto nada) |

`TrustedOnlyGrant::from_approval` valida la aprobación. Quien llama la consume (un solo uso) y guarda la concesión.

La concesión deja de valer en cualquiera de estos casos:

- otra versión del adaptador;
- otro paquete o un fichero añadido;
- otro publicador;
- otros permisos;
- después de `expires_at`.

**Secretos**: un adaptador `UNTRUSTED` no recibe `secret_refs` por defecto, aunque el manifiesto los declare. Solo recibe los que la concesión nombra de forma explícita, y tienen que estar declarados en el manifiesto (si no, `approval_secret_not_declared`).

## Elevación de permisos al actualizar

`Policy::review_upgrade` compara los permisos ya aceptados con los del manifiesto nuevo (`PermissionDelta`):

- **Igual o menos**: `ACCEPTED`.
- **Algo nuevo**: `APPROVAL_REQUIRED`, con el delta exacto, el `operation_digest` (`elevation_digest`) y el `resource_id`.
  - La aprobación de ese digest, del mismo actor y con `code_hash` = `package_sha256` lo acepta.
  - La de otro delta, otro actor u otro paquete no vale.

## Límites conocidos

- **Límites de memoria/procesos del Job Object, AppContainer y ACL por SID de paquete**: P130.
- **El host comprueba lo que la operación declara (`Access`)**. Que el proceso del adaptador no toque nada más solo lo garantizará el sistema operativo (P130).
- **Persistencia de `adapter_installation`, la pantalla de delta y la UI del estado de firma**: los prompts de instalación de adaptadores y de la GUI.
- **Ejecución elevada real**: la comprobación con `whoami` se ha observado en el PC IA a integridad media (`S-1-16-8192`). El rechazo con LARM elevado se prueba con una sonda inyectada. Observarlo de verdad exige que el operador ejecute las pruebas como administrador con `LARM_EXPECT_ELEVATED=1`.
