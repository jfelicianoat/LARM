# P017 — Content addressed store, deduplicación e importación

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**. Todas las pruebas pasan, incluida la real (6 pasos más la regresión de P041), pero sus dependencias P016 y P041 no tienen REAL_PASS. [evidence.json](P017_content-addressed-store-deduplicacion-e-importacion.evidence.json) · [operación](../operations/import-cas.md)

## Qué se construyó

- **CAS** (`larm-storage/src/cas.rs`):
  - SHA-256 en streaming;
  - caché por tamaño + mtime, que solo evita releer y nunca cuenta como verificación;
  - layout `blobs/sha256/ab/<hash>`;
  - copia por staging con el hash releído, colocación sin sobrescribir y blob de solo lectura;
  - sin duplicar un blob que ya existe;
  - vista para runtimes con hard links y, si no se puede enlazar, referencias separadas.
- **Recorrido** (`larm-storage/src/import.rs`): determinista, con cabeceras acotadas; no sigue enlaces ni junctions.
- **Escáner** (`larm-models/src/scan.rs`):
  - reconoce GGUF (con lectura acotada de la arquitectura y el nombre de la cabecera), SafeTensors, índices, ONNX y external_data, PyTorch (el pickle se marca, nunca se abre), configuración, tokenizador, procesador, Diffusers, MLC, manifiestos de Ollama y documentación;
  - agrupa en variantes: shards, mmproj y cuantización;
  - ignora lo demás y lo informa con su motivo.
- **Importación** (`larm-models/src/import.rs`):
  - **ADOPT**: `EXTERNAL` o `HOST_MANAGED`, con `registration_mode` `ADOPTED`, sin tocar nada;
  - **COPY**: al CAS, `LARM_MANAGED`, o `SHARED` si el blob se comparte;
  - registro con las reglas de identidad de P041;
  - idempotente.
- **Repositorio**: alta idempotente de `artifact_location`, marcado `SHARED` e informe de duplicados por hash con el ahorro posible en el mismo volumen.
- **Revisiones**: una revisión `CURRENT` puede sustituir a la anterior (que pasa a `SUPERSEDED`) cuando lo pide la fuente, como una carpeta local cuyo contenido cambia. Por defecto sigue siendo un conflicto.
- **Daemon**: comandos `models-import-dir`, `storage-duplicates` y `runtime-view`.

## Defecto de diseño encontrado por las pruebas

Reimportar una carpeta cuyo contenido había cambiado chocaba con `current_revision_exists`: la revisión `content:` nueva y la `CURRENT` anterior eran de la misma distribución.

Ahora la importación local pide la sustitución. Revisión nueva y anterior, que pasa a `SUPERSEDED`, se escriben en una transacción, como manda el contrato 01 para las revisiones verificadas.

## Decisiones

- **Hard links**: solo para la vista de runtimes, sobre blobs inmutables y gestionados por LARM. Enlazar un fichero `EXTERNAL` dentro del CAS no es seguro, porque el usuario podría modificarlo y cambiaría el blob. Si no se puede enlazar (otro volumen, recurso de red, FAT), la vista lleva referencias separadas a los blobs, sin copiarlos.
- **`registration_mode`**: no tiene columna en `model_variant` ni en `artifact_location`, así que va en su metadata. El contrato 01 lo separa de la propiedad.
- **Sin API propia**: P017 no tiene operaciones en `api-operations.json`. `POST /models/import` es de P098 y el análisis de almacenamiento, de P018. La función se ofrece con comandos del daemon.
- **Fuente de la importación**: hay que indicar una fuente registrada; no hay ninguna «carpeta local» predefinida en el contrato. Las fuentes son de P042.

## Pruebas

| Prueba del prompt | Dónde |
|---|---|
| Duplicate hash | `duplicados_por_hash_sin_borrar`, `entre_volumenes…` y real (`storage-duplicates`) |
| Hardlink fallback | Unitaria (enlaces en el mismo volumen) y real (C: con `st_nlink` 2; `\\localhost\Temp` con referencias, «cross-device link») |
| Scan mixed folder | `carpeta_mixta_adoptada_sin_tocar_nada` y real |
| Unknown file ignored with report | Unitaria del escáner, integración y real (`.exe`, `.txt`, `.gguf` sin magia) |

**Negativos**:

- carpeta fuera de la ubicación;
- propiedad inválida al adoptar;
- carpeta inexistente;
- hash falso al copiar (la copia se descarta);
- fichero que cambia mientras se hashea;
- junction no seguida;
- nombre ya ocupado en la vista con otro contenido.

**Mutaciones**: cada una se aplicó y se restauró el código. Las 9 hacen fallar al menos una prueba:

1. caché solo por tamaño;
2. copia sin verificar;
3. blob escribible;
4. desconocidos registrados;
5. sin marcar compartidos;
6. ahorro entre volúmenes;
7. la caché cuenta como verificación;
8. se siguen junctions;
9. adopción fuera de la ubicación.

La 7 no se detectaba al principio; se reforzó la prueba.

## Prueba real (`tools/p017_real.py`, 6 pasos PASS)

Se ejecutó con el daemon en el PC IA. Oráculos: hashlib, `os.stat` (`st_nlink` y `st_mtime_ns`) y el atributo de solo lectura.

1. **Adopción de una carpeta mixta**:
   - 5 variantes y 12 filas `EXTERNAL`/`ADOPTED`;
   - `.exe`, `.txt` y `.gguf` sin magia, ignorados e informados;
   - pickle marcado;
   - ni un byte ni una mtime cambiados.
2. **Reimportación**: 10 ficheros desde la caché, 0 releídos y 0 filas nuevas.
3. **Duplicados**: el Q8 en dos carpetas, con 921 707 bytes de ahorro posible; nada borrado.
4. **Copia al CAS**:
   - 9 blobs verificados y de solo lectura;
   - mmproj y README guardados una sola vez (4 filas `SHARED`);
   - original intacto;
   - repetir no copia.
5. **Vista para el runtime**:
   - en C:, 3 hard links;
   - en `\\localhost\Temp`, referencias en `view.json` sin copiar ningún blob.

## Pendiente

- **Importadores restantes y reconstrucción sin base de datos**: P098 (CT2, NCNN, MNN y sidecars de manifiesto).
- **Limpieza de duplicados y análisis de almacenamiento**: P018.
- **Interfaz para el ahorro posible**: la de la pantalla de almacenamiento.
