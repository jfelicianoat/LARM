# Propuesta de revisión contractual P141: perfiles de autenticación privada

**Estado: propuesta para revisión; no normativa.** El avance P141 verificó que el contrato actual no basta para conectar los mecanismos privados de forma interoperable y segura.

## Hallazgos del contrato 3.8.0

- `source-profiles.json` exige `auth_flow_mapping`, pero no define su schema, revisión, campos ni cómo se liga a los orígenes permitidos del paquete del adaptador.
- `api-operations.json` declara start, poll, revoke y callback OAuth, pero el daemon no tiene handlers para esas rutas.
- `OAuthStart` sólo lleva fuente, scopes, flow y redirect URI. No hay un perfil tipado que proporcione client ID público, endpoints grant/token/revoke, método de autenticación de cliente, cabecera del token ni origen permitido.
- `OAuthFlow` obliga a devolver `user_code` y `verification_uri` incluso para PKCE; el contrato no aclara los valores de esos campos cuando el flujo es de navegador.
- La tabla `credential_reference` ya contempla `TOKEN`, `PASSWORD`, `PRIVATE_KEY`, `COOKIE` y `OAUTH_REFRESH`, pero no hay una operación contractual que vincule esos secretos de forma atómica a una cuenta existente. La ruta Basic sólo acepta usuario y contraseña.
- El contrato UI describe SSH agent/host key, cookie domain/path/expiry/secure y Git LFS OID/size, pero no tipa sus perfiles, metadatos protegidos ni fallos observables.

## Revisión propuesta

Crear revisión 3.9.0 de P141 que:

1. Defina `SourceAuthProfile` versionado dentro del perfil cualificado del adaptador. Sus branches de `TOKEN`/`BEARER`/`API_TOKEN` fijan el nombre y prefijo exactos de cabecera; `SSH` fija agente/clave y host-key fingerprints; `COOKIE` fija domain/path/expiry/secure; `GIT_LFS` fija el batch endpoint, transferencia, OID SHA256 y size; OAuth declara flows/endpoints/scopes. Cada origen de red debe coincidir con un `network_origin` exacto concedido al paquete instalado. Perfil ausente, incompleto o no cualificado produce `UNSUPPORTED_AUTH` antes de IO.
2. Defina endpoints OAuth por grant: device authorization/token/revocation para `DEVICE`; authorization/token/revocation, client ID público y callback loopback para `AUTH_CODE_PKCE`. Scopes solicitados deben ser subconjunto de los fijados por el perfil. Secretos de cliente no se admiten en el perfil.
3. Defina la respuesta de `OAuthFlow` por tipo: device entrega `user_code`, URI de verificación, expiración e intervalo; PKCE entrega `authorization_url` y un identificador de flujo, sin simular `user_code`.
4. Defina almacenamiento y transiciones: state aleatorio de 32 bytes y un solo uso; verifier PKCE de 32 bytes con S256; device_code, verifier, access token, refresh token, private keys y cookies exclusivamente en Credential Manager; callback sólo en loopback IPv4/IPv6, puerto efímero, path exacto y listener cerrado tras un resultado. SQLite conserva metadatos de flujo, owner, expiración, hash del state, intervalo y estado, nunca los valores secretos.
5. Añada una operación de escritura/rotación ligada a `{source_id, account_id}` y un tipo de secreto explícito, con actualización atómica de `CredentialReference` en la cuenta. El worker sólo obtiene el secreto después de revalidar cuenta, referencia, tipo, scopes y concesión exacta. Los metadatos de cookie quedan validados y limitados al origin permitido; no se extraen de otros programas.
6. Fije comportamiento verificable: `slow_down` suma cinco segundos; timeout aplica backoff; callback rechaza state inválido/repetido, error limpia el flujo y no intercambia tokens; un 401 permite una sola renovación; 403 gated no renueva; redirects no llevan credenciales a otro origin; Git LFS no trata pointers como modelos ni el OID como hash de contenido hasta verificarlo.

## Base normativa

- [RFC 7636, §4 y §7](https://www.rfc-editor.org/rfc/rfc7636.html): verifier aleatorio de al menos 256 bits y challenge S256 obligatorio cuando se dispone de él.
- [RFC 8628, §3.4](https://www.rfc-editor.org/rfc/rfc8628.html): polling respeta `interval`; `slow_down` incrementa el intervalo en cinco segundos; tras timeout se reduce la frecuencia.
- [RFC 8252, §7.3 y §8.3](https://www.rfc-editor.org/rfc/rfc8252.html): redirect de escritorio sólo en loopback IP, listener abierto durante la autorización y cerrado al recibir la respuesta.

## Pruebas requeridas para el cambio

Pruebas de schema/compatibilidad de perfil; host+SQLite+Credential Manager bajo fallos; callback state/PKCE válidos, inválidos, caducados y repetidos; backoff/`slow_down`; una sola renovación ante 401; 403 gated; cabecera token fijada; host key distinta; cookie fuera de domain/path/expiry; pointer Git LFS/OID/size; separación de origins/redirects; redacción; integración Windows con fixtures locales; y una fuente privada real autorizada con renovación/revocación antes de declarar `REAL_PASS`.
