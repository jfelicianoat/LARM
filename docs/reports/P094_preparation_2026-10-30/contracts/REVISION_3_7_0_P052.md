# Revisión de contrato LARM 3.7.0 — conformidad P052 del SDK source

Fecha: 2026-10-24  
Base: paquete 3.6.1  
Estado: incorporada al paquete contractual.

La revisión detectó una diferencia entre el schema normativo cerrado y el DTO tipado de `larm-adapter-sdk`: `DownloadPlanRequest` ya admitía `optional_artifact_ids` en el schema 3.1.0, pero el SDK Rust lo rechazaba como campo desconocido. El DTO conserva ahora el miembro opcional y su presencia en la serialización; los IDs se validan como UUIDv7 únicos y con el límite contractual de 4096.

Los intervalos de tamaño por artefacto ya están definidos por `ArtifactSizeEstimate` y se transportan en `ArtifactManifest.artifacts[].size_estimate`. `DownloadPlanRequest` no lleva tamaños, por lo que no se cambia el schema ni se duplican datos. Un adaptador que no pueda resolver IDs del catálogo central contra su manifiesto fijado devuelve `UNSUPPORTED_CAPABILITY` antes de producir un plan, en vez de ignorar la selección.

El schema `larm.schema.json`, la firma publicada de la operación y las demás reglas de P052 permanecen sin cambios. Esta revisión alinea su materialización tipada y documenta el comportamiento cerrado.
