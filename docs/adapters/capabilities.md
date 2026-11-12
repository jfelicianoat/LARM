# Descriptores de capacidad en los SDK

P026 incorpora `CapabilityRegistry` en Rust y Python. Es una estructura privada de
autoría del paquete; no añade DTOs ni campos al protocolo público. La capacidad
debe tener mapping y estimador implementados por el handler, schemas de entrada y
salida, versiones/hashes y una fixture. Tener un descriptor válido no concede
cualificación, confianza, admisión ni promoción.

El bundle es un objeto cerrado con `schema_version: "1.0"` y `entries` (1–128).
Cada entrada tiene exactamente:

| Campo | Significado |
|---|---|
| `id`, `version` | Identidad canónica y versión de dos o tres componentes numéricos |
| `input_schema`, `output_schema` | JSON Schema local y autocontenido, hasta 1 MiB cada uno |
| `input_sha256`, `output_sha256` | SHA-256 de los schemas serializados mediante RFC 8785 |
| `mapping` | Clave del mapping que el handler declara implementado |
| `estimator_id` | Clave de una función real registrada por el handler |
| `fixture` | Objeto cerrado `input`/`output`, ambos válidos contra sus schemas |
| `fixture_sha256` | SHA-256 RFC 8785 de ese objeto |

Se rechazan campos extra, IDs repetidos, hashes o fixtures alterados, referencias
externas, mapping ausente y estimador inexistente. Las capacidades desconocidas
necesitan namespace; los alias publicados sólo se aceptan al leer, nunca al
anunciar. Para las 18 capacidades publicadas deben coincidir exactamente versión
y schemas canónicos; no se puede llamar `embedding.create` a un echo arbitrario.
`tools/p026_capability_schemas.py` genera sus 36 schemas, índice y hashes embebidos
en Rust a partir de los contratos canónicos. Python comprueba también hashes de
las fuentes del índice. El host sigue verificando el hash del paquete completo.

El servidor de ambos SDK exige un `CapabilityRegistry` antes de aceptar un
handshake que anuncie capacidades. Rust lo obtiene de `AdapterHandler::capability_registry`
(también reenviado por `ModelSourceAdapter`); Python de `handler.registry`. El
servidor llama `check_advertised` después de validar versión e identidad. Cada
capacidad necesita descriptor y estimador ejecutable sobre su fixture;
la salida del estimador debe cumplir `MemoryEstimate`. Valores desconocidos siguen
siendo `null` con confianza `UNKNOWN`. Esta comprobación no mide ni limita la
memoria del proceso y no sustituye la admisión de recursos de Core.

En cada operación, validar el input antes del mapping/IO y el output antes de
devolverlo. La referencia de protocolo usa `estimate` para validar la entrada y
ejecutar su estimador; después usa `validate(..., "output", ...)`. Rust ofrece
`input`, `output` y `estimate`. Los errores de schema no incluyen los valores
privados que fallaron. `schema_id` genera una URN con identidad, versión y hash.

Los dos SDK calculan el mismo hash del perfil, ordenado por ID. Los tests usan un
corpus compartido y ocho vectores RFC 8785 con floats, extremos y orden UTF-16.
Python usa `rfc8785==0.1.4`, fijado con hash, licencia y copia en el paquete. Se
conserva el límite de enteros del dominio (valor absoluto ≤ 2^53); se rechazan
NaN, infinitos, surrogates aislados y claves no string. El fixture de embedding
del corpus sólo comprueba contratos: ningún test afirma haber ejecutado un modelo.

La cualificación completa debe ejecutar el comportamiento real, ligar paquete,
schemas, mapping, estimador y fixture, y probar los fallos de cada familia antes
de que P024 permita promoción. El registro está conectado a las cinco referencias.
`tools/p026_reference_capabilities.py check` fija los cinco descriptores source y
el descriptor runtime echo; los testigos source sólo prueban schema y no son
recibos de jobs ejecutados. Las referencias funcionales conservan sus pruebas de IO.

Python ofrece `open_session()` opcional: el servidor lo invoca una sola vez tras
validar major, identidad y todos los estimadores, antes de admitir operaciones de
familia. Source, transformation y benchmark abren ahí sus locks y bases privadas;
un handshake rechazado no crea estado ni ejecuta recuperación. Fallar la apertura
de sesión no autoriza callbacks ni expone excepciones privadas. Un manifiesto con
capacidades vacías es válido para un launcher sin soporte de inferencia.
P026 permanece en curso hasta completar harness y cualificación.
