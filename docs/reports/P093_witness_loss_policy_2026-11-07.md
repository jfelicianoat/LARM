# P093 — política de pérdida definitiva del testigo / reinicio del sistema

Fecha: 2026-11-07. Incremento posterior al [H07 de bytes parciales](P093_public_h07_partial_bytes_2026-11-07.md), siguiente prioridad del STATE: la política que ADR-0016 dejaba pendiente («una intención sin testigo observable conserva recursos y su política sigue pendiente»). Contrato público sin cambios.

## Política implementada

- **Prueba del sistema operativo**: cada intención nativa registra `boot_session` (época del último arranque, vía sysinfo) al crearse; un journal anterior sin el campo se lee como 0 (compatibilidad). Un valor distinto del boot actual prueba que NINGÚN proceso ni handle de esa sesión sobrevive — es la única evidencia de cierre que no necesita testigo.
- **REGISTERED (intento reclamado)**: `finish_interrupted_system_restart` cierra el intento sin testigo con la misma transacción del cierre normal — `WORKER_INTERRUPTED` recuperable, marcador `attempt_closed.system_restart=(boot_anterior, boot_actual)`, recursos liberados exactos, intención cerrada. La recuperación ya marcó el trabajo `FAILED_RECOVERABLE` al arrancar; el operador reinicia con plan nuevo.
- **PREPARING (sin reclamar)**: `close_preparation_system_restart` retira la intención (CLOSED) sin tocar el trabajo — permanece QUEUED con su señal de despacho y sus recursos de admisión, y el consumidor lo prepara desde cero.
- **Misma sesión (testigo posiblemente vivo)**: sin prueba de reinicio, el comportamiento anterior se conserva — la recuperación intenta el testigo y, si no está, los recursos se RETIENEN (la ausencia de proceso/pipe no acredita cierre).
- Las dos puertas nuevas rechazan pruebas inválidas (boot 0 o igual al actual) con `transformation_system_restart_proof_invalid`.

## Aceptación dedicada (`transformation_publica_perdida_definitiva_del_testigo_y_redespacho_hasta_publicacion`)

Preparación real con helper vivo (boot actual registrado) → daemon y helper mueren sin reconciliar → reescritura declarada del journal que simula el reinicio (boot=1) → el daemon reiniciado retira la intención SIN testigo → el trabajo vuelve a despacharse: la admisión original selló su observación de destino en el arranque ANTERIOR y falla cerrada (`STALE_REVISION/transformation_destination_changed`) → el operador reinicia con plan nuevo → COMPLETED por publicación CAS. Recuentos [1,2,0,0], intención inicial CLOSED con boot 1 en su payload, original intacto, principals revocados (con reintento corto ante la contención transitoria de la revocación). [Informe](P093_runs/public-witness-lost-20261107/attempt5/native-public-witness-lost.json).

## Pruebas

- **SQL** (`transformation_plans` 77/77, tres casos nuevos): cierre sin testigo del intento registrado (recursos liberados, evento `attempt_closed_system_restart`, replay idempotente, pruebas inválidas rechazadas); retiro de la preparación con trabajo re-despachable; y la secuencia realista con la reescritura del journal (boot preparado actual → simulado antiguo). Igual-sesión sin prueba se retiene (test de preparación pre-política).
- **Validación completa**: [attempt5](P093_runs/public-witness-lost-20261107/attempt5/verification-synopsis.json) — **513 pruebas PASS / 0 FAIL / 0 ignoradas, 31/31 comprobaciones** (la 31ª nueva: public-witness-lost), auditor independiente PASS (896 entradas, 31 logs/recibos). Attempts 1–4 conservados: attempt1 (revocación sin reintento), attempt2 (base del auditor), attempt3 (ventana `backend_busy` del permiso del motor), attempt4 (recuento SQL del auditor 74→77).

## Límites conservados

Conversores GGUF/ONNX reales, aceptación de runtimes (llama.cpp/Ollama/Python) y la interfaz de escritorio para transformaciones siguen pendientes. P093 sigue `BLOCKED_REAL`/preparación sin `CODE_PASS` integral ni `REAL_PASS`; contrato 3.13.0 intacto.
