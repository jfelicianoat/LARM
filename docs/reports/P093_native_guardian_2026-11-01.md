# P021/P093 — supervisor nativo recuperable

Fecha: 2026-11-01. Contrato público 3.13.0 autorizado e intacto. Objetivo global de completar LARM activo. Componente interno verificado; P093 continúa en preparación/`BLOCKED_REAL`, sin `CODE_PASS` integral ni `REAL_PASS`.

## Cambio y alcance

Se añade un modo privado del binario real, `worker-guardian`. El proceso dedicado se incorpora a un Job Object exterior antes de ejecutar código del adaptador. Sus hijos heredan esa pertenencia desde su creación, incluidos los que nacen antes de la asignación al Job Object interior del Worker. Conserva ambos controles, el proceso Worker y los supervisores reales de salida, stdout y stderr. Esta contención exterior se aplica al helper nuevo; los consumidores existentes todavía usan su ruta anterior.

La pérdida del stdin del propietario solicita la terminación de los miembros propios del objeto nativo. Una consulta indica `CLOSED` únicamente cuando el Worker ha drenado realmente sus canales y la consulta al objeto exterior confirma que solo queda el supervisor. Cada consulta repite esas comprobaciones. El supervisor permanece vivo después del cierre, de modo que otro propietario puede reconectarse; `RELEASE` se permite tras ese cierre y desactiva kill-on-close solo al comprobar de nuevo que no quedan hijos.

El canal reutiliza el named pipe restringido al SID del operador y SYSTEM, con rechazo de clientes remotos e instancia de servidor retenida entre conexiones. Cada respuesta firma con Ed25519 un reto nuevo, la acción, el descriptor completo, el estado y los PIDs diagnósticos. El descriptor persistible contiene versión interna, sesión, fence y clave pública. La semilla privada y el entorno pasan por stdin heredado; no se escriben en argumentos, logs ni archivos. La lectura inicial y los mensajes tienen límites de tamaño y plazo. La resolución del SID usa el ejecutable del sistema por ruta explícita y sin ventana.

Ni un descriptor guardado, un PID, una firma antigua, un timeout ni la ausencia del pipe acreditan cierre. Si muere el supervisor, kill-on-close inicia la terminación, pero la observación falla y no autoriza liberar recursos.

Este helper es una primitiva de proceso P021. **No concede P022, no implementa `TransformationAttemptGuard`, no muta SQLite y no habilita la API de transformaciones.** Falta enlazar el origen servidor de la clave y el descriptor con la cualificación y el journal antes de ejecutar, transportar los RPC tipados y componer la recuperación del intento. El servicio público conserva `worker_unavailable`. No debe envolverse un descriptor suministrado por un cliente como si fuese autoridad de un intento.

La decisión y sus límites se documentan en [ADR-0015](../adr/0015-supervisor-nativo-recuperable.md). No se añade código `unsafe`, dependencia nueva ni excepción a la política existente.

## Pruebas nativas y fallo conservado

Se ejecutan el helper real y una fixture de adaptador con tres escritores: uno con stdout/stderr heredados, otro sin esos canales y un nieto creado por un proceso intermedio. Mantienen archivos exclusivos abiertos y siguen escribiendo hasta su terminación.

1. Se mata el propietario de prueba por su handle real, se descarta el observador y se carga únicamente el descriptor público guardado. La reconexión consulta al supervisor vivo, espera el cierre físico, comprueba los tres procesos escritores ausentes, abre sus archivos de forma exclusiva y verifica que dejaron de crecer. El supervisor se retira después de liberar su testigo.
2. Se rechazan otro fence, otra clave, una retirada anticipada y tres frames inválidos. El supervisor sigue atendiendo. Con el propietario vivo, se solicita cierre, se comprueban los archivos y la retirada termina con código 0 observado por el propietario.
3. Se termina el supervisor usando el handle conservado de ese proceso propio. Los archivos se liberan por kill-on-close, pero el descriptor y el observador siguen fallando: la desaparición del testigo no se transforma en una prueba válida.

Son tres casos del componente con un propietario de prueba Python y una fixture de familia benchmark. **No son H07 completo con caída del servidor API y un job de transformación cualificado.** Ningún caso libera recursos P093. El conjunto conserva nueve archivos de escritores, sus tamaños y SHA, descriptores públicos, manifiestos, binarios de fixture y logs.

El [intento 1](P093_runs/guardian-native-20261101/attempt1/verification-synopsis.json) falló correctamente al añadir la comprobación de retirada: el helper había cerrado el árbol/IO, pero una lectura no cancelable de `tokio::io::stdin` impedía terminar el runtime mientras el propietario mantenía el canal abierto. Pasaron siete unitarias y dos de los tres casos nativos; las comprobaciones posteriores no se ejecutaron. Se conserva su [log](P093_runs/guardian-native-20261101/attempt1/verify-native-guardian.log).

Se corrige con un lector dedicado y acotado de entrada que avisa por canales; el runtime no espera esa lectura bloqueante al retirarse. El intento final comprueba tanto EOF por caída del propietario como salida limpia con él todavía vivo. Las siete unitarias cubren firma, nonce/fence/acción/estado, firmante incorrecto, descriptores cerrados, límites, EOF, bootstrap completo, clave y manifiesto.

## Resultado final congelado

**26/26 comprobaciones PASS, 387 pruebas PASS, 0 FAIL, 0 ignoradas.** Wrapper y auditor independiente terminan con código 0. Se contrastan 866 entradas de código/contrato y 26 logs/recibos, sin cambios de fuente durante la ejecución.

| Grupo | Pruebas correctas |
|---|---:|
| Protocolo, firmas y bootstrap del supervisor | 7 |
| Supervisor y propietario reales | 3 |
| Regresión completa del Worker del host | 18 |
| Regresión anterior de propiedad del daemon, dominio, API, SQL, transformación y CAS | 359 |
| Total | 387 |

Clippy de dominio, DB, transform, host y daemon, todos los targets y warnings denegados; formato; política de `unsafe`; arquitectura de 17 crates y 79 copias contractuales: PASS. El ejemplo nativo se compila dentro del conjunto antes de copiarlo a las fixtures. La auditoría contrasta los binarios y archivos retenidos, reconstruye sus hashes y preserva los límites de alcance.

También vuelve a pasar P026 de referencia (176 casos funcionales, 16 wire, 17 observaciones y 9 gates físicos) y el recorrido directo Core/SQLite/CAS: tres intentos, trece eventos, diecisiete negativos de publicación, un derivado registrado, original intacto y cero reservas/locks finales. Esa regresión conserva catálogo, preview y ámbito de fixture; no acredita un conversor o runtime GGUF/ONNX real. No hubo cambios ni aceptación GUI en este incremento.

- [Resumen final, intento 2](P093_runs/guardian-native-20261101/attempt2/verification-synopsis.json).
- [Auditoría independiente](P093_runs/guardian-native-20261101/attempt2/post-run-audit.json).
- [Caída del propietario](P093_runs/guardian-native-20261101/attempt2/native-worker-guardian-owner-crash.json), [clientes inválidos](P093_runs/guardian-native-20261101/attempt2/native-worker-guardian-invalid-clients.json) y [caída del supervisor](P093_runs/guardian-native-20261101/attempt2/native-worker-guardian-guardian-crash.json).
- [Regresión de propiedad del daemon](P093_runs/guardian-native-20261101/attempt2/native-daemon-owner.json) y [transformación/CAS](P093_runs/guardian-native-20261101/attempt2/native-transformation.json).
- [Wrapper congelado](P093_runs/guardian-native-20261101/attempt2/frozen-verify.py) y [auditor congelado](P093_runs/guardian-native-20261101/attempt2/frozen-audit.py).

Siguiente paso: enlazar este propietario con P022, la identidad de ejecución y el journal del intento antes de ejecutar código, incorporar el transporte tipado y probar H07 con caída/reinicio del daemon real y conservación/liberación exacta de recursos. Siguen pendientes la política tras pérdida del supervisor/reinicio del sistema, el productor/consumidor público, reservas por fase/volumen, retención privada, reconstrucción de procedencia, FAT32 completo, conversores/hooks reales y UI. El desarrollo global de LARM continúa pendiente.
