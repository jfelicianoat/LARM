# ADR-0012 — Compartir rutas dentro de una cualificación viva

Estado: implementado y regresiones PASS; la aceptación TCP se registra en el [informe del incremento](../reports/P044_remote_tcp_2026-10-29.md). Fecha: 2026-10-29. Extiende [ADR-0011](0011-bloqueo-rutas-durante-hash.md).

## Contexto

La primera búsqueda real de seis modelos por TCP supera el límite productivo de 30 s. En el intento 3 se completan sólo dos ingestas antes de HTTP 504. Dentro de la consulta, 35 verificaciones del paquete acumulan 30.247 ms y seis adquisiciones de pins, 5.776 ms; las fases concurrentes se solapan. El hash del ejecutable tarda 29–50 ms y no justifica conservar un digest. La evidencia está en el [informe TCP](../reports/P044_remote_tcp_2026-10-29.md).

## Decisión

La cualificación ya retiene los archivos de paquete, inputs inmutables y ejecutable hasta terminar su IO. `PinnedInputs` conserva ahora también el propietario de rutas usado para adquirirlos. Cada medición clona su mapa con `Arc<File>`: comparte handles vivos, nunca metadata, inventarios ni SHA. Las mediciones concurrentes tienen su propio mapa y no se serializan mediante un mutex.

Cada checkpoint sigue recorriendo todos los antecesores y leyendo metadata fresca de cada handle. Se conserva el rechazo real de `FILE_WRITE_ATTRIBUTES`/reparse points, la apertura padre antes de hijo y los mismos flags seguros y restricciones de compartición. Cada hash vuelve a enumerar el inventario y lee todos los bytes. `Policy.authorize`, los hashes completos de ejecutable e inputs, los grants y el resto del contexto siguen midiéndose. No se autoriza IO por un resultado anterior.

El límite de 8.192 handles y el fallback al recorrido original siguen activos. Una lista independiente conserva cada pin de bytes adquirido aunque el mapa de rutas se vacíe al entrar en fallback. El último propietario cierra cada handle. Si se cancela un observador durante una medición bloqueante, sus owners viven hasta terminar esa medición; no se libera la admisión prematuramente. Las funciones públicas de hash/publicación conservan su propietario nuevo por llamada. No hay caché global, FFI nueva ni excepción a la prohibición de `unsafe`.

## Validación exigida

Bloqueo y liberación reales de archivo/antecesor tras clonar y cerrar owners; equivalencia del SHA completo con el SDK; detección de una entrada añadida al inventario; reparse point creado con `FILE_WRITE_ATTRIBUTES` después de adquirir el handle; retención de pins de bytes tras fallback; cancelación, cambios de contexto y remedir antes de commit. Las regresiones y el ensayo TCP con seis modelos y 30 s deben registrar sus resultados. Una mejora local o un build correcto no acredita el recorrido TCP.

La versión final pasa 24 pruebas internas, 18 de cualificación, 16 de runtime y siete de fuente, con cero fallos/ignoradas. Pasan también Clippy de todos los targets del host/daemon, formato, política P001 y 78 copias contractuales. El cierre Windows adicional se valida en ADR-0013. [Recibo de regresiones](../reports/P044_runs/remote-tcp-acquisition-20261029/scoped-final-sessions.command.json).
