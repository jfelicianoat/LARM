# Revisión contractual 3.5.0 — transporte por bloques de ModelScope (P047)

Fecha: 2026-10-23  
Predecesora: 3.4.0  
Alcance: declara `source.download_chunk` como método soportado por el perfil ModelScope y fija la referencia del SDK oficial en `modelscope_hub` v0.4.0.

El perfil `MODELSCOPE` incluye ahora `source.download_chunk`, alineado con la firma, DTO, límites y validaciones definidos por la revisión 3.3.0 para P051. El worker devuelve bloques binarios acotados al Core; el adaptador no escribe en el destino, no expone URLs firmadas ni credenciales y no inicia por sí mismo una descarga.

La declaración solo anuncia la capacidad del adaptador. El host sigue exigiendo que el método figure en el manifiesto instalado y aplique confianza, permisos y autorización existentes. Esta revisión no habilita autenticación privada ni gated, no reduce requisitos de licencia o checksum y no cambia selección, estimación, preflight o confirmación explícita de P052.

## Verificación documental

- El perfil `MODELSCOPE` declara el método implementado por P047.
- Firma, límites y validación del bloque permanecen definidos en la revisión 3.3.0 y los schemas vigentes.
- La referencia del SDK queda fijada en el tag oficial `modelscope_hub` v0.4.0.
- Los demás perfiles no cambian.
