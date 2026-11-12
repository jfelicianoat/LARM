# Revisión contractual 3.4.0 — transporte por bloques de Hugging Face (P046)

Fecha: 2026-10-23  
Predecesora: 3.3.0  
Alcance: declara `source.download_chunk` como método opcional soportado por el perfil Hugging Face.

El perfil `HUGGING_FACE` incluye ahora `source.download_chunk`, alineado con la firma y los DTO opcionales incorporados en la revisión 3.3.0 para P051. El worker P046 entrega bloques remotos acotados al Core; no escribe en el destino, no revela URLs firmadas ni credenciales y no activa por sí mismo una descarga.

La declaración solo anuncia la capacidad del adaptador. El host sigue exigiendo que el método figure en el manifiesto instalado y aplique confianza, permisos y autorización existentes. La revisión no habilita autenticación privada, no reduce los requisitos de licencia o checksum y no cambia las reglas de selección, estimación ni confirmación explícita de P052.

## Verificación documental

- El perfil HF declara el método que implementa el worker P046.
- La firma, los límites y la validación del bloque permanecen definidos por la revisión 3.3.0 y los schemas vigentes.
- Los demás perfiles de fuentes no cambian.
