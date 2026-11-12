# ADR-0011 — Ownership de rutas durante cada hash de paquete

Estado: implementado. Fecha: 2026-10-28. P044/P022. Los resultados de aceptación se registran en el [informe del incremento](../reports/P044_optimized_search_2026-10-28.md).

Extensión de 2026-10-29: [ADR-0012](0012-rutas-compartidas-por-cualificacion.md) permite compartir los handles ya retenidos por una cualificación viva. La API pública de hash sigue creando un propietario por llamada; la extensión no conserva metadata ni digests.

## Contexto

El paquete de referencia contiene Python y sus dependencias fijadas. Cada checkpoint de IO volvía a recorrer todos los antecesores de cada archivo. La medida nativa optimizada muestra un coste dominante en esos recorridos: 37 verificaciones de paquete acumulan 47.836 ms frente a 1.126 ms de ejecutable/inputs. Son tiempos de fases, con solapamiento de operaciones concurrentes.

Se probó una consulta directa de atributos Windows: pasa las pruebas de equivalencia y reduce un microensayo de 73.510 a 51.400 µs, pero la aceptación sigue fallando a los 30 s de fuente, con cuatro de seis ingestas. Se retira esa implementación FFI y su excepción: el host vuelve a prohibir `unsafe` y P001 sigue admitiendo sólo los módulos de ADR-0009/0010. El resultado fallido y sus huellas se conservan.

## Decisión

`controlled_hash` crea un propietario de rutas nuevo para cada llamada. En Windows adquiere handles mediante `std::fs::OpenOptions` y las extensiones seguras Windows, desde la raíz hasta cada entrada. `FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS` permite comprobar el propio objeto; su metadata debe carecer de enlaces/reparse points. Los handles sólo comparten lectura: se deniegan escritura, reemplazo y borrado del objeto mientras dure ese hash. Un antecesor adquirido protege la búsqueda del hijo siguiente.

Los checkpoints siguientes reconocen los handles de los objetos bloqueados sin volver a abrir sus rutas, pero **releen la metadata de cada antecesor en cada checkpoint**. El permiso `FILE_WRITE_ATTRIBUTES` no queda excluido por `FILE_SHARE_WRITE`; puede convertir un directorio vacío en junction aunque siga abierto. Una prueba real reprodujo ese bypass en la implementación preliminar (FAIL conservado), y la lectura fresca lo rechaza. Se siguen llamando todos los checkpoints de límites, cancelación y hooks y se leen todos los bytes para el SHA canónico. El inventario se recorre de nuevo en cada hash; no se afirma que los handles sustituyan el recorrido ni impidan añadir entradas. Ningún atributo, handle o digest se conserva entre operaciones.

Se retienen como máximo 8.192 handles. Si se alcanza el límite o no se puede adquirir un objeto por permisos, compartición o limitaciones UNC, se cierran los handles y se usa la comprobación anterior completa en ese y todos los checkpoints siguientes. Una ruta inexistente nunca se conserva como comprobada. Los rechazos de rutas relativas, traversal y errores se mantienen. Fuera de Windows se conserva el recorrido anterior.

El propietario se destruye al devolver éxito, error o cancelación cooperativa. Los guards externos de cualificación, las mediciones antes/después de cada RPC y las comprobaciones del commit siguen siendo responsables de su propio alcance. Este propietario no autoriza RPC ni reutiliza una cualificación.

## Validación

Se comprueban bloqueo real de escritura/rename de archivo y antecesor, liberación al cerrar, fallback al límite sin pérdida de validación, junction real y descendientes existentes/inexistentes, sustitución después de un hash anterior y cancelación durante lectura con cierre de todos los handles. Se compara el SHA completo con el recorrido anterior. Las mediciones locales no sustituyen la aceptación P026/búsqueda con defaults.

La razón para retirar el intento de atributos se apoya también en las limitaciones de [GetFileAttributesW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileattributesw). Los flags y el cierre se aplican mediante las APIs seguras de [OpenOptionsExt](https://doc.rust-lang.org/std/os/windows/fs/trait.OpenOptionsExt.html). [CreateFileW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew) documenta que la compartición no afecta a los accesos a atributos; [MS-FSA FSCTL_SET_REPARSE_POINT](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4aeefef8-92c3-4abc-af7a-a610caf8a165) admite `FILE_WRITE_ATTRIBUTES` para modificar el reparse point. La prueba usa ese acceso mínimo, no un `GENERIC_WRITE` que el bloqueo ya denegaba.
