# Revisión de contrato LARM 3.10.1 — fuente principal y mirrors alternativos P052

Fecha: 2026-10-25. Ámbito: `DownloadPlanResponse`, `DownloadPlanRequest` y ejecución de P052. Esta revisión corrige el significado de `source_candidates` definido en 3.10.0.

`DownloadPlanResponse.source_candidates` es una lista de alternativas de mirror, no una lista que incluya la fuente principal. Admite de cero a 16 elementos. `DownloadPlanResponse.source_id` sigue siendo la fuente principal asociada a la distribución de la variante seleccionada. Si no hay mirror exacto, la lista es `[]` y la fuente principal continúa disponible para preview, activación y transferencia. Esto conserva descargas cuya estimación de tamaño es ESTIMATED o UNKNOWN.

Un mirror debe mapear todos los artefactos seleccionados con ruta normalizada, SHA-256 y tamaño exacto iguales a los del objetivo, y superar los gates vigentes de revisión, autorización, conformance, permisos y transporte. Los IDs fuente pueden diferir. No se incluye un mirror si faltan datos exactos, ni se relajan permisos o gates para permitir fallback.

`DownloadPlanRequest.source_id`, si aparece, debe coincidir con la fuente principal. No selecciona una alternativa. El hash de planes nuevos incluye explícitamente la lista, incluso cuando está vacía. Al activar, se comparan los mirrors fijados con los elegibles actuales. Para compatibilidad, se acepta el hash histórico sin la propiedad `source_candidates` sólo al consumir registros antiguos cuyo candidato está ausente o vacío.

Cada artefacto empieza por la fuente principal y puede cambiar secuencialmente a mirrors aprobados ante timeout, error de I/O o HTTP 408/429/5xx. El worker elimina el staging parcial y reinicia desde offset cero. Auth, licencia, permiso, protocolo, revisión o integridad no son reintentables. La transición de fallback guarda en una única transacción el progreso reiniciado, `download.mirror.failed` y el outbox. La finalización coteja cada source_id y revision_key con el candidato autorizado y persiste la procedencia efectiva.

## Verificación

- `DownloadPlanResponse.source_candidates` permite `[]` y conserva `maxItems: 16`.
- El plan nuevo con lista vacía incluye el campo en su hash; los registros históricos vacíos pueden validar el hash anterior.
- Una estimación UNKNOWN o ESTIMATED no elimina la fuente principal.
- Un mirror sin mapeo exacto de cualquiera de los artefactos seleccionados se excluye.
- Ningún fallo no transitorio se convierte en fallback.
