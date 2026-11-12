# Revisión de contrato LARM 3.10.0 — mirrors cualificados P052

Fecha: 2026-10-25  
Base: paquete 3.9.1  
Estado: revisión preparada para integrar planificación y ejecución multi-fuente.

El contrato P052 ya exigía mirrors con hash y tamaño compartidos y prohibía mezclar segmentos, pero no definía el conjunto de alternativas que el operador debía revisar ni los datos por fuente necesarios para revalidar el plan y guardar procedencia.

Se añaden los schemas cerrados `DownloadMirrorArtifact` y `DownloadSourceCandidate`, y el campo obligatorio `DownloadPlanResponse.source_candidates` (1–16 candidatos). Cada candidato incluye identidad de fuente/variante/revisión, hash del manifiesto, prioridad, confianza, autenticación, HTTPS y el mapeo de IDs para cada artefacto seleccionado. `source_id` se define como candidato preferido; en `DownloadPlanRequest` es un selector de preferencia y debe pertenecer al conjunto elegible.

El orden se fija por autenticación disponible, confianza EXACT>HIGH>ESTIMATED>UNKNOWN, HTTPS, prioridad descendente y `source_id` ascendente. La elegibilidad requiere fuente y binding habilitados, cualificación P026 actual, capacidad de descarga por bloques y autorización vigente para cuenta/licencia. La equivalencia de un mirror exige cobertura íntegra de los artefactos seleccionados con ruta, SHA-256 y tamaño exacto iguales; no basta la identidad canónica del modelo.

El plan hash incluye la lista ordenada y cada mapeo. La UI muestra los candidatos y pide confirmación explícita. La activación revalida todos los candidatos. El worker usa una fuente por fichero, y sólo cae a la siguiente ante timeout, error de I/O, HTTP 408, 429 o 5xx; elimina su temporal y reinicia ese fichero desde cero. Errores de autorización, licencia, permisos, protocolo, revisión o integridad son terminales y no habilitan failover.

Se añade el evento durable `download.mirror.failed`, sin URL ni secretos. La completación valida por artefacto que source_id y revision_key pertenecen al plan y guarda la procedencia real del mirror usado. Los planes preexistentes que no contienen `source_candidates` conservan únicamente su fuente original, sin inferencias.

Esta revisión define el contrato, no declara cualificada ninguna fuente ni satisface una prueba real. P052 continúa sujeto a `BLOCKED_REAL` mientras falten sus dependencias o pruebas reales.
