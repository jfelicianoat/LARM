# P092 — Recomendación de variante y runtime para el PC

Fecha:2026-10-30. Estado: `BLOCKED_REAL`. Compositor interno implementado y probado con fixtures; aceptación física, importación externa y conexión con Search pendientes. El objetivo global de LARM permanece activo.

Se verificó y copió el contexto documental con el selector oficial:98 archivos para la preparación actual, paquete3.12.1 y78 copias contractuales idénticas. La especificación íntegra, contrato compartido y manifiesto mantienen los hashes de la lectura registrada el29. P044/P091 siguen sin REAL_PASS integral; este trabajo independiente no completa al consumidor.

`crates/larm-core/src/recommendation.rs` compone las entidades completas del dominio con compatibilidad y contexto hardware/perfil. Reutiliza validadores y P091 con pesos efectivos versionados, aplica cobertura y gates, ranking técnico o combinado, fuentes equivalentes, desempates deterministas y top-N. Conserva identidades, alternativas, razones y excluidos. No concede permisos de ejecución ni cambia el contrato HTTP. [Implementación y alcance](P092_candidate_composer_2026-10-30.md).

`crates/larm-domain/src/evaluation.rs` materializa el `evaluation_reference` cerrado ya publicado y normaliza únicamente una escala explícita. Los nullable siguen obligatorios; no hay score inventado ni evaluación desde metadata/popularidad. Sin una referencia PASS comparable se conserva NO EVALUADO. La validación estructural no autentica procedencia, no verifica firmas ni sustituye P116.

Verificación focal:18/18 Core y7/7 Domain PASS después de reproducir y corregir la admisión de registros de dominio inválidos. Clippy de Core/Domain/daemon con todos sus targets PASS. El primer gate general falla en lifecycle (6/7 checks PASS); se conserva completo. El diagnóstico identifica un fixture sin journal, mantiene ese escenario como corrupción y añade el cierre atómico con journal:5/5 lifecycle PASS. [Diagnóstico](P051_restart_journal_2026-10-30.md).

La repetición general pasa **7/7 comprobaciones**: workspace Rust1443 PASS/0FAIL y6 aceptaciones optativas ignoradas (no contadas como superadas), fmt, Clippy, fixtures, arquitectura, copias y runner. Los20 inputs y las siete transcripciones coinciden por SHA después de finalizar. Oráculo del schema28/28 PASS y evidencia formal validada; **4 PASS/0FAIL/1REAL BLOCKED**, estado BLOCKED_REAL. [Gate](check_P092_real.json), [recibo y alcance](P092_runs/reviewed-composer-20261030/verification-synopsis.json), [evidencia](P092_recomendacion-de-variante-y-runtime-para-el-pc.evidence.json). No se repiten frontend/Tauri ni se habilita una recomendación en la app en esta fase.

## Trabajo que sigue siendo necesario

- Conectar productores físicos de catálogo, cualificación, compatibilidad y benchmark a la tupla completa con evidencia vigente. Los fixtures actuales no constituyen esa prueba.
- Conectar P116 autenticado y sus reglas de origen, estado y firma; verificar la evaluación externa y sus artefactos con los servicios reales.
- Suministrar objetivos técnicos como pesos efectivos y etiquetar cualquier proxy de precisión/cuantización, sin inventar evaluación funcional ni presets numéricos.
- Componer el resultado con Search y su contexto de caché/paginación, conservando H05 y distinguiendo recomendación técnica de evaluación. No inventar un endpoint, DTO o preset numérico que no figure en los contratos.
- Ejecutar los casos sin evaluación, con evaluación y mismo artefacto en varias fuentes; negativos, benchmark de ejecución cualificada y prueba real sobre el host. Las pruebas del cálculo puro no cierran esos recorridos.

P090/P015/P035 mantienen aceptación real pendiente. La GUI de comparación/plan/descarga/verificación ya tiene pruebas nativas históricas del30 tras desbloquear Windows; esas pruebas no cualifican recomendaciones y corresponden a su snapshot anterior. [Evidencia de ese recorrido](P044_models_gui_2026-10-30.md). Esta fase no cambia frontend ni consume credenciales Broker/Agora.
