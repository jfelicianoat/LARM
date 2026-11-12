# P046 — Adaptador source de Hugging Face

P046 añade un worker portable para descubrir modelos públicos de Hugging Face, inspeccionar revisiones fijadas y entregar bloques acotados al gestor de descargas. El worker no escribe modelos en los destinos del usuario ni ejecuta descargas por su cuenta; el flujo usa `source.download_chunk` para que P051 valide y publique los bytes. `source.download` falla cerrado.

La búsqueda paginada acepta texto y deja las capacidades para el postfiltro de Core. Los formatos reconocidos se separan por variante y carpeta: GGUF, safetensors, PyTorch y ONNX. Una revisión usa el SHA de commit upstream y un manifiesto RFC 8785. Si el mismo commit vuelve con otro manifiesto, el adaptador devuelve `INTEGRITY_FAILED`.

Los tamaños indican su evidencia por artefacto. `EXACT` requiere tamaño y SHA-256 publicados; `HIGH` conserva tamaños de metadata/LFS o de `Content-Length` obtenido por `HEAD`; `UNKNOWN` mantiene bytes y límites nulos. El fallback `HEAD` solo informa `size_estimate` y no cambia el hash del manifiesto. `blob_id` no se interpreta como SHA-256. El gestor vuelve a verificar todos los bytes al terminar.

El paquete portable contiene Python fijado, el SDK y los schemas del contrato. El smoke test inicia el worker aislado, valida el handshake y ejecuta un `source.auth_probe` anónimo. El perfil declara orígenes HTTPS exactos para Hugging Face y sus endpoints oficiales de archivos; se rechazan otros redirects y las URL firmadas no se devuelven ni se escriben en errores.

## Verificación

- 19 pruebas focales pasan con validación de request/response contra los schemas source. Cubren paginación y cursors alterados, secretos en metadata, formatos y carpetas múltiples, revisiones inmutables, tamaños exactos/altos/desconocidos, `HEAD`, gated/private, errores 429, redirects y rangos reanudables de hasta 512 KiB.
- La compilación y el smoke test del paquete portable pasan; el worker aislado completa handshake y `source.auth_probe` anónimo.
- La búsqueda pública real `q=llama`, `limit=1`, sin credenciales ni descarga, completó correctamente y devolvió dos resultados. La evidencia formal registra 3 PASS, 0 FAIL y 0 pruebas bloqueadas.
- P046 permanece `BLOCKED_REAL` porque P025, P043 y P045 no tienen `REAL_PASS`; la autenticación privada y gated también requiere el resolver P045/P141. El perfil conserva `REQUIRES_REAL_QUALIFICATION`.
- La puerta común inicial en sandbox registró `cargo_test` y `package_copy` fallidos: tres pruebas HTTP declarativas no alcanzaron loopback y el perfil local aún difería del paquete. Después, la suite declarativa pasó 7/7 con loopback; `cargo test --workspace -j 1 -- --test-threads=1` completó con código 0, y `package_copy` pasó 58/58 tras sincronizar la revisión 3.4.0. El verificador del paquete pasó sus 3100 checks y `validate_evidence.py` validó todas las evidencias. La invocación común elevada se detuvo tras 15 minutos dentro de `cargo_test`, por lo que no existe un recibo único de gate 14/14; las puertas y la suite se verificaron por separado.

La selección opcional de P052 y sus cuatro niveles/tipos de tamaño están definidos por la revisión contractual 3.1.0; P051 3.3.0 define el transporte opcional por bloques y P046 3.4.0 declara esa capacidad en el perfil Hugging Face. El preflight de LARM no inicia un trabajo: P052 requiere confirmación explícita del operador antes de crear la descarga. El adaptador source no suplanta esa confirmación ni acepta destinos antes del preflight de Core.

La evidencia ejecutable está en [P046 evidence](P046_hugging-face-source-adapter.evidence.json), con transcripciones y hashes en `P046_runs/`. La revisión del paquete está en [REVISION_3_4_0_P046](../contracts/REVISION_3_4_0_P046.md); la búsqueda pública ya está cualificada en esta sesión, pero el flujo privado y las dependencias source siguen pendientes.
