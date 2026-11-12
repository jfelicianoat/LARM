# Ollama runtime worker — incremento de inspección física

El worker implementa `runtime.inspect_model` mediante el SDK NDJSON publicado.
Recibe la ruta de un manifiesto situado bajo `manifests/` de un almacén explícito
en el perfil local del worker. Devuelve `ModelInspection`; no crea identidades
de catálogo, bundles, instalaciones ni promociones en Core.

Comprueba tamaños y SHA256 de config, capas y manifiestos anidados, mantiene los
roles y rutas relativas al almacén, deduplica blobs y rechaza incoherencias,
metadatos remotos, enlaces/reparse points, overflow y cancelación. En Windows
retiene handles de ancestros sin compartir delete y del archivo sin compartir
write/delete durante la lectura. La cabecera GGUF v2/v3 se contrasta cuando el
config declara GGUF; no se certifica que el modelo cargue ni que su backend sirva.

`architecture` permanece null: el `architecture=amd64` del config pertenece a
la imagen contenedora. No se infieren capacidades, contexto ni aceptación de
licencia; los archivos de licencia forman parte de los artefactos inspeccionados.

El paquete no pide permisos de escritura, red, ejecución de procesos ni secretos.
No copia blobs ni abre una instancia de Ollama. La ejecución de producción debe
pasar por los grants, RootMap y cualificación del host; los checks de ruta del
worker no son aislamiento del SO. No se incluyen pesos ni binarios de Ollama en
el paquete. Python/dependencias son los fijados por el repositorio.

Los demás métodos runtime obligatorios devuelven un error normalizado
`UNSUPPORTED_CAPABILITY`; los opcionales no se anuncian. No está implementado
todavía el adaptador P070 completo: faltan inventario central HOST_MANAGED,
descubrimiento/versionado/backend, carga/ejecución/leases y acciones MANAGED
según ownership/policy. P099 y la aceptación integral continúan pendientes.
