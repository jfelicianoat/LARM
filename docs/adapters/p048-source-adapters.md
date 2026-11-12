# Adaptadores de fuentes P048

P048 incorpora un worker Rust común con perfiles aislados para GitHub Releases, una URL HTTP explícita, una carpeta local y una carpeta UNC privada. El manifiesto instalado limita el acceso de red y filesystem de cada perfil. El worker no modifica la fuente, no escribe en el destino de descarga y no crea trabajos: entrega bloques de hasta 512 KiB al Download Manager.

| Perfil | Comportamiento | Límites |
| --- | --- | --- |
| `GITHUB` | Lee releases y activos de un solo `owner/repo` configurado. `source.search` busca dentro de ese repositorio y `source.sync` conserva su inventario. | No ofrece una búsqueda universal de GitHub. Pagina hasta 10 páginas de 100 releases; si el catálogo excede ese límite, devuelve un error reintentable en vez de un inventario parcial. Solo consulta publicaciones públicas mientras P045/P141 no habiliten credenciales privadas. |
| `HTTP` | Inspecciona por `HEAD` una URL fijada por el usuario; usa `Content-Length`, ETag, `Last-Modified` y el SHA-256 configurado cuando existe. `source.auth_probe` admite Basic desde Credential Manager para una fuente HTTPS vinculada a una cuenta; envía `HEAD` al URL configurado y no sigue redirecciones. | La URL debe pertenecer a un origen autorizado. Las redirecciones HTTP se rechazan. HTTP sin `Content-Length` queda `UNKNOWN`. Las operaciones autenticadas de manifiesto y descarga aún requieren completar sus gates consumidores P045/P051. |
| `LOCAL` | Escanea una raíz local mediante P017 y vuelve a comprobar raíz, ancestros, reparse points y ruta relativa al leer cada bloque. | El tamaño observado por P017 es `ESTIMATED`; el worker no vuelve a recorrer y hashear pesos completos. |
| `UNC_PRIVATE` | Usa el escáner P017 bajo la identidad integrada de Windows y limita la espera de accesos de red. | Solo es aceptable con una share y una identidad cualificadas. Sin acceso real, devuelve error retryable o `auth_required`; P045/P141 gobiernan credenciales adicionales. |

Los tamaños mantienen su procedencia: `EXACT` requiere tamaño y SHA-256 publicados o configurados; `HIGH` indica tamaño de fuente sin hash; `ESTIMATED` identifica observaciones locales de P017; `UNKNOWN` no inventa bytes. P052 sigue calculando destino, espacio libre, presupuesto y confirmación explícita. El adaptador no convierte una estimación en confirmación.

El SDK trata `source.search` como marcador de operación de búsqueda del perfil source. `SearchSupport` valida sus filtros; no necesita un estimador de memoria de modelo. El resto de capacidades ejecutables sigue exigiendo sus descriptores y estimadores. La búsqueda GitHub queda así disponible mediante el worker sin anunciar soporte de runtime que no implementa.

Las rutas `source.download` se rechazan con `UNSUPPORTED_CAPABILITY`: la transferencia pertenece al Download Manager por medio de `source.download_chunk`. El adaptador valida rangos, límites de cuerpo, ETag y origen de los redireccionamientos de activos GitHub. Git LFS permanece fuera del paquete: resolver punteros con Git o hooks externos necesita una interfaz de permisos y cualificación propia.

El perfil HTTP representa una sola URL. Su plan conserva el `variant_id` del
catálogo central y devuelve la revisión, el manifiesto y el hash de ese origen;
el consumidor vuelve a cotejarlos con el plan aprobado antes de transferir.
Los perfiles con varios modelos mantienen su resolución explícita de identidad,
y las selecciones opcionales que no se puedan mapear se rechazan. Cada lectura
de bloque emite progreso de inicio/fin ligado al ID de la llamada. HTTP/GitHub
atienden la cancelación mientras esperan red, cerrando ese futuro de lectura;
no se descartan futuros con tareas de filesystem bloqueantes.

La [receta HTTP de conformidad](conformance.md) ejercita el ejecutable original
con datos propios, un servidor loopback y fallos HTTP externos al worker. Se
mantiene separada de la receta que inspecciona el ledger privado de referencia.
La descarga completa del daemon, una fuente remota privada y la GUI requieren
sus aceptaciones específicas.

La cualificación combina fixtures negativas de traversal, symlink escape, UNC no disponible, límites/rangos HTTP, HTTP sin tamaño, rate limit GitHub y hash/tamaño de assets; además ejecuta el worker Windows empaquetado contra una carpeta P017 temporal, y una consulta pública GitHub sin credenciales ni descarga de activos. El sondeo Basic usa un servicio local de loopback para probar 200, 401, 403 y rechazo de redirecciones; la fuente privada real y la lectura positiva de UNC siguen pendientes de autorización y de los gates P043/P045/P051/P141.
