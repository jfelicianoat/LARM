# P049 — perfiles de fuentes adicionales

Cada fuente se genera como un worker y un manifiesto independientes: `larm.source.kaggle`, `larm.source.ollama-library`, `larm.source.openxlab`, `larm.source.ngc` y `larm.source.civitai`. La carpeta de cada proveedor incluye su entrada de adapter, `provider.json`, `capabilities.json` y la referencia oficial revisada. Las entradas comparten el protocolo y la validación contractual de LARM.

## Capacidades actuales

| Fuente | Soporte anunciado | Límite del perfil |
| --- | --- | --- |
| Kaggle | Ninguno | La CLI oficial lista modelos, versiones y ficheros, pero este adapter aún no resuelve `credential_reference_id` mediante P045 ni anuncia búsqueda o planes. No lee variables de entorno ni almacena tokens. |
| Ollama Library | Ninguno | El endpoint `/api/tags` pertenece al runtime local de Ollama; el adapter no lo confunde con el catálogo web y no raspa la página Library. |
| OpenXLab | Ninguno | La documentación revisada permite navegar y descargar repositorios, pero no publica aquí un contrato estable de búsqueda de catálogo para el adapter. No raspa la web. |
| NGC | Ninguno | El catálogo dispone de UI/CLI y puede exigir autenticación o entitlement. El adapter no infiere compatibilidad AMD ni ejecuta consultas hasta cualificar el contrato y los permisos. |
| Civitai | `source.search`, `source.get_model`, `source.list_revisions`, `source.get_manifest`, `source.estimate_size` | Usa solo API Site v1 pública, solicita `nsfw=false` y sigue `metadata.nextCursor`. Las restricciones regionales pueden devolver solo contenido SFW. Licencia, nivel NSFW, versión y SHA-256 se conservan cuando la API los expone. |

Las fichas oficiales consultadas son [Kaggle model CLI](https://github.com/Kaggle/kaggle-cli/blob/main/docs/models.md), [Ollama API](https://docs.ollama.com/api/introduction), [OpenXLab descarga de modelos](https://openxlab.org.cn/docs/en/models/%E4%B8%8B%E8%BD%BD%E6%A8%A1%E5%9E%8B.html), [NGC Catalog User Guide](https://docs.nvidia.com/ngc/latest/ngc-catalog-user-guide.html) y [Civitai Site API — Models](https://github.com/civitai/civitai-developer-docs/blob/main/site/reference/models.md). Civitai documenta `GET /models`, `GET /models/{id}`, límites de página y cursores; su referencia de [model versions](https://github.com/civitai/civitai-developer-docs/blob/main/site/reference/model-versions.md) describe `files[]`, `sizeKB`, `primary` y `hashes.SHA256`.

## Tamaños y transferencia

Civitai expone `sizeKB`, no un conteo exacto de bytes. LARM lo transforma en una estimación por intervalo con evidencia, conserva `UNKNOWN` cuando falta o no es válido, y nunca lo promueve a exacto. La estimación agregada queda desconocida si algún fichero carece de intervalo. El detalle del modelo registra `size_bytes: null`; el manifiesto añade la estimación tipada.

`source.download_plan`, `source.download` y `source.download_chunk` no inician transferencia desde Civitai. La autenticación de proveedor y su resolución segura dependen de P045; la descarga se habilitará después de integrar el credential reference y ejecutar pruebas reales. Los otros cuatro perfiles devuelven `UNSUPPORTED_CAPABILITY` antes de IO de red. Las credenciales no aparecen en URLs, logs, fixtures ni el paquete generado.

## Paquete y pruebas

`adapters/source/p049/package.py` genera workers con Python portable fijado al lock, SDK, contratos, manifests hash y un perfil UUIDv7 por fuente. Civitai permite únicamente `https://civitai.com:443`; los perfiles aún no cualificados no reciben permiso de red. `tests/integration/p049_adapters.py` valida respuestas contra schemas y cubre cursor/query, identidad estable, path traversal, licencia/NSFW, versiones, intervalos, UNKNOWN, 401, 429, timeout, cancelación y aislamiento de los cinco manifests. El smoke del worker comprueba el hash empaquetado, handshake y probe anónimo.

La búsqueda real read-only desde el worker Windows empaquetado devolvió cinco modelos el 2026-10-24. La evidencia aún mantiene P049 en `BLOCKED_REAL`: los otros cuatro perfiles no están cualificados y P043, P045 y P025 siguen sin `REAL_PASS`. La búsqueda observada no habilita por sí sola descarga, licencias ni credenciales.
