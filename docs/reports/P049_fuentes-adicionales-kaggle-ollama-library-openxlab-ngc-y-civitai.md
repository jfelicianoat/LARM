# P049 — fuentes adicionales

Fecha: 2026-10-24  
Estado: `BLOCKED_REAL`

P049 añade cinco adapters source independientes con manifests generados por proveedor. Civitai implementa lectura pública de catálogo, detalle, versiones, manifest y estimación; los perfiles Kaggle, Ollama Library, OpenXLab y NGC declaran sus capacidades aún no cualificadas y fallan cerrado antes de red. No se activa scraping ni se interpreta `/api/tags` del runtime Ollama como catálogo web.

## Comportamiento

Civitai consulta el Site API v1. La búsqueda usa el límite documentado (máximo 100), `nsfw=false` y el cursor opaco de `metadata.nextCursor`, ligado a la búsqueda y al tamaño de página. Cada fuente conserva UUIDv7 estables locales para variante/revisión. El detalle expone licencia, base model, etiquetas y nivel NSFW solo cuando llegan en la respuesta oficial; cada fila de manifest conserva SHA-256 si existe.

La API publica `sizeKB`, que LARM muestra como estimación con límites en bytes y evidencia. No se devuelve tamaño exacto ni se rellena un dato ausente; si cualquier archivo no tiene un intervalo utilizable, el agregado queda `UNKNOWN`. El plan y la transferencia de Civitai están deshabilitados hasta que P045 resuelva credenciales de forma segura.

Kaggle tiene una CLI oficial para enumerar modelos, versiones y archivos, pero este perfil no anuncia esas acciones hasta integrar la referencia de credencial de P045 y cualificar el worker. Para Ollama Library, la documentación vigente describe `/api/tags` como recurso del runtime y no como catálogo Library. La búsqueda pública de OpenXLab y el contrato query de NGC tampoco se habilitan sin un contrato estable verificado. Los cuatro adapters devuelven `UNSUPPORTED_CAPABILITY`; no hacen IO ni scraping.

Referencias primarias: [Kaggle model CLI](https://github.com/Kaggle/kaggle-cli/blob/main/docs/models.md), [Ollama API](https://docs.ollama.com/api/introduction), [OpenXLab](https://openxlab.org.cn/docs/en/models/%E4%B8%8B%E8%BD%BD%E6%A8%A1%E5%9E%8B.html), [NGC Catalog](https://docs.nvidia.com/ngc/latest/ngc-catalog-user-guide.html), [Civitai models](https://github.com/civitai/civitai-developer-docs/blob/main/site/reference/models.md) y [Civitai model versions](https://github.com/civitai/civitai-developer-docs/blob/main/site/reference/model-versions.md).

## Verificación

Las pruebas focales en Windows pasan 6/6. Cubren schemas del adapter, cursores paginados ligados a consulta, identidad UUIDv7 repetible, atributos de licencia/NSFW, versiones, ruta hostil descartada, tamaños ESTIMATED y UNKNOWN, 401, 429 retryable, timeout, cancelación anterior al IO, perfil por proveedor y manifests de los cinco paquetes. Los cinco paquetes generados conservan orígenes de red separados; solo Civitai recibe `https://civitai.com:443`.

El smoke del paquete portable Windows pasa: su hash coincide, completa handshake JSON-RPC y devuelve el probe anónimo de metadatos. La consulta pública read-only desde el mismo worker también pasa en Windows y devuelve cinco modelos. El test contractual SDK de P052 pasa 5/5 para IDs opcionales, omisión compatible, UUIDv7 único y rango de tamaño validado. El gate integral (`tools/check.ps1 -Prompt P049 -Real`) pasa 14/14 controles; la suite serial del workspace pasa 1.105/1.105 tests, junto con formato, Clippy, arquitectura, `package_copy` (62/62), Tauri y lint/typecheck/tests/build del frontend. El recibo formal y el gate se enlazan en [evidencia P049](P049_fuentes-adicionales-kaggle-ollama-library-openxlab-ngc-y-civitai.evidence.json) y [check P049](check_P049_real.json).

P049 sigue en `BLOCKED_REAL` aunque Civitai superó la búsqueda read-only: P043, P045 y P025 no tienen `REAL_PASS`, y los perfiles Kaggle, Ollama Library, OpenXLab y NGC permanecen `UNSUPPORTED`. La transferencia Civitai y la integración de Kaggle con credential reference también esperan P045. La evidencia registra el resultado real de esta consulta por separado de los mocks.

## Contratos relacionados

La revisión P052 del paquete contractual 3.7.0 alinea el DTO Rust con `optional_artifact_ids` ya presente en el schema normativo, y documenta que los intervalos pertenecen a `ArtifactSizeEstimate` por artefacto. No cambia las firmas publicadas ni duplica intervalos en `DownloadPlanRequest`; se verificó con 5 tests del SDK y con `package_copy`.
