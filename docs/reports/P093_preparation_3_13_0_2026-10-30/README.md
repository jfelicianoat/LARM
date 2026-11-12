# Contexto de P093

Leer [P093](P093.md), [especificación íntegra](LARM_SPEC_V1.md) y [contrato compartido](SHARED_CONTRACT.md). Los contratos, schemas, fixtures y snapshots adjuntos son entradas obligatorias.

Dependencias: P016, P040, P051.

El selector verifica los hashes del paquete fuente y copia documentos; no evalúa resultados de implementación. Los reports trasladados deben revisarse y las dependencias deben estar REAL_PASS. Sin reports se incluye DEPENDENCIES_NOT_VERIFIED.md.

manifest.json conserva la identidad del paquete completo; CONTEXT_MANIFEST.json registra sólo los archivos de este contexto. No ejecutar la verificación del paquete completo contra esta selección.
