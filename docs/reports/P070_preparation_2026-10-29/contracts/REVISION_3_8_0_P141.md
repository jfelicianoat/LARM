# Revisión de contrato LARM 3.8.0 — vínculo de cuenta para P141

Fecha: 2026-10-24  
Base: paquete 3.7.0  
Estado: revisión aprobada por el operador; materializada en el paquete.

El `AuthProbe` existente transportaba `source_id`, `auth_mode` y `credential_reference_id`, pero el worker no podía seleccionar la entrada de Credential Manager: el modelo vincula el usuario de Credential Manager al `account_id`, no al UUID de la referencia. Se añade `account_id` como campo nullable obligatorio al DTO cerrado `AuthProbe`, manteniendo los otros campos y sus significados.

Para `auth_mode=NONE`, `account_id` y `credential_reference_id` deben ser null. Para un mecanismo con credencial, el host vuelve a resolver en SQLite la fuente, la cuenta, su referencia y el registro de credencial. Exige que la cuenta pertenezca a la fuente, que coincidan ambos IDs, que `service=larm/source/{source_id}`, que `username=account_id` y que el tipo del secreto corresponda al mecanismo. Sólo entonces inicia el worker y declara `service` en `Access.secrets`; el manifiesto de la instalación y la concesión deben declarar exactamente esa referencia. El adaptador resuelve el contenido en Credential Manager dentro del proceso worker. El request sólo contiene IDs, nunca el valor secreto.

El cambio no altera CredentialReference, la tabla SQLite ni el formato del secreto. No modifica los flows OAuth, el refresh/revoke, SSH, cookies, Git LFS o la política de UNC. Los adaptadores que no declaren el service exacto, la fuente sin cuenta seleccionada, un vínculo stale o un tipo de secreto incompatible fallan cerrados antes de IO. El grafo contractual permite `keyring` como dependencia directa de P048 para leer la entrada en el proceso worker; el paquete HTTP declara sólo `larm/source/{source_id}` y el host lo concede únicamente para un probe Basic vinculado y validado.

Compatibilidad: es un cambio aditivo en un DTO de request con schema cerrado; los workers source de la revisión anterior rechazarán el campo extra, por lo que la conformidad P141 requiere la nueva revisión del SDK/schema y la cualificación correspondiente.
