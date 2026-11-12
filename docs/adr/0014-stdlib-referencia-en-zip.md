# ADR-0014 — Biblioteca Python de referencia en un ZIP determinista

Estado: implementado; equivalencia/imports PASS; la aceptación de las cuatro familias y TCP se registra en el [informe](../reports/P044_remote_tcp_2026-10-29.md). Fecha: 2026-10-29.

## Contexto

El intento TCP 4 conserva HTTP504/30s: mejora los pins y completa tres ingestas frente a dos, pero la verificación repetida de muchos archivos sigue dominando. Su mediana de verificación de paquete es888ms; las 37 mediciones suman33.300ms con solapamiento. Los checks y hashes siguen siendo completos; no se amplía el plazo ni se altera la búsqueda de seis modelos.

El Python local fijado3.14.6 aporta586 archivos de stdlib retenidos,11.666.934bytes. De ellos498 archivos/9.717.442bytes pertenecen a grupos de código puro. El mismo constructor de intérprete se usa en los paquetes de referencia source/protocol/transformation/benchmark.

## Decisión

Los grupos de primer nivel que sólo contienen `.py`/`.pyc` se guardan en `python314.zip`, mediante ZIP_STORED, fechas y atributos estables, orden determinista y directorios explícitos. No se añade ninguna exclusión a las siete del constructor anterior. Se conserva cada byte seleccionado. Los grupos con cualquier recurso u otro archivo permanecen íntegros en `Lib`, incluidos sus módulos; así mantienen su layout para cargas relativas a `__file__` y extensiones nativas. Se conservan también directorios originalmente vacíos, DLLs, executable, licencias, site-packages y todos los metadatos/versiones fijados. Los enlaces/entradas no regulares del árbol se rechazan.

La ruta aislada `._pth` incorpora el archivo antes de `Lib`. Las cuatro familias usan el mismo configurador para mantener ese archivo, SDK y módulo de familia. Un valor de familia fuera del conjunto cerrado se rechaza antes de escribir. El bill de dependencias y los métodos/permisos no cambian. No se cambia el runtime del operador ni se descarga o instala Python.

El [importador ZIP de Python](https://docs.python.org/3.14/library/zipimport.html) admite módulos y paquetes de código, y excluye extensiones dinámicas. El [empaquetado Windows](https://docs.python.org/3.14/using/windows.html#the-embeddable-package) contempla una biblioteca ZIP y rutas aisladas. Son soporte del formato, no una prueba de equivalencia de este paquete: ésta se comprueba localmente.

Los mismos hashes/recorridos del host siguen leyendo todos los archivos y todos los bytes del archivo ZIP, con atributos frescos y guards actuales. El ZIP permanece almacenado sin compresión. No se conserva un digest de paquete ni se omite una cualificación. Cambia el paquete físico y su SHA; la aceptación debe emitir una cualificación nueva sobre ese paquete y ejecutar las recetas originales.

## Evidencia y límites

Cuatro pruebas PASS21,187s: payload idéntico a una copia independiente mediante el `copytree` anterior; recursos/binarios conservados; ZIP reproducible pese a timestamps; rutas de las cuatro familias; ejecución real aislada con JSON cargado desde ZIP, recursos en disco, DLLs y nueve distribuciones fijadas. [Recibo](../reports/P044_runs/remote-tcp-acquisition-20261029/stdlib-package-tests.command.json). El SHA del archivo stdlib real es `52f5b1d06db875bcf4e19bff805e70ef4f99be781c2334c93b36a1b5501e007a`.

No se considera acreditado TCP, GUI, upstream ni las cuatro familias por este smoke. Deben repetirse el recorrido TCP original y las recetas nativas de las familias afectadas. El código Rust/ejecutables no cambia; su igualdad con los del intento4 se comprueba antes de reutilizarlos.
