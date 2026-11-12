# Índice de ejecución — revisión 3

Seguir este orden topológico; los códigos se conservan para trazabilidad.

|Paso|Prompt|Dependencias|
|---:|---|---|
|1|[P000 — Inventario real y locks de entorno antes de implementar](prompts/11_complementos/P000_inventario-real-y-locks-de-entorno-antes-de-implementar.md)|—|
|2|[P001 — Bootstrap del monorepo y estructura contractual](prompts/00_fundamentos/P001_bootstrap-del-monorepo-y-estructura-contractual.md)|P000|
|3|[P002 — Reglas de arquitectura, ADR y contratos de dependencia](prompts/00_fundamentos/P002_reglas-de-arquitectura-adr-y-contratos-de-dependencia.md)|P001|
|4|[P003 — Configuración, errores y logging estructurado](prompts/00_fundamentos/P003_configuracion-errores-y-logging-estructurado.md)|P001, P002|
|5|[P004 — Persistencia SQLite y framework de migraciones](prompts/00_fundamentos/P004_persistencia-sqlite-y-framework-de-migraciones.md)|P001, P002, P003|
|6|[P009 — Autenticación, secretos y autorizaciones desde la primera API](prompts/11_complementos/P009_autenticacion-secretos-y-autorizaciones-desde-la-primera-api.md)|P003, P004|
|7|[P005 — API administrativa Axum v1 y OpenAPI](prompts/00_fundamentos/P005_api-administrativa-axum-v1-y-openapi.md)|P003, P004, P009|
|8|[P006 — Shell desktop Tauri, navegación y cliente API](prompts/00_fundamentos/P006_shell-desktop-tauri-navegacion-y-cliente-api.md)|P005|
|9|[P007 — CI local, quality gates y fixtures comunes](prompts/00_fundamentos/P007_ci-local-quality-gates-y-fixtures-comunes.md)|P001, P006, P004, P005|
|10|[P008 — Prerequisitos y packaging de desarrollo Windows](prompts/00_fundamentos/P008_prerequisitos-y-packaging-de-desarrollo-windows.md)|P001, P007, P000|
|11|[P010 — Hardware Profiler: Windows, CPU y memoria](prompts/01_hardware_storage/P010_hardware-profiler-windows-cpu-y-memoria.md)|P003, P004|
|12|[P011 — Hardware Profiler: GPU, NPU y backends](prompts/01_hardware_storage/P011_hardware-profiler-gpu-npu-y-backends.md)|P010|
|13|[P012 — Storage Profiler y Volume Identity Windows](prompts/01_hardware_storage/P012_storage-profiler-y-volume-identity-windows.md)|P010|
|14|[P013 — HardwareFingerprint reproducible](prompts/01_hardware_storage/P013_hardwarefingerprint-reproducible.md)|P010, P011, P012|
|15|[P014 — StorageLocation Registry y políticas de colocación](prompts/01_hardware_storage/P014_storagelocation-registry-y-politicas-de-colocacion.md)|P012, P004|
|16|[P015 — StorageEstimate y preflight de espacio](prompts/01_hardware_storage/P015_storageestimate-y-preflight-de-espacio.md)|P014|
|17|[P019 — Guards de artefactos, reservas y journal transaccional](prompts/11_complementos/P019_guards-de-artefactos-reservas-y-journal-transaccional.md)|P004|
|18|[P016 — ArtifactLocation, resolución de rutas y movimiento seguro](prompts/01_hardware_storage/P016_artifactlocation-resolucion-de-rutas-y-movimiento-seguro.md)|P014, P015, P019|
|19|[P040 — Dominio de modelos, variantes y procedencia](prompts/04_models_sources/P040_dominio-de-modelos-variantes-y-procedencia.md)|P004, P003|
|20|[P041 — ModelBundle, artefactos requeridos e identidad canónica](prompts/04_models_sources/P041_modelbundle-artefactos-requeridos-e-identidad-canonica.md)|P040|
|21|[P017 — Content-addressed store, deduplicación e importación](prompts/01_hardware_storage/P017_content-addressed-store-deduplicacion-e-importacion.md)|P016, P041|
|22|[P018 — Storage Analyzer, offline volumes y rebuild registry](prompts/01_hardware_storage/P018_storage-analyzer-offline-volumes-y-rebuild-registry.md)|P017|
|23|[P020 — Adapter manifest y protocolo v1](prompts/02_adapters/P020_adapter-manifest-y-protocolo-v1.md)|P002, P003, P004, P009|
|24|[P021 — Adapter Host: workers, timeout, crash y lifecycle](prompts/02_adapters/P021_adapter-host-workers-timeout-crash-y-lifecycle.md)|P020|
|25|[P022 — Permisos, confianza y aislamiento de adaptadores](prompts/02_adapters/P022_permisos-confianza-y-aislamiento-de-adaptadores.md)|P020, P021|
|26|[P023 — Adaptador declarativo configurable desde GUI](prompts/02_adapters/P023_adaptador-declarativo-configurable-desde-gui.md)|P020, P021, P022|
|27|[P024 — Plugin Registry, instalación y actualización de adaptadores](prompts/02_adapters/P024_plugin-registry-instalacion-y-actualizacion-de-adaptadores.md)|P020, P022|
|28|[P043 — ModelSourceAdapter protocol y catálogo de fuentes](prompts/04_models_sources/P043_modelsourceadapter-protocol-y-catalogo-de-fuentes.md)|P020, P040, P041|
|29|[P025 — SDK de adaptadores y adaptadores de referencia](prompts/02_adapters/P025_sdk-de-adaptadores-y-adaptadores-de-referencia.md)|P020, P021, P024, P043|
|30|[P026 — Conformance SDK para las cinco familias de plugins](prompts/11_complementos/P026_conformance-sdk-para-las-cinco-familias-de-plugins.md)|P025|
|31|[P030 — Dominio y persistencia de runtimes](prompts/03_runtime_core/P030_dominio-y-persistencia-de-runtimes.md)|P004, P020|
|32|[P031 — Descubrimiento e instalación de versiones de runtime](prompts/03_runtime_core/P031_descubrimiento-e-instalacion-de-versiones-de-runtime.md)|P030, P021, P019|
|33|[P027 — CUSTOM_RULES como datos y estrategia NuGet gestionada](prompts/11_complementos/P027_custom-rules-como-datos-y-estrategia-nuget-gestionada.md)|P023, P031|
|34|[P028 — Registro remoto firmado y exportación/importación de plugins](prompts/11_complementos/P028_registro-remoto-firmado-y-exportacion-importacion-de-plugins.md)|P024, P026|
|35|[P029 — Outbox y eventos durables antes de watchers y procesos](prompts/11_complementos/P029_outbox-y-eventos-durables-antes-de-watchers-y-procesos.md)|P004, P005|
|36|[P032 — Actualización side-by-side, promoción, rollback y pinning](prompts/03_runtime_core/P032_actualizacion-side-by-side-promocion-rollback-y-pinning.md)|P031|
|37|[P033 — Runtime Process Supervisor e instancias](prompts/03_runtime_core/P033_runtime-process-supervisor-e-instancias.md)|P030, P021, P019, P029|
|38|[P034 — Launch Profiles y configuración reproducible](prompts/03_runtime_core/P034_launch-profiles-y-configuracion-reproducible.md)|P030, P033|
|39|[P035 — Memory Estimator y scheduler de instancias](prompts/03_runtime_core/P035_memory-estimator-y-scheduler-de-instancias.md)|P011, P034, P019|
|40|[P036 — InferenceEndpoint y registro de protocolos](prompts/03_runtime_core/P036_inferenceendpoint-y-registro-de-protocolos.md)|P033|
|41|[P037 — Onboarding genérico de runtimes desde GUI/API](prompts/03_runtime_core/P037_onboarding-generico-de-runtimes-desde-gui-api.md)|P023, P030, P031, P036|
|42|[P042 — Lifecycle de modelos y availability derivada](prompts/04_models_sources/P042_lifecycle-de-modelos-y-availability-derivada.md)|P040, P041, P016|
|43|[P044 — Búsqueda federada, normalización y ranking técnico inicial](prompts/04_models_sources/P044_busqueda-federada-normalizacion-y-ranking-tecnico-inicial.md)|P043, P041|
|44|[P045 — Autenticación de fuentes y CredentialReference](prompts/04_models_sources/P045_autenticacion-de-fuentes-y-credentialreference.md)|P003, P043, P009|
|45|[P046 — Hugging Face Source Adapter](prompts/04_models_sources/P046_hugging-face-source-adapter.md)|P025, P043, P045|
|46|[P047 — ModelScope Source Adapter](prompts/04_models_sources/P047_modelscope-source-adapter.md)|P025, P043, P045|
|47|[P048 — GitHub Releases, URL directa, local y UNC Source Adapters](prompts/04_models_sources/P048_github-releases-url-directa-local-y-unc-source-adapters.md)|P043, P045, P017|
|48|[P049 — Fuentes adicionales: Kaggle, Ollama Library, OpenXLab, NGC y Civitai](prompts/04_models_sources/P049_fuentes-adicionales-kaggle-ollama-library-openxlab-ngc-y-civitai.md)|P043, P045, P025|
|49|[P050 — Source Watcher, sincronización incremental y notificaciones](prompts/04_models_sources/P050_source-watcher-sincronizacion-incremental-y-notificaciones.md)|P044, P046, P047, P029|
|50|[P051 — Download Manager: cola, pausa, reanudación y checksum](prompts/04_models_sources/P051_download-manager-cola-pausa-reanudacion-y-checksum.md)|P015, P043, P019, P029|
|51|[P052 — Planificador de tamaño y descarga multi-fuente](prompts/04_models_sources/P052_planificador-de-tamano-y-descarga-multi-fuente.md)|P015, P043, P051, P041|
|52|[P099 — Perfiles externos por versión y fixtures de APIs reales](prompts/11_complementos/P099_perfiles-externos-por-version-y-fixtures-de-apis-reales.md)|P020, P043, P036, P000|
|53|[P060 — llama.cpp: descubrimiento, instalación y versiones](prompts/05_runtime_adapters/P060_llama-cpp-descubrimiento-instalacion-y-versiones.md)|P025, P031, P032, P011, P099|
|54|[P103 — Protocol Sidecar framework](prompts/07_broker_protocol/P103_protocol-sidecar-framework.md)|P036, P021, P026|
|55|[P090 — Compatibility Engine: hard gates](prompts/06_compat_transform/P090_compatibility-engine-hard-gates.md)|P013, P035, P041, P030, P019|
|56|[P061 — llama.cpp: compatibilidad, serving, smoke tests y llama-bench](prompts/05_runtime_adapters/P061_llama-cpp-compatibilidad-serving-smoke-tests-y-llama-bench.md)|P060, P034, P036, P042, P103, P090|
|57|[P071 — Python Environment Manager y PyTorch/Transformers](prompts/05_runtime_adapters/P071_python-environment-manager-y-pytorch-transformers.md)|P031, P022, P040|
|58|[P062 — ONNX Runtime base y Execution Providers](prompts/05_runtime_adapters/P062_onnx-runtime-base-y-execution-providers.md)|P025, P031, P011, P071, P099|
|59|[P063 — ONNX Runtime GenAI: LLM/VLM serving](prompts/05_runtime_adapters/P063_onnx-runtime-genai-llm-vlm-serving.md)|P062, P036, P034, P103, P090|
|60|[P064 — Windows ML y proveedores AMD/otros](prompts/05_runtime_adapters/P064_windows-ml-y-proveedores-amd-otros.md)|P062, P011, P099|
|61|[P093 — Transformation Pipeline framework](prompts/06_compat_transform/P093_transformation-pipeline-framework.md)|P016, P040, P051|
|62|[P065 — Microsoft Olive: transformación y optimización ONNX](prompts/05_runtime_adapters/P065_microsoft-olive-transformacion-y-optimizacion-onnx.md)|P062, P071, P093|
|63|[P066 — stable-diffusion.cpp adapter](prompts/05_runtime_adapters/P066_stable-diffusion-cpp-adapter.md)|P025, P031, P034, P036, P103, P099, P090|
|64|[P067 — whisper.cpp adapter](prompts/05_runtime_adapters/P067_whisper-cpp-adapter.md)|P025, P031, P034, P036, P103, P099, P090|
|65|[P068 — Lemonade: detección y adopción no destructiva](prompts/05_runtime_adapters/P068_lemonade-deteccion-y-adopcion-no-destructiva.md)|P025, P030, P045, P099|
|66|[P069 — Lemonade: certificación modelo por modelo para AI Broker](prompts/05_runtime_adapters/P069_lemonade-certificacion-modelo-por-modelo-para-ai-broker.md)|P068, P036, P042, P090|
|67|[P070 — Ollama runtime/host adapter](prompts/05_runtime_adapters/P070_ollama-runtime-host-adapter.md)|P025, P030, P036, P099, P090|
|68|[P072 — Diffusers/PyTorch runtime](prompts/05_runtime_adapters/P072_diffusers-pytorch-runtime.md)|P071, P036, P103, P099, P090|
|69|[P073 — MLC LLM adapter](prompts/05_runtime_adapters/P073_mlc-llm-adapter.md)|P025, P031, P036, P093, P103, P099, P090|
|70|[P074 — sherpa-onnx adapter](prompts/05_runtime_adapters/P074_sherpa-onnx-adapter.md)|P062, P036, P103, P099, P090|
|71|[P075 — OpenVINO Runtime adapter](prompts/05_runtime_adapters/P075_openvino-runtime-adapter.md)|P025, P031, P036, P103, P099, P090|
|72|[P076 — Foundry Local adapter](prompts/05_runtime_adapters/P076_foundry-local-adapter.md)|P025, P030, P036, P099, P090|
|73|[P077 — CTranslate2 adapter](prompts/05_runtime_adapters/P077_ctranslate2-adapter.md)|P025, P031, P036, P093, P103, P099, P090|
|74|[P078 — ComfyUI host adapter](prompts/05_runtime_adapters/P078_comfyui-host-adapter.md)|P025, P030, P036, P022, P099, P090|
|75|[P079 — LM Studio host adapter](prompts/05_runtime_adapters/P079_lm-studio-host-adapter.md)|P025, P030, P036, P099, P090|
|76|[P080 — KoboldCpp adapter](prompts/05_runtime_adapters/P080_koboldcpp-adapter.md)|P025, P030, P036, P099, P090|
|77|[P081 — WSL2 y Container execution host abstraction](prompts/05_runtime_adapters/P081_wsl2-y-container-execution-host-abstraction.md)|P030, P022, P031|
|78|[P082 — Adaptador oficial NCNN y serving por worker](prompts/11_complementos/P082_adaptador-oficial-ncnn-y-serving-por-worker.md)|P025, P031, P036, P103, P099|
|79|[P083 — Adaptador oficial MNN y bundles de inferencia](prompts/11_complementos/P083_adaptador-oficial-mnn-y-bundles-de-inferencia.md)|P025, P031, P036, P103, P099|
|80|[P091 — Estimación de recursos y TechnicalFitScore](prompts/06_compat_transform/P091_estimacion-de-recursos-y-technicalfitscore.md)|P090, P015, P035|
|81|[P092 — Recomendación de variante y runtime para el PC](prompts/06_compat_transform/P092_recomendacion-de-variante-y-runtime-para-el-pc.md)|P044, P091|
|82|[P094 — Pipeline GGUF: conversión y cuantización](prompts/06_compat_transform/P094_pipeline-gguf-conversion-y-cuantizacion.md)|P060, P093|
|83|[P095 — Pipeline ONNX y Olive: exportación y cuantización](prompts/06_compat_transform/P095_pipeline-onnx-y-olive-exportacion-y-cuantizacion.md)|P062, P065, P093|
|84|[P096 — Provenance de conversiones y lineage graph](prompts/06_compat_transform/P096_provenance-de-conversiones-y-lineage-graph.md)|P093, P094, P095|
|85|[P110 — Benchmark Engine framework](prompts/08_benchmark_evaluation/P110_benchmark-engine-framework.md)|P013, P034, P096|
|86|[P100 — Runtime Lease service](prompts/07_broker_protocol/P100_runtime-lease-service.md)|P033, P035, P036, P042, P019, P090|
|87|[P101 — Contrato LARM con AI Broker y LocalRuntimeResolver](prompts/07_broker_protocol/P101_contrato-larm-con-ai-broker-y-localruntimeresolver.md)|P100, P009, P069|
|88|[P113 — Event Bus, notificaciones y auditoría](prompts/08_benchmark_evaluation/P113_event-bus-notificaciones-y-auditoria.md)|P005, P050, P110, P029|
|89|[P107 — Cambio compañero en AI Broker: LocalRuntimeResolver real](prompts/11_complementos/P107_cambio-companero-en-ai-broker-localruntimeresolver-real.md)|P101, P099|
|90|[P118 — Cambios compañeros en Agora, Lab y Model Drift](prompts/11_complementos/P118_cambios-companeros-en-agora-lab-y-model-drift.md)|P107, P110, P113|
|91|[P114 — Agora Bridge entre PC IA y PC principal](prompts/08_benchmark_evaluation/P114_agora-bridge-entre-pc-ia-y-pc-principal.md)|P113, P110, P118|
|92|[P115 — Contrato con Local-AI-Lab](prompts/08_benchmark_evaluation/P115_contrato-con-local-ai-lab.md)|P101, P114|
|93|[P116 — Contrato con Model Drift y certificaciones](prompts/08_benchmark_evaluation/P116_contrato-con-model-drift-y-certificaciones.md)|P115|
|94|[P111 — Benchmarks de texto y LLM](prompts/08_benchmark_evaluation/P111_benchmarks-de-texto-y-llm.md)|P110, P061, P063|
|95|[P112 — Benchmarks de imagen, vídeo, ASR y TTS](prompts/08_benchmark_evaluation/P112_benchmarks-de-imagen-video-asr-y-tts.md)|P110, P066, P067, P074|
|96|[P142 — Cobertura normativa y manifiesto de evidencia por requisito](prompts/11_complementos/P142_cobertura-normativa-y-manifiesto-de-evidencia-por-requisito.md)|P007|
|97|[P117 — Detección de regresiones y promoción segura](prompts/08_benchmark_evaluation/P117_deteccion-de-regresiones-y-promocion-segura.md)|P032, P110, P116, P111, P112, P142|
|98|[P097 — Actualización de modelos y revisiones coexistentes](prompts/11_complementos/P097_actualizacion-de-modelos-y-revisiones-coexistentes.md)|P050, P051, P042, P117|
|99|[P098 — Importadores de todos los bundles y recuperación sin base de datos](prompts/11_complementos/P098_importadores-de-todos-los-bundles-y-recuperacion-sin-base-de-datos.md)|P017, P041|
|100|[P102 — Capability contracts multimodales para AI Broker](prompts/07_broker_protocol/P102_capability-contracts-multimodales-para-ai-broker.md)|P101, P074, P066, P067, P071, P072, P073, P075, P077, P078, P082, P083|
|101|[P104 — MCP Administration Server](prompts/07_broker_protocol/P104_mcp-administration-server.md)|P005, P022, P051, P032, P100, P029|
|102|[P105 — MCP opcional de acceso a modelos](prompts/07_broker_protocol/P105_mcp-opcional-de-acceso-a-modelos.md)|P103, P102|
|103|[P106 — Laya y modelos System-1 dentro de LARM](prompts/07_broker_protocol/P106_laya-y-modelos-system-1-dentro-de-larm.md)|P042, P092, P100, P103|
|104|[P108 — Cambio compañero en AI Broker: contratos multimodales](prompts/11_complementos/P108_cambio-companero-en-ai-broker-contratos-multimodales.md)|P107, P102, P103, P106|
|105|[P119 — Admisión autorizada de descargas automáticas](prompts/11_complementos/P119_admision-autorizada-de-descargas-automaticas.md)|P050, P052, P009|
|106|[P120 — Dashboard real de Inicio](prompts/09_gui/P120_dashboard-real-de-inicio.md)|P006, P113, P014, P033|
|107|[P121 — Descubrir: búsqueda federada y filtros](prompts/09_gui/P121_descubrir-busqueda-federada-y-filtros.md)|P044, P092, P120|
|108|[P122 — Detalle de modelo, variantes y preflight de descarga](prompts/09_gui/P122_detalle-de-modelo-variantes-y-preflight-de-descarga.md)|P052, P092, P121|
|109|[P123 — Mis modelos y gestión multidisco](prompts/09_gui/P123_mis-modelos-y-gestion-multidisco.md)|P016, P018, P042, P098|
|110|[P124 — Runtimes, versiones, backends e instancias](prompts/09_gui/P124_runtimes-versiones-backends-e-instancias.md)|P032, P033, P037|
|111|[P125 — Descargas y transformaciones](prompts/09_gui/P125_descargas-y-transformaciones.md)|P051, P093|
|112|[P126 — Benchmarks, evaluaciones y comparaciones](prompts/09_gui/P126_benchmarks-evaluaciones-y-comparaciones.md)|P110, P116, P117|
|113|[P141 — Autenticación privada OAuth, SSH, cookies y Git LFS](prompts/11_complementos/P141_autenticacion-privada-oauth-ssh-cookies-y-git-lfs.md)|P045, P046, P047, P048, P049|
|114|[P127 — Fuentes, cuentas y adaptadores](prompts/09_gui/P127_fuentes-cuentas-y-adaptadores.md)|P024, P045, P049, P141, P028|
|115|[P128 — Notificaciones, watchers y actualizaciones](prompts/09_gui/P128_notificaciones-watchers-y-actualizaciones.md)|P050, P113, P032, P097, P119|
|116|[P139 — Pantalla Hardware y asistente completo de almacenamiento](prompts/11_complementos/P139_pantalla-hardware-y-asistente-completo-de-almacenamiento.md)|P120, P011, P014, P015, P018|
|117|[P140 — Configuración completa y asistentes de runtime y fuente](prompts/11_complementos/P140_configuracion-completa-y-asistentes-de-runtime-y-fuente.md)|P127, P037, P139, P028, P027, P119|
|118|[P129 — UX, accesibilidad y estados de error finales](prompts/09_gui/P129_ux-accesibilidad-y-estados-de-error-finales.md)|P120, P128, P139, P140|
|119|[P130 — Sandboxing, remote code y custom components](prompts/10_hardening_release/P130_sandboxing-remote-code-y-custom-components.md)|P022, P071, P078, P026, P028|
|120|[P131 — Secretos, auth local y permisos API](prompts/10_hardening_release/P131_secretos-auth-local-y-permisos-api.md)|P045, P005, P104, P009|
|121|[P132 — Startup reconciliation, offline mode y crash recovery](prompts/10_hardening_release/P132_startup-reconciliation-offline-mode-y-crash-recovery.md)|P018, P033, P051, P093, P019, P029, P100|
|122|[P133 — Updater de LARM, backup y rebuild](prompts/10_hardening_release/P133_updater-de-larm-backup-y-rebuild.md)|P004, P132, P098|
|123|[P134 — Diagnostics bundle, privacidad y observabilidad final](prompts/10_hardening_release/P134_diagnostics-bundle-privacidad-y-observabilidad-final.md)|P003, P113, P131|
|124|[P143 — Pruebas de aislamiento del SO y endpoints remotos](prompts/11_complementos/P143_pruebas-de-aislamiento-del-so-y-endpoints-remotos.md)|P130, P131, P028|
|125|[P137 — Packaging Windows, User Background Agent y Windows Service opcional](prompts/10_hardening_release/P137_packaging-windows-user-background-agent-y-windows-service-opcional.md)|P132, P131, P133|
|126|[P135 — Suite end-to-end del producto](prompts/10_hardening_release/P135_suite-end-to-end-del-producto.md)|P129, P132, P107, P108, P118, P143, P134, P137, P142, P082, P083, P097, P098, P119, P139, P140, P141, P027, P028, P026|
|127|[P136 — Stress, concurrencia y rendimiento del daemon](prompts/10_hardening_release/P136_stress-concurrencia-y-rendimiento-del-daemon.md)|P135|
|128|[P138 — Aceptación final, auditoría contra spec y release candidate](prompts/10_hardening_release/P138_aceptacion-final-auditoria-contra-spec-y-release-candidate.md)|P001, P002, P003, P004, P005, P006, P007, P008, P010, P011, P012, P013, P014, P015, P016, P017, P018, P020, P021, P022, P023, P024, P025, P030, P031, P032, P033, P034, P035, P036, P037, P040, P041, P042, P043, P044, P045, P046, P047, P048, P049, P050, P051, P052, P060, P061, P062, P063, P064, P065, P066, P067, P068, P069, P070, P071, P072, P073, P074, P075, P076, P077, P078, P079, P080, P081, P090, P091, P092, P093, P094, P095, P096, P100, P101, P102, P103, P104, P105, P106, P110, P111, P112, P113, P114, P115, P116, P117, P120, P121, P122, P123, P124, P125, P126, P127, P128, P129, P130, P131, P132, P133, P134, P135, P136, P137, P000, P009, P019, P026, P027, P028, P029, P082, P083, P097, P098, P099, P107, P108, P118, P119, P139, P140, P141, P142, P143|
