# Revisión de contrato LARM 3.1.0 — P052

Fecha: 2026-10-22  
Base: paquete 3.0.0  
Estado: incorporada al paquete contractual.

Resuelve dos carencias para el preview local: selección explícita de artefactos opcionales y tamaño con confianza, cotas, procedencia y evidencia por artefacto. Permite preflight sin destino para comparar alternativas, liga la selección y el destino al hash inmutable y exige confirmación explícita antes de crear el job. Las filas históricas se migran a UNKNOWN y no se reinterpretan.

Archivos normativos actualizados: `02_STORAGE_DOWNLOAD.md`, `07_API_EVENTS_MCP.md`, `api-operations.json`, `persistence.sql`, `schemas/larm.schema.json`, `schemas/domain.schema.json`, el prompt P052 y la migración 0006. Se consolidaron además las cuatro extensiones locales de P031/P099 ya usadas por el proyecto, antes de volver a sincronizar sus copias.
