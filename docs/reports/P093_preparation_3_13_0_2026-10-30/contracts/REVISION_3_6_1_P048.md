# Revisión contractual 3.6.1 — registro del crate P048 en el grafo

Fecha: 2026-10-24  
Predecesora: 3.6.0  
Alcance: registra `larm-source-p048` y sus dependencias internas autorizadas en `crate-graph.json`.

P048 es un worker de familia source que depende de `larm-domain`, `larm-models` y `larm-adapter-sdk`. Esta revisión hace explícitas esas aristas para que P002 pueda comprobarlas; no altera el comportamiento de los adaptadores ni amplía sus permisos.

## Verificación documental

- El grafo incluye `larm-source-p048` con sus tres dependencias internas directas.
- La puerta de arquitectura valida el crate del worker junto al resto del workspace.
