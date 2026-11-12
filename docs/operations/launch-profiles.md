# Parámetros de lanzamiento y su hash

Un perfil de lanzamiento (spec §56) se guarda como `LaunchParameters` y se identifica por `profile_hash`. El CRUD de perfiles y el arranque real llegan con P034; P013 fija el formato y el hash.

## Qué incluye el hash

`profile_hash` es el sha256 del JSON canónico RFC 8785 de los parámetros **efectivos**:

- hilos, contexto, batch, concurrencia y capas en GPU;
- backend, tokenizador y plantilla de chat;
- generación (temperatura, top_p, semilla y máximo de tokens);
- `memory_limit_bytes`, `runtime_arguments`, `adapter_options`;
- el entorno: `allowlist`, más las variables con su valor no secreto o su referencia de credencial `larm/…`.

Los secretos nunca entran en el hash, ni en las respuestas ni en los logs: solo su referencia. Rotar un secreto no cambia el hash; para invalidar evidencia previa hace falta una nueva revisión técnica del entorno (contrato 11).

Cambia el hash si cambia un argumento, el orden de los argumentos, una opción, una variable, una referencia, el límite de memoria o un parámetro de generación. Reordenar las claves de un objeto no lo cambia.

Los números siguen `Number.prototype.toString` de ECMAScript, como manda RFC 8785: `1.0` se escribe `1`, `0.000001` se escribe `0.000001` y `1e21` se escribe `1e+21`. Un entero mayor que 2^53 se rechaza porque un doble no lo representa exactamente; los tamaños viajan como string decimal.

## Reglas (contrato 11, H03–H04)

Se rechazan antes de cualquier IO, con su motivo:

| Motivo | Caso |
|---|---|
| `memory_limit_zero` | `memory_limit_bytes` igual a 0. Debe ser positivo o `null` (sin límite adicional). |
| `env_name_invalid` | Nombre fuera de `[A-Za-z_][A-Za-z0-9_]*`. |
| `allowlist_duplicate`, `env_duplicate` | Nombres repetidos, también si solo cambian las mayúsculas: Windows no las distingue. |
| `env_not_allowed` | Variable que no está en la `allowlist`. |
| `credential_reference_invalid` | Referencia fuera de `larm/`. |
| `generation_out_of_range` | Temperatura fuera de 0..2, top_p fuera de 0..1 o `max_output_tokens` igual a 0. |

Una variable con valor y referencia a la vez no cumple el schema.

Los perfiles builtin materializan `null`, `[]`, `{allowlist: [], variables: []}` y `{}`. El límite efectivo es el mínimo de los límites no nulos del perfil y de `Constraints.max_memory_bytes`.

## Validar un fichero

```text
larm-daemon launch-profile-hash --file perfil.json
```

Imprime `{"valid": true, "profile_hash": "…"}`, o `{"valid": false, "reason": "…", "detail": "…"}` y sale con 1.
