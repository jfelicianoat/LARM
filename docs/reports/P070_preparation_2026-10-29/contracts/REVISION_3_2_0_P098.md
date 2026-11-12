# Revisión de contrato LARM 3.2.0 — P098

Fecha: 2026-10-23  
Base: paquete 3.1.0  
Alcance: vista previa read-only de importación local requerida por P123.

## Operación

`POST /api/v1/models/import/preview` usa scope `models:write`, recibe únicamente `ImportPreviewRequest {source_path}` y responde síncronamente con `ImportScanPreview` (200). No requiere `Idempotency-Key`: la operación no crea ni modifica estado. Cada consulta recorre de nuevo el origen; una respuesta anterior no autoriza ni fija el resultado de una importación posterior.

La operación reutiliza los límites y el recorrido seguro de P017/P098: como máximo 100.000 archivos, cabecera limitada a 64 KiB por fichero, orden determinista y sin seguir enlaces simbólicos ni junctions. No ejecuta ni deserializa contenido de modelo. Los errores de lectura usan el envelope común de API; una ruta vacía o cuerpo con campos desconocidos se rechaza antes de recorrer.

## Respuesta

`ImportScanPreview` muestra variantes y archivos reconocidos, rol, obligatoriedad, facts obtenidos de cabeceras acotadas y tamaño exacto de cada fichero legible. Los tamaños se representan como cadenas decimales acotadas a `i64::MAX`. `bytes_seen` suma los archivos regulares legibles, incluidos los ignorados; `recognized_bytes` suma una sola vez cada ruta reconocida aunque comparta documentación entre variantes. Ningún acumulado puede desbordarse ni superar el máximo decimal contractual.

Los campos `relative_path` son siempre relativos al directorio solicitado; la respuesta no incluye ni repite la ruta absoluta. `ignored` lista archivos legibles sin formato/modelo reconocido. `skipped` lista enlaces, junctions y archivos ilegibles con motivo; los tamaños de esos elementos no se añaden a `bytes_seen` porque el walker no pudo medirlos de forma segura.

## Frontera de confirmación

El preview no calcula hashes, crea bundles, escribe manifiestos, registra filas, copia datos, reserva almacenamiento ni inicia un job. La interfaz muestra la respuesta junto al destino y modo elegidos. Si el origen cambia, descarta el preview; antes de iniciar `POST /api/v1/models/import`, solicita una confirmación explícita sobre los parámetros visibles. El endpoint de importación vuelve a recorrer y validar los ficheros; el preview no es un snapshot ni una autorización.

## Archivos normativos

- `contracts/api-operations.json`
- `schemas/larm.schema.json`
- `contracts/00_PACKAGE.md`
- `manifest.json`
