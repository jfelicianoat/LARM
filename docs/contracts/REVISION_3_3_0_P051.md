# Revisión contractual 3.3.0 — flujo de bloques de descarga P051

Fecha: 2026-10-23  
Predecesora: 3.2.0  
Alcance: añade una firma source opcional y sus DTO al protocolo de adaptadores; no cambia los cuerpos HTTP P052 ni las obligaciones de adaptadores existentes.

`source.download` conserva su firma y respuesta `JobReceipt`. Se añade `source.download_chunk`, opcional (`required:false`), para que el adaptador obtenga un bloque remoto dentro de su propio proceso y entregue los bytes al Core sin revelar URLs firmadas, cabeceras o credenciales. La solicitud fija source, job, plan/hash, aprobación nullable, revisión, ruta relativa, offset, máximo de bytes y validador de reanudación. La respuesta contiene offset, base64, EOF, total nullable y validador nullable.

Un bloque contiene como máximo 524288 bytes; su representación base64 queda por debajo del frame NDJSON de 1 MiB. La ruta debe ser relativa y normalizada. Los identificadores/hash/offset, canonical base64, tamaño de respuesta, sumas comprobadas y semántica EOF/total se validan antes de escribir. La respuesta puede revelar un validador distinto para que el executor descarte el temporal y reinicie desde cero; nunca se anexan esos bytes a un offset anterior.

El método solo se invoca si aparece en el manifiesto del adaptador y su paquete supera confianza, permisos y autorización existentes. Su presencia no concede autenticación, licencia, aprobación, espacio, checksum o acceso al storage. Sin método, los adaptadores mantienen compatibilidad, pero no son elegibles para descargas locales ejecutadas por Core.

## Verificación documental

- `source.download` permanece intacto.
- DTOs estrictos con `additionalProperties:false`; nullable requerido, tamaño de bytes decimal y cota de bloque comprobada en SDK.
- Pruebas esperadas: schemas independientes, respuestas malformadas/cambiantes, frame menor de 1 MiB, host protegido, offset/checkpoint/reanudación e integración con staging P051.
