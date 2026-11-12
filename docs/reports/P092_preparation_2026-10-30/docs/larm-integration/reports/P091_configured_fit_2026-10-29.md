# P091/P092 — ponderación configurada del ajuste técnico

Estado: preparación independiente verificada; P091 y sus dependencias conservan `BLOCKED_REAL`. El objetivo global continúa activo.

## Implementación

`calculate_technical_fit_with_weights` admite el tipo `Settings.benchmarks.technical_fit_weights` ya publicado. La entrada anterior `calculate_technical_fit` conserva exactamente los pesos predeterminados 30/20/10/15/10/10/5 y su resultado; no se modifican schemas, defaults ni rutas API. La composición futura deberá suministrar la configuración efectiva y conservar su revisión con el assessment. La nueva función no carga ni persiste Settings por sí sola.

La media ponderada elimina del denominador sólo las dimensiones sin dato y normaliza coverage frente al total de pesos configurados. Una dimensión de peso cero no altera la confianza agregada. Siete valores u32 máximos se suman en u64, y el gate60% usa productos enteros para evitar decisiones de elegibilidad por redondeo. Una política íntegramente cero devuelve N/A, coverage0, confianza UNKNOWN y gate falso; no se rechaza una representación que el schema existente admite ni se inventa un score.

Se conservan validación de CompatibilityRecord, todos los hard gates, la huella exacta del benchmark, mínimo de cinco mediciones y ausencia de imputación. El breakdown continúa siendo interno, con los pesos suministrados y los componentes ignorados visibles. Este cambio no produce un EvaluationScore ni interpreta popularidad como calidad.

Los campos de ponderación técnica y de mezcla recomendación/evaluación ya existen en Settings; no hace falta publicar nuevos campos para preparar su cálculo. El60/40 sigue siendo el valor predeterminado de recomendación, sin haberse implementado aquí su mezcla ni la autoridad de importación externa P116.

## Verificación

Diez casos nuevos, junto con los nueve anteriores: equivalencia de defaults, cambio explícito de objetivo técnico, coverage60/40 sin inventar velocidad, confianza sin dimensiones deshabilitadas, todos los pesos cero, máximos u32, gates fallidos, invariancia de escala de pesos, roundtrip de Settings y rechazo de benchmark con otra huella.

El primer pase de17 casos dio16 PASS/1 FAIL por un literal incorrecto en la nueva prueba de confianza: el backend evidenciado ya usa EXACT, mientras la prueba esperaba HIGH. Se corrigió únicamente ese literal conforme al comportamiento preexistente y se añadieron roundtrip/huella. No se afirma haber reproducido un fallo real del producto con ese RED.

La ejecución final pasa64/64 pruebas unitarias de larm-core, incluidas19 de score, en5,33 s (wrapper14,86 s). Clippy de todos los targets de larm-core con `-D warnings` pasa en35,50 s; fmt, grafo de17 crates,78 copias contractuales y prueba del schema escalar también pasan. La ejecución de fixtures se clasifica UNIT, no REAL.

Recibos, comandos, logs completos y hashes: `P091_runs/configured-fit-20261029/`. El registro anterior de evidencia se conserva como `prior.evidence.json`; la evidencia vigente conserva el benchmark local cualificado como BLOCKED.

## Pendientes y alcance

P090, P015 y P035 permanecen BLOCKED_REAL según sus reports actuales. No hay benchmark local cualificado conectado a este cálculo ni se ha compuesto un assessment productivo desde runtime/backend/hardware/profile. No se declara REAL_PASS, recomendación activa o readiness de P092.

Se seleccionó y verificó el contexto documental de P092 (96 archivos, paquete3.12.1) y se leyó la especificación completa de3159 líneas no vacías, contrato compartido, manifiesto y contratos aplicables. La copia documental no verifica las dependencias. P092 sigue necesitando candidatos completos, ranking/top-N, evaluación externa comparable y selección de fuente; no se añaden DTOs públicos alternativos ni un endpoint sin contrato.

El recorrido visual sigue pendiente de confirmar sesión Windows desbloqueada y activa, por el error de Sky `GetCursorPos: Acceso denegado`. No se intentó de nuevo en este incremento. Las pruebas TCP previas del lector de cualificación pertenecen a su snapshot anterior: este cambio de Core no está cubierto por aquel sello nativo y no se han recompilado/rerun los binarios de aceptación por este cálculo puro.
