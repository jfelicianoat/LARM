# P091 — Estimación de recursos y TechnicalFitScore

Fecha: 2026-10-22  
Estado: `BLOCKED_REAL`

P091 añade un cálculo puro de TechnicalFit para candidatos compatibles y con todas las hard gates en `PASS`. Usa los pesos y fórmulas del contrato 05, excluye dimensiones desconocidas del denominador, presenta coverage y deja el score como `N/A` para candidatos incompatibles, desconocidos o no cualificados. No calcula evaluación externa ni calidad del modelo.

## Cambios

- [Score P091](../../crates/larm-core/src/compatibility/score.rs) implementa los siete componentes: memoria (headroom requerido frente al presupuesto), backend efectivo con evidencia, madurez, velocidad, almacenamiento medido, contexto y estabilidad.
- El resultado interno contiene score, breakdown por componente, peso, fuente, confidence y base observada. Para memory enseña bytes requeridos, presupuesto y headroom; para storage enseña clase y MB/s. Los valores desconocidos quedan `None` y sus pesos no entran en el denominador. Coverage se expresa en porcentaje y el gate de coverage indica únicamente si se alcanza el 60% requerido para la etapa de recomendación.
- Un benchmark sólo se usa si su SHA-256 coincide con el fingerprint de ejecución del candidato. Cinco o más medidas exitosas permiten sustituir el estimate de memoria por el pico observado, y proporcionar velocidad/MB/s medidos; estabilidad usa hasta las últimas 20 ejecuciones coincidentes y requiere cinco. Sin benchmark coincidente se conservan sólo los estimates permitidos; la velocidad y el almacenamiento no se imputan.
- Confidence total es conservadora: toma el nivel de menor confianza entre los componentes incluidos. El contrato no prescribe cómo agregar confidence; esta regla se mantiene interna al motor.
- El [schema contractual](../contracts/schemas/domain.schema.json) continúa exponiendo `technical_fit` como escalar nullable. El breakdown y confidence permanecen en el resultado interno, pues el schema cerrado no permite campos públicos alternativos. El cálculo no se persiste ni se proyecta al endpoint read-only de P090 sin un productor y una operación contractual de escritura.

## Validación

Pasan 9 pruebas focales en Windows: determinismo y pesos contra `defaults.json`; sustitución de memoria, velocidad y almacenamiento por observaciones con fingerprint igual; descarte del benchmark si cambia el fingerprint; mínimo de cinco medidas; cálculo de coverage sin imputar; backend detectado pero no efectivo; score `N/A` para incompatibilidad y compatibility desconocida; rechazo de un CompatibilityRecord inválido; y serialización pública escalar sin breakdown/confidence añadidos. La prueba de schema acepta `technical_fit` dentro de rango y rechaza campos públicos nuevos y score fuera de rango. Clippy focal y formato también pasan.

El runner focal actual registra 5 PASS, 0 FAIL y 1 prueba real BLOCKED en [P091 PromptEvidence](P091_estimacion-de-recursos-y-technicalfitscore.evidence.json). El gate histórico [check_P091_real.json](check_P091_real.json) conserva el primer intento restringido: tres pruebas HTTP de health local no podían alcanzar loopback y la copia de contratos aún estaba divergida. La prueba aislada de health pasó con loopback habilitado; después `cargo test --workspace -j 1 -- --test-threads=1` terminó con exit 0 y todas las pruebas pasaron. La revisión contractual 3.1.0 sincronizó las copias y `tools/package_copy.py check` vuelve a pasar. El gate completo no se repitió después de esos cambios documentales.

## Límites y bloqueos

- La huella, los gates y los benchmarks aún no se componen desde runtime real cualificado. No hay un benchmark local enlazado a la ejecución persistida, y P090, P015 y P035 no tienen `REAL_PASS`; por contrato 00, P091 no puede cerrar como `REAL_PASS`.
- El contrato 05 asigna el score de storage a MB/s observados y §115 identifica clases NVME/SSD/HDD/NETWORK/REMOVABLE/UNKNOWN, pero ninguno fija un multiplicador de clase. El breakdown muestra la clase y no aplica un ajuste inventado; queda pendiente definir cómo debe alterar una recomendación numérica.
- La forma pública sólo admite el escalar `technical_fit`. Para ofrecer breakdown/confidence en la interfaz hace falta una representación contractual versionada antes de publicarlos.
- Autorización, cancelación, IO, journal/outbox, reservas y recuperación no aplican al módulo puro porque no realiza efectos externos. Los rechazos de schema, estado y evidencia sí se cubren.

El recibo está en [P091 PromptEvidence](P091_estimacion-de-recursos-y-technicalfitscore.evidence.json). El estado se mantiene `BLOCKED_REAL` hasta que las dependencias y la ejecución local cualificada aporten evidencia real.
