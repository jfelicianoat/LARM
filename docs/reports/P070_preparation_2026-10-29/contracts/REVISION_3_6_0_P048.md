# Revisión contractual 3.6.0 — transporte por bloques para fuentes P048

Fecha: 2026-10-23  
Predecesora: 3.5.0  
Alcance: declara `source.download_chunk` en los perfiles GitHub Releases, HTTP directo, LOCAL y UNC_PRIVATE que P048 implementa. Git LFS conserva su estado anterior y no anuncia bloques en esta revisión.

Los cuatro perfiles P048 añaden `source.download_chunk` según la firma, DTO, límites y validaciones fijadas por la revisión 3.3.0 para P051. Los workers entregan bloques acotados al Core/Download Manager; no escriben el destino, no aceptan licencias, no revelan credenciales ni crean por sí mismos trabajos de descarga.

La declaración solo anuncia la capacidad. El host sigue exigiendo que el método figure en el manifiesto instalado y aplique confianza, permisos y autorización. Las credenciales privadas siguen sujetas a P045/P141; las transferencias a orígenes HTTP de terceros y los recursos UNC privados requieren cualificación explícita. Esta revisión no cambia selección de artefactos, estimación, preflight ni confirmación de P052.

## Verificación documental

- GitHub, HTTP, LOCAL y UNC_PRIVATE anuncian el método que implementan P048.
- Firma, límites y validación del bloque permanecen definidos por la revisión 3.3.0 y los schemas vigentes.
- Git LFS no cambia y no se considera habilitado por esta revisión.
- Los demás perfiles no cambian.
