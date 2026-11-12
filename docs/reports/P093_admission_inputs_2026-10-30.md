# P093 — admisión inicial y archivos de entrada protegidos

Snapshot histórico del incremento anterior a la autorización3.13.0. La revisión fue autorizada y publicada después; [estado posterior y pruebas](P093_restart_3_13_0_2026-10-30.md). Los recibos de este informe conservan su contrato3.12.1 y sus fuentes originales.

Fecha: 2026-10-30. Incremento de preparación; objetivo global activo. P093 sigue BLOCKED_REAL. No se declara CODE_PASS integral, conversión de un modelo ni aceptación GUI. El contrato publicado permanece en3.12.1 y la propuesta3.13.0 de reinicio continúa pendiente de autorización.

## Comportamiento incorporado

`SqlTransformationPlans.activate_plan` acepta una confirmación inicial en una transacción. Vuelve a comprobar propietario, sello, caducidad, recibo/perfil P026 y guard físico del paquete. Contrasta la identidad/revisión del bundle, miembros seleccionados y las ubicaciones locales observadas. El hash upstream puede ser desconocido: para un archivo local verificado exige el hash y tamaño de su ubicación física. Los nombres lógicos del bundle y las rutas físicas del CAS se tratan por separado.

Toma locks exclusivos de variante y artefactos de entrada, y de un UUID reservado para la futura variante de salida. Los volúmenes se bloquean en modo compartido y se deduplican cuando varias raíces pertenecen al mismo disco. La admisión conserva el original y no registra ninguna variante de salida: el job queda QUEUED, attempt0 y output_variant_idNULL. El claim que inicia realmente el primer intento sigue pendiente.

Después de las comprobaciones costosas, el proveedor interno obtiene una medida reciente de espacio. Se cotejan los volúmenes y totales sellados, margen vigente, mínimo por volumen, reservas de otros trabajos y presupuesto. Un tamaño UNKNOWN sin presupuesto devuelve UNKNOWN_SIZE. El presupuesto se comprueba contra el pico extra previo a ejecutar; el control de bytes durante el futuro worker todavía debe implementarse.

Job, journal del job, consumo del plan, reservas, locks, aprobación de un uso, recibo idempotente y evento/outbox se confirman juntos. El replay pertenece al actor y contrasta tanto el hash HTTP como el contenido canónico de ExecutePlan; no crea otro trabajo ni exige repetir una acción ya aceptada. El recibo sobrevive a reapertura SQLite aunque ya no estén disponibles el worker o el input. Una clave diferente no puede volver a consumir ese plan.

Un negativo reproduce una aceptación parcial si SQLite omite una escritura obligatoria: el primer caso suprime el INSERT del job y el método anterior respondía con éxito. Se conserva [RED](P093_runs/admission-inputs-20261030/red-suppressed-writes.log). Ahora se comprueban las filas insertadas de job/journal/idempotencia y el conjunto exacto de locks y reservas persistidos. [GREEN](P093_runs/admission-inputs-20261030/green-suppressed-writes.log) cubre las cinco omisiones y exige rollback completo.

`PinnedReadFile` reutiliza el propietario de rutas Windows del host, manteniendo handles de archivo y ancestros sin compartir escritura/borrado. Una inspección sin handles retenidos no satisface esta API: el fallback se rechaza. `VerifiedTransformationInput` protege todos los archivos seleccionados antes de medirlos, comprueba tamaño, SHA256 y EOF mediante lecturas acotadas, y mantiene sus propietarios durante la admisión. También admite archivos vacíos. La adquisición se realiza fuera del ejecutor asíncrono; los propietarios pertenecen al trabajo de lectura hasta que éste termina. No concede cualificación a un adaptador ni resuelve por sí sola la identidad del volumen.

## Verificación del incremento

La verificación final termina PASS:11/11 comprobaciones,237 pruebas PASS,0 FAIL y0 ignoradas. Dominio172; planes SQLite20, perfiles7, guard13 y reservas3; recuperación heredada4; contrato HTTP3; archivos reales9; regresión de rutas6. Los casos filtrados no se cuentan como ejecutados ni ignorados. Clippy de los cuatro crates afectados con todos sus targets, compilación de comprobación del daemon, formato, arquitectura17 y copias78/78 PASS. No se ha generado un nuevo ejecutable release ni repetido el gate global.

Se conservan comandos, logs y hashes en [admission-inputs-20261030](P093_runs/admission-inputs-20261030/). El [resumen](P093_runs/admission-inputs-20261030/verification-synopsis.json) recoge34 inputs idénticos durante todas las comprobaciones; una lectura posterior independiente confirma esos34 hashes y los11 logs. El wrapper termina con salida0, sesión12101. La consulta final de metadatos de procesos encuentra0 ejecutables propios `larm-*.exe` bajo el repositorio, sin leer argumentos ni credenciales. Los intentos RED/GREEN anteriores conservan su alcance y no se suman de nuevo al total final.

Las pruebas SQL usan cualificación y observaciones de input deliberadamente simuladas. Las pruebas de archivos sí abren, leen y protegen archivos locales reales de Windows, pero emplean descriptores de catálogo de fixture. Ninguno de estos dos grupos por separado demuestra el recorrido catálogo→daemon→worker→publicación.

El primer intento físico sin elevación termina con3 tests del host y5 de input PASS, y1 FAIL al preparar una junction: Windows devuelve Acceso denegado antes de llamar al verificador de LARM. El mismo caso pasa con los permisos de ejecución necesarios, sin cambiar su código ni quitar la aserción. La verificación final vuelve a ejecutar el grupo completo. No se convierte el fallo inicial en PASS ni se ignora el caso.

## Apertura de la ventana

Tras la confirmación del operador de que Windows estaba desbloqueado, se intentó abrir el ejecutable de escritorio existente. El controlador devolvió `GetCursorPos failed: Acceso denegado. (0x80070005)`; se refrescó la lista de ventanas y el único reintento devolvió lo mismo. No se cambió ninguna configuración de Windows ni se obtuvo una nueva aceptación del producto. [Observación registrada](P093_runs/admission-inputs-20261030/desktop-retry-observation.json).

## Trabajo pendiente

El servicio público todavía responde que el motor no está disponible. Faltan el selector físico del catálogo con resolución actual por volumen, el productor de planes de un worker cualificado y el proveedor de espacio de producción. También faltan claim/fencing, ejecución y cancelación supervisadas, liberación coordinada de recursos, staging, verificación/smoke y publicación con provenance. El guard de entrada de esta fase protege la admisión; al iniciar un job encolado habrá que volver a verificar y proteger sus inputs.

La recuperación heredada no constituye todavía la recuperación de estos nuevos recursos de ejecución: antes de habilitar la composición real debe enlazarse con el cierre probado del worker y los journals de plan/job. No se liberan locks o reservas por asumir que un proceso ha terminado.

La admisión FAT32 usa de momento una cota conservadora del total frente al máximo de un archivo,4GiB−1. No demuestra compatibilidad con outputs fragmentados cuyo agregado supere ese máximo. Debe sustituirse por el desglose sellado por archivo y volumen antes de aceptar esa combinación de filesystem y transformación.

No se publica la ruta de reinicio propuesta sin autorización explícita. P094 sigue necesitando herramientas llama.cpp cualificadas, conversión/cuantización GGUF reales y load/smoke; estos tests no las sustituyen. Las aceptaciones de GUI del incremento anterior y el gate global anterior conservan sus propios snapshots históricos.
