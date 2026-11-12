# P122 — Detalle de modelo, variantes y preflight de descarga

Estado: `BLOCKED_REAL`.

La pantalla “Mis modelos” ofrece dos vistas para técnicos locales: inventario físico y variantes disponibles para comparar o preparar una descarga. La carga es progresiva; los filtros muestran disponibilidad, daños, actualizaciones y propiedad. El detalle compara fuente, formato, cuantización, ajuste técnico y tamaños locales observados.

El preflight usa `POST /api/v1/download-plans`, permite seleccionar artefactos opcionales y establece un presupuesto duro en GiB para tamaños desconocidos. Distingue tamaños exactos, estimados por intervalo y desconocidos. Para cada destino enseña el nombre configurado, letra, espacio libre, reservado y utilizable, y la razón de elegibilidad. El botón «Preparar plan para este destino» persiste el plan de la ubicación elegible y muestra cuándo caduca; preparar el plan no reserva espacio ni crea un job.

La [revalidación del 2026-10-25](P051_http_admission_2026-10-25.md) conecta la confirmación explícita con `POST /api/v1/downloads`. Exige plan vigente para el destino preparado, impide doble envío y recupera una respuesta perdida con cuerpo y clave idénticos. Muestra el recibo y un enlace a Descargas sólo al comprobar la aceptación; caducidad y rechazo de acceso siguen siendo errores. El daemon revalida la fuente cualificada antes de admitir el trabajo y refresca el espacio después de esa validación.

Tras comparar, el panel de licencia y acceso muestra la licencia declarada, el modo de autenticación y si existe una cuenta vinculada, junto con la confianza y validación del conector instalado. Si la fuente o el conector no se pueden consultar, lo indica como desconocido. El texto aclara que preparar el plan no acepta la licencia ni prueba credenciales o acceso anónimo; la autorización efectiva sigue correspondiendo a la ejecución.

## Comprobaciones

- `npm run lint`, `npm run typecheck`, `npm test` y `npm run build`: PASS.
- Suite del escritorio actual: 16 archivos y 118 pruebas PASS.
- Pruebas focales de P122: el módulo pasó 18/18; cubre tamaños exactos/desconocidos, selección opcional, conversión del presupuesto duro, comparación, paginación, destinos insuficientes, preparación por destino, confirmación explícita y doble clic, replay tras pérdida de respuesta, caducidad, rechazo de acceso, advertencias de licencia/acceso/confianza y la lectura de trust real del conector local.
- `python tools/p122_evidence.py` permite repetir pruebas focales, suite del escritorio, lint, typecheck y build; no se ejecutó completo en este incremento. El PromptEvidence previo registra históricamente 5 PASS, 0 FAIL y 1 prueba REAL bloqueada.

Los transportes de las pruebas de interfaz usan respuestas contractuales simuladas y acreditan `CODE_PASS`. También se revisó visualmente en escritorio y anchura 375. La API real del daemon ya demuestra preflight, confirmación única y recuperación de una descarga controlada tras reinicio; falta el recorrido GUI→daemon integrado y el proveedor externo. P051 continúa sin aceptación completa; P092/P121 tampoco han alcanzado `REAL_PASS`. Por ello P122 no se cierra. Las advertencias muestran configuración declarada y trust del conector, pero falta demostrar la aceptación de licencia y el acceso privado durante una ejecución real.

Evidencia: [PromptEvidence](P122_detalle-de-modelo-variantes-y-preflight-de-descarga.evidence.json). La entrada visual se conecta desde [App.tsx](../../apps/desktop/src/App.tsx); la pantalla y sus pruebas están en [models.tsx](../../apps/desktop/src/models.tsx) y [models.test.tsx](../../apps/desktop/src/models.test.tsx).
