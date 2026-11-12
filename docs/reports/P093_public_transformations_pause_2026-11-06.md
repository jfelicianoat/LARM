# P093 — integración pública y punto de pausa

Fecha: 2026-11-06. Desarrollo pausado por petición explícita del usuario, en un punto sin pruebas ni procesos auxiliares en ejecución. **Implementación pendiente de validación nativa completa.** P093 conserva preparación/BLOCKED_REAL, sin CODE_PASS integral ni REAL_PASS. Contrato público 3.13.0 autorizado, sin cambios en este incremento.

## Implementación conservada

El daemon compone las rutas públicas existentes con selección de una instalación cualificada actual, preview real, input físico, destino/versiones y espacio observado. Solo registra la receta de referencia Base64. El plan distingue datos algorítmicos de cuotas de tamaño UNKNOWN acotadas; crear el plan no admite ejecución. La confirmación revalida los recursos y guarda atómicamente la admisión y una señal durable privada de despacho. Los replays exactos se resuelven antes de tocar archivos o cualificación.

El consumidor espera al inventario inicial de recuperación y enlaza intención durable, helper registrado, Execute/Verify observados, cierre nativo y publicación CAS. El reinicio usa el mismo trabajo, un plan nuevo y control de revisión. Se conservan las anclas del host/base durante sus tareas. La señal de despacho no concede autoridad de ejecución ni de cierre. [Decisión y límites](../adr/0016-productor-consumidor-transformacion-publica.md).

Estas rutas están implementadas, pero la prueba pública se ha detenido antes de alcanzar su primer plan. No se presenta la implementación como un recorrido validado ni se hereda el PASS de las fuentes anteriores.

## Pruebas terminadas

La corrida congelada `attempt1` terminó entre 17:12:11 y 17:16:16 UTC, con código101: **358 pruebas Rust correctas, una fallida y cero ignoradas**. Se completaron doce comprobaciones y falló la decimotercera de veintiocho programadas; las quince restantes no se ejecutaron. Sus 890 entradas permanecieron intactas y tienen copia exacta de fuentes.

Pasaron formato, arquitectura, copias contractuales, Clippy de cinco crates/todos los targets con warnings denegados, 77 pruebas unitarias DB, 63 de planes/admisión/reinicio, 181 de dominio, seis HTTP, diez OpenAPI, dieciocho del protocolo del supervisor y tres del supervisor nativo. Las cinco nuevas pruebas SQL cubren señal durable, replay, separación de admisión directa, corrupción/omisiones con rollback y UNKNOWN acotado sin presupuesto. Las pruebas de desarrollo SQL fallidas anteriores y su corrección de fixture también se conservan; la última ejecución SQL terminó con 63/63 PASS.

La prueba pública recibió **401 AUTH_REQUIRED / bearer_invalid**, cuando su primer negativo esperaba404 por adaptador inexistente. El comprobador había creado un segundo principal temporal después de usar el servidor con el primero, y lo utilizó inmediatamente. El autenticador conserva una caché de cinco segundos y solo la recarga al caducar. Es una explicación respaldada por el código; todavía no se ha aplicado ni verificado el ajuste del comprobador. La aserción ocurre antes de crear el primer plan o admitir un trabajo de transformación.

Al propagarse la aserción se ejecutaron los destructores de los principales temporales y del servidor propio. El log no contiene errores de revocación; esto no sustituye una auditoría independiente de Credential Manager. La consulta CIM final, válida, encontró cero procesos auxiliares del workspace. No se utilizaron PIDs ni ausencia de proceso como autoridad para liberar recursos Core. En esta prueba no se llegó a admitir un trabajo.

La auditoría de preservación contrasta fuentes actuales/copias, logs, recibos y artefactos nativos de los pasos terminados. Su PASS acredita conservación del fallo, **no aceptación funcional de esta fase**. El auditor completo de éxito no se ejecutó.

## Punto exacto para retomar

1. Ajustar el alta/uso del segundo principal de la fixture a la frontera de recarga vigente —o crearlo antes del primer uso del autenticador— sin debilitar autenticación ni cambiar expectativas de autorización.
2. Reejecutar el caso público con fuentes congeladas y, si pasa, las veintiocho comprobaciones y el auditor independiente. Verificar creación sin ejecución, confirmación, recuperación del servidor propio, plan nuevo, reinicio del mismo trabajo, replay y bytes/manifiestos CAS.
3. No atribuir aceptación a progreso/cancelación, caída durante bytes parciales H07, pérdida definitiva del testigo/reinicio del sistema, conversores GGUF/ONNX, runtimes ni UI. Una intención sin testigo observable conserva recursos; su política sigue pendiente.

El resto del objetivo global LARM queda también pausado. Las autorizaciones contractuales anteriores continúan vigentes; no hace falta repetirlas al reanudar.

## Evidencias

- [Resumen fallido](P093_runs/public-transform-20261106/attempt1/verification-synopsis.json), [salida terminal](P093_runs/public-transform-20261106/attempt1/terminal-wrapper.json) y [log del caso público](P093_runs/public-transform-20261106/attempt1/verify-public-transformation.log).
- [Auditoría de conservación](P093_runs/public-transform-20261106/attempt1/failure-preservation-audit.json), [comprobación de procesos](P093_runs/public-transform-20261106/attempt1/post-run-process-check.json) y [fuentes exactas](P093_runs/public-transform-20261106/attempt1/source-before).
- [SQL de desarrollo final](P093_runs/public-transform-20261106/development-sql-final.json) y [baseline preservado](P093_runs/public-transform-20261106/baseline.json).
- El último incremento anterior validado conserva [488 pruebas/27 comprobaciones](P093_guardian_output_publication_2026-11-02.md), correspondientes a sus fuentes de 2026-11-02 y a su alcance interno explícito.
