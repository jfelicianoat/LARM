# Revisión de alineación contractual de P051/P052

Fecha: 2026-10-23  
Resultado: las capacidades de P052 ya están en el paquete LARM 3.2.0; no hace falta abrir otra revisión para añadirlas.

> Actualización posterior: esa conclusión se refería únicamente a duplicar los campos de selección y tamaños de P052. Para el plano de datos faltante de P051 sí se preparó y verificó la revisión 3.3.0, descrita al final de este documento.

## Hallazgos

La copia normativa local coincide con el paquete fuente 3.2.0: `python tools/package_copy.py check` verificó 56 archivos. La revisión P052 3.1.0 ya define `DownloadPlanRequest.optional_artifact_ids` como una lista de UUIDv7 opcional, única y limitada a 4096; omitirla equivale a `[]`. También fija `ArtifactSizeEstimate` por artefacto con confianza EXACT, HIGH, ESTIMATED (punto o intervalo) y UNKNOWN, límites decimales, procedencia, evidencia y motivo. La respuesta del preflight expone esas estimaciones y todos los destinos.

La implementación HTTP de `POST /api/v1/download-plans` coincide con ese contrato. El flujo de activación usa `ExecuteDownloadPlan {plan_id, plan_hash, operator_confirmed:true}`; el JSON Schema HTTP lo define así. El schema adapter genérico `ExecutePlan` conserva su `approval_id` para otras operaciones y no debe reutilizarse como DTO HTTP de confirmación de descargas.

Se encontró una carencia de implementación en `larm-adapter-sdk`: `SourceArtifact` no podía decodificar el campo `size_estimate` que el schema ya permite en `ArtifactManifest`. La SDK ahora acepta y valida esa estimación allí sin elevar datos antiguos, y rechaza que un tamaño exacto de manifiesto contradiga `size_bytes`; `ModelInspection` sigue rechazando ese campo porque su schema no lo incluye. Se añadió una prueba de intervalo por artefacto, de incoherencia exacta y de rechazo fuera de contexto. No se añadió selección por ID al protocolo upstream: esos UUID son identidades de artefactos de LARM; el Core mantiene la selección y debe traducirla al plan físico del bundle antes de invocar una fuente.

## Decisión

No proponer 3.3.0 para duplicar campos ya incorporados en 3.1.0. Mantener la revisión normativa 3.2.0 y corregir la deriva de implementación cubierta por la SDK. Si se decide que una fuente externa debe recibir una selección, deberá definirse aparte un campo estable en términos que la fuente conozca (por ejemplo, rutas de manifiesto fijadas), sin exponer IDs locales como identidad upstream.

## Verificación

- `python tools/package_copy.py check` — PASS, 56 copias idénticas al paquete 3.2.0.
- `cargo test -p larm-adapter-sdk --test source_protocol manifiesto_publica_intervalos_por_artefacto_y_no_confunde_exactitud -j 1 -- --exact --test-threads=1` — PASS.
- El contrato base sigue siendo cerrado: sin campos adicionales no declarados, y unknown continúa representándose como null con evidencia, nunca como cero.

## Revisión posterior — 3.3.0 para P051

La revisión 3.3.0 no vuelve a declarar los campos P052. Añade `source.download_chunk` como método opcional, con solicitud ligada a fuente/job/plan/hash/revisión/ruta/offset/aprobación y respuesta en bloques base64 acotados. El Core conserva URL y credenciales dentro del worker autorizado; el SDK valida offset, tamaño, base64 canónico, EOF/total y validador. El staging persiste el offset durable acompañado del validador y reinicia el artefacto cuando cambia.

`tools/verify_pack.py` pasó 3.099 checks, el paquete se sincronizó y las 57 copias pasaron `package_copy check`. La suite diferencial se amplió a 40 firmas, 80 direcciones y 8.903 casos. La integración del host con staging pasa con un proceso de adaptador real; falta conectarla al consumidor de jobs y un repositorio de producción para el checkpoint conjunto offset/validador. Por tanto, la revisión resuelve el contrato y la frontera de bytes, pero P051 sigue `BLOCKED_REAL`.
