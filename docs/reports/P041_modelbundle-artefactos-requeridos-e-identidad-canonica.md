# P041 — ModelBundle, artefactos requeridos e identidad canónica

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas pasan, incluida la real (7 pasos más la regresión de P040), pero su dependencia P040 no tiene REAL_PASS (le faltan P003 y P004). [evidence.json](P041_modelbundle-artefactos-requeridos-e-identidad-canonica.evidence.json) · [operación](../operations/models.md)

## Qué se construyó

- **Dominio** (`larm-domain/src/model.rs`):
  - `ModelVariant`, `ModelArtifact` y `ModelBundle` según el schema;
  - roles de artefacto: weights, tokenizer, processor, mmproj, VAE, text_encoder, vocoder, config, external_data y other;
  - hash canónico del bundle (RFC 8785) sobre `{relative_path, sha256, size_bytes, required}`, con rutas POSIX y sin duplicados sin distinguir mayúsculas;
  - hash de identidad de la variante calculado solo con los requeridos;
  - estado VERIFIED, INCOMPLETE o CORRUPT;
  - roles necesarios según formato y capacidades.
- **`IdentityResolver`** (`larm-models/src/identity.rs`):
  - grados `IDENTICAL`, `PROBABLE_SAME_MODEL`, `DERIVED_MODEL`, `UNRELATED` y `UNKNOWN`, con sus motivos;
  - solo `IDENTICAL` permite fusión automática;
  - el nombre por sí solo nunca pasa de `UNKNOWN`.
- **Bundle** (`larm-models/src/bundle.rs`): construcción determinista y verificación de los bytes en disco, guardada con CAS y su evidencia.
- **Registro**: resuelve la identidad frente a los candidatos (homónimos, bytes compartidos y evidencia declarada) y guarda resoluciones, evidencia y linaje. Registra cada variante con sus artefactos y su bundle en una transacción y es idempotente.
- **SQLite** (`larm-db/src/variants.rs`): variantes, artefactos y bundles.
- **API de solo lectura**: `/variants`, `/models/{id}/variants`, `/model-bundles` y `/model-artifacts`, con su detalle. `openapi.json` queda con 42 operaciones implementadas.
- **Daemon**: comando `bundle-verify`.

## Defectos encontrados por la prueba real

1. **Un modelo probable no se comparaba nunca.** Con otro nombre y sin bytes comunes, aunque compartiera arquitectura, autor y tokenizador, no se le buscaba como candidato. Los candidatos ahora también se buscan por la evidencia guardada. Tiene prueba de regresión.
2. **Un candidato sin evidencia se anotaba como duplicado.** Al ampliar los candidatos, un `UNKNOWN` sin más evidencia acababa en `possible_duplicates`. Ahora solo se anota un `PROBABLE` o una coincidencia de nombre. Tiene su propia prueba.

## Decisiones

- **Dos hashes**: el `manifest_hash` del bundle cubre todos los artefactos; el `bundle_hash` de la variante, solo los requeridos. Así se cumplen las dos frases del contrato 01: el bundle hash sobre la lista con `required`, y la identidad de la variante sobre los descriptores requeridos.
- **El rol va en `metadata.role`**: el schema cerrado de `model_artifact` no tiene columna para él.
- **Completitud según la variante**: si falta un rol que el formato necesita (por ejemplo, el mmproj de un GGUF de visión), el bundle queda INCOMPLETE aunque estén todos sus ficheros. Con un formato desconocido no se exige nada.
- **Sin sha256 o tamaño de un artefacto no hay bundle**: no se inventan.

## Pruebas

| Prueba del prompt | Dónde |
|---|---|
| Identical hashes | `hashes_identicos_agrupan` y real (otro nombre y otra fuente: un modelo) |
| Same name different model | `mismo_nombre_modelo_distinto` (UNRELATED, sin duplicado) y unitaria |
| Derived quantization | `cuantizacion_derivada_conserva_linaje` y real (GGUF con hashes padre) |
| Probable no auto-merge | `probable_no_se_fusiona`, `probable_por_evidencia…` y real |

**Negativos**:

- rutas con `\`, `..`, absolutas o duplicadas sin distinguir mayúsculas, sin escribir nada;
- sha256 o tamaño inválidos;
- bundle sin hashes (`bundle_missing`);
- manifiesto que ya no corresponde (`manifest_mismatch`);
- filtros y estados no previstos: 400;
- modelo o variante desconocidos: 404.

**Mutaciones**: cada una se aplicó y se restauró el código. Las 9 hacen fallar al menos una prueba:

1. `PROBABLE` fusiona;
2. identidad calculada con los opcionales;
3. un shard ausente da VERIFIED;
4. sin exigir los roles necesarios;
5. sin linaje;
6. sin candidatos por evidencia;
7. un `UNKNOWN` como duplicado;
8. `IDENTICAL` sin comparar configuración;
9. duplicados distinguiendo mayúsculas.

## Prueba real (`tools/p041_real.py`, 7 pasos PASS)

Se ejecutó con el daemon en el PC IA, sobre ficheros reales hasheados con hashlib (oráculo independiente).

1. **Identidad**: se importan seis modelos y quedan cinco.
   - Qwen-X y su espejo con los mismos bytes: `IDENTICAL`, un modelo.
   - La cuantización GGUF: `DERIVED_MODEL` con linaje.
   - El Instruct: `PROBABLE_SAME_MODEL`, con duplicado mutuo.
   - Nova: aparte.
2. **`bundle-verify`** sobre un GGUF multimodal (2 shards, mmproj y README opcional):
   - completo: VERIFIED;
   - sin un shard: INCOMPLETE;
   - con un byte del mmproj cambiado: CORRUPT;
   - restaurado y sin el README opcional: VERIFIED (CAS v5).
3. **GGUF de visión sin mmproj**: INCOMPLETE por rol faltante.
4. **API**: listas, variantes del modelo agrupado, bundles por estado, artefactos por nombre, 404 y 400.

## Pendiente

- **Conectar la verificación del bundle con el ciclo de vida** (`DOWNLOADED → VERIFIED`): es de las descargas (P020+).
- **Pantalla de modelos**.

Avance P044/P041 de ingesta 2026-10-27: preparador P041 compartido y writer atómico conectados; duplicados recíprocos incluidos en el commit y evidencia de hashes requeridos incompletos/históricos rechazada para auto-merge. 117 pruebas focales y nativo original P022/P026 1/1 (178,60 s) con 99 casos/21 observaciones. Fuente creada deshabilitada por Core, perfil fixture ligado a ese UUID antes de cualificar, habilitación posterior y seis modelos/variantes/artefactos/snapshots desde RPC público, sin seed SQL de modelos ni estado físico ficticio. 2.014 inputs/ejecutables coinciden. La preparación de perfiles es todavía explícita en el harness; falta componer búsqueda HTTP y primera adquisición Core/GUI. [Informe](P044_P041_ingestion_2026-10-27.md).

Avance P044/P041/P043 (2026-10-28), `BLOCKED_REAL`: el daemon compone el proveedor cualificado de búsqueda con RPC público e ingesta P041 atómica; huellas de fuente/cualificación invalidan cachés y cursores, y la transacción rechaza observaciones de una configuración antigua. Conserva certezas/intervalos e IDs centrales sin inventar elegibilidad. Pasan 170 pruebas Rust focales únicas y nativo HTTP autenticado dentro del proceso 1/1 en 285,52 s: P026 original 99 casos/21 observaciones, tres páginas de seis modelos, replay sin escrituras, revocación física 409 y fuente deshabilitada 503, sin bundles/descargas ni nuevas peticiones HTTP /artifact/ durante búsqueda. Coinciden 2.016 inputs/ejecutables; Clippy/formato/arquitectura/OpenAPI y 78 copias PASS. El ensayo usa timeout300s/caché600s; primera consulta169,891s. El default productivo sigue30s y su rendimiento/cancelación nativa NO están validados. Pendientes optimización, perfiles de todas las fuentes, autenticación de cuenta, primera adquisición Core completa, GUI y restantes fuentes/runtimes; el objetivo global sigue activo. [Informe y recibos](P044_qualified_search_2026-10-28.md).
