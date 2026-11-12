# P092 — compositor interno de recomendaciones

Fecha: 2026-10-30. Preparación independiente; aceptación real integral pendiente. El objetivo global de LARM permanece activo.

El estado actual de «Descubrir» sigue provisional. P121 depende de P044/P092/P120, y todavía faltaba el compositor de candidatos de P092. Se implementa esa dependencia sin habilitar una recomendación en la interfaz ni alterar el contrato HTTP.

## Implementación

`crates/larm-core/src/recommendation.rs` recibe entidades del dominio existentes: CanonicalModel, Distribution, Revision, Variant, RuntimeVersion, Installation, Backend y StorageLocation, junto con compatibilidad y entradas de TechnicalFit. Reutiliza los validadores existentes de modelo/distribución/revisión/variante/runtime/instalación/backend, comprueba identidades y relaciones, hardware/perfil y clase de almacenamiento, y rechaza tuplas repetidas. Los registros revocados, corruptos, retirados, con runtime roto, backend no evidenciado, volumen desconocido o ubicación inaccesible no reciben score técnico. Una ubicación de sólo lectura puede servir pesos para inferencia; no se confunde con permiso de colocación o escritura.

Se reutiliza P091 con los pesos efectivos publicados en Settings. Coverage inferior al 60% conserva el diagnóstico y excluye la recomendación técnica. Los pesos/revisión/hardware/perfil/contexto de evaluación quedan en el resultado interno. Este resultado no concede permisos para cargar, ejecutar, descargar, adoptar ni promover nada; la composición real deberá seguir usando las autoridades existentes.

`crates/larm-domain/src/evaluation.rs` materializa exactamente `evaluation_reference`, ya publicado en el schema, incluidos escala, procedencia, versión, suite/dataset, fingerprint, status y firma nullable obligatoria. No añade un endpoint ni una representación contractual alternativa. Valida estructura/invariantes y normaliza la escala explícita HIGHER_BETTER/LOWER_BETTER, conservando null y rechazando escalas inválidas/valores no finitos/fuera de rango. Los extremos finitos cuya resta excede f64::MAX se calculan sin producir infinito. Esta validación no autentica una importación ni verifica una firma: ambas obligaciones siguen siendo de P116.

Sólo una referencia PASS de la variante y fingerprint exactos, y de suite/dataset solicitados, puede aportar EvaluationScore. Otras referencias o ausencia de score muestran NO EVALUADO. No se lee evaluación desde metadata libre ni popularidad, descargas, licencia o estrellas. Con evaluación comparable se mezclan los pesos explícitos (default60/40); sin ella se declara ranking técnico. Un conjunto mixto usa orden técnico en toda la lista, conservando el score externo disponible en sus candidatos, para evitar comparar una mezcla con un ajuste técnico puro.

Se resuelven las fuentes equivalentes antes del ranking: mismo modelo canónico, bundle identificado por hash, versión/instalación/backend/ubicación/fingerprint y mismos valores de ajuste/cobertura/confianza/headroom/evaluación/razones. Se prefiere trust verificado y se conservan los IDs completos de alternativas; no se agrupa por nombre ni se autoriza un mirror por ese agrupamiento. El orden global usa RecommendationScore cuando todo el conjunto tiene evaluación comparable; después TechnicalFit descendente, headroom descendente y variant/runtime-version ID ascendentes, con backend/storage/distribution como desempate restante estable. Top-N se aplica después de componer todo el conjunto; los excluidos y sus motivos siguen visibles.

## Verificación

Primer pase: 12 Core PASS y 6/7 Domain; el fallo es del oráculo de roundtrip al comparar Number(1) con Number(1.0). Se corrige para comparar la identidad RFC8785/SHA y el roundtrip tipado; no se afirma haber descubierto un fallo del producto. Segundo pase: 17/17 Core y 7/7 Domain PASS (sesión67992 terminal0). Se incluyen escalas, null, strict fields, referencias no comparables, pesos extremos/cero, cobertura/gates, empates, duplicados, fuentes, almacenamiento read-only y determinismo de serialización.

El selector documental oficial comprueba y copia98 archivos para P092, paquete3.12.1. Las78 copias contractuales coinciden; spec/contrato compartido/manifiesto son idénticos por SHA a la preparación del29, cuya lectura completa ya estaba registrada. No se cambia la norma. Los fixtures de esta fase son sintéticos y sus pruebas son UNIT/CONTRACT, no REAL.

El primer gate oficial Rust termina **FAIL** (6/7 comprobaciones PASS): pasan fmt, Clippy del workspace, fixtures, arquitectura, copias y el runner de evidencia; `cargo test --workspace` se detiene en lifecycle. El runner registra BLOCKED_REAL con4PASS/1BLOCKED y tres bloqueos; su exit0 no equivale a REAL_PASS. Recibo y logs originales conservados en `P092_runs/candidate-composer-20261030/`; sus18 inputs no cambiaron durante la ejecución. No se presume el resultado de los targets posteriores al fallo.

La revisión posterior añade un negativo que reproduce la admisión de un rol fuera del contrato (RED), reutiliza los validadores del dominio y corrige los fixtures para respetar rol GENERAL, estado derivado y stage de instalación. Los artefactos equivalentes conservan también manifiesto y revisión coherentes. Resultado focal: **18/18 Core y7/7 Domain PASS**; Clippy de Core/Domain/daemon con todos sus targets PASS. Logs `domain-validation-red.log`, `domain-validation-green.log` y `review-clippy.log` en la misma carpeta.

El fallo de lifecycle se diagnostica y se conserva como escenario de corrupción, junto a un caso de cierre atómico con journal. Lifecycle completo **5/5 PASS**; [detalle y límites](P051_restart_journal_2026-10-30.md). No se cambia código de producción para omitir la consistencia del journal ni se amplía el timeout.

La repetición general termina **PASS,7/7 comprobaciones**, exit0,2499,27s. `cargo test --workspace -j1 -- --test-threads=1` registra **1443 PASS,0 FAIL y6 ignoradas** en134 resúmenes de harness (incluyen doctests). Las seis aceptaciones optativas son búsqueda optimizada, perfil de fases, GUI nativa, primera adquisición TCP, primera adquisición Core y observación local Ollama; no se ejecutaron en este gate ni se cuentan como PASS. Los sellos históricos de sus ejecuciones separadas conservan su alcance y snapshot. No se repiten frontend ni Tauri porque esta fase sólo modifica Rust sin activar una interfaz nueva.

Durante el gate permanecen idénticos los20 inputs vigilados, incluidos lifecycle y su helper. El contraste independiente posterior confirma esos20 hashes y los hashes de las siete transcripciones. [Recibo oficial](P092_runs/reviewed-composer-20261030/official-rust-gate.command.json), [resumen verificable](P092_runs/reviewed-composer-20261030/verification-synopsis.json) y [gate](check_P092_real.json).

El oráculo independiente de representación ejecuta28 comprobaciones PASS. El runner formal registra **4 PASS,0 FAIL y1 REAL BLOCKED**; su documento mantiene **BLOCKED_REAL**. El validador de evidencia termina PASS, exit0, con inputs sin cambios. [Evidencia formal](P092_recomendacion-de-variante-y-runtime-para-el-pc.evidence.json) y [validación](P092_runs/reviewed-composer-20261030/evidence-schema.command.json). No se convierte el PASS del gate técnico en aceptación física del módulo.

## Pendiente

Faltan productores físicos/cualificación/benchmark vigentes para componer las tuplas desde la app, el importador autenticado P116 con sus restricciones y procedencia, conexión con Search/caché/paginación y la presentación completa P121. Los objetivos/presets técnicos deben suministrar pesos explícitos; el posible proxy de precisión/cuantización del objetivo «quality» sigue pendiente de ese productor y de su etiquetado. No se inventan presets numéricos ni evaluación funcional local.

P044/P091 conservan BLOCKED_REAL integral. Las pruebas del cálculo puro no aceptan ese recorrido ni las dependencias. Los sellos nativos de GUI anteriores son históricos respecto de esta nueva implementación Rust; no se vuelven a atribuir al estado de fuentes actual. Esta fase no cambia frontend ni Broker/Agora ni consume credenciales externas.
