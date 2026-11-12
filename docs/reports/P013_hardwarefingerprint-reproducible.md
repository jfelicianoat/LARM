# P013 — HardwareFingerprint reproducible

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 23/23 PASS (3 reales); solo bloqueado porque sus dependencias P010, P011 y P012 no tienen REAL_PASS. [evidence.json](P013_hardwarefingerprint-reproducible.evidence.json) · [huella](../operations/hardware.md) · [parámetros de lanzamiento](../operations/launch-profiles.md)

## Qué se construyó

- **JSON canónico completo (RFC 8785)**:
  - Antes, `canonicalize` rechazaba los números no enteros. Ahora implementa `Number.prototype.toString` de ECMAScript: dígitos mínimos y notación exponencial fuera de [1e-6, 1e21).
  - Se valida con los vectores del apéndice B de la RFC.
  - Un entero mayor que 2^53 se rechaza en vez de alterarse en silencio.
  - Hacía falta porque `LaunchParameters.generation` lleva `temperature` y `top_p`.
- **Huella de hardware**:
  - Fija `fingerprint_version = 1`. Los campos y el orden canónico del contrato 04 ya estaban implementados en P010–P011.
  - Calcula el diff de componentes estables entre dos snapshots: dispositivos por `kind:device_id` y dependencias por nombre, de modo que reordenar no es un cambio.
  - Cada configuración nueva guarda `metadata.fingerprint` con `version`, el perfil y la huella anteriores, y los cambios. La misma configuración conserva el diff que se calculó al aparecer.
  - La pantalla Hardware muestra la versión y los cambios.
- **H03–H04, parámetros de lanzamiento** (`larm-domain/src/launch.rs`):
  - `LaunchParameters`, `Generation`, `EnvironmentConfig` y `EnvironmentBinding` con la forma exacta del schema;
  - `profile_hash`, sha256 del JSON RFC 8785 de los parámetros efectivos, con referencias de credencial y nunca secretos;
  - las reglas del contrato 11;
  - perfiles builtin;
  - límite efectivo, el mínimo de los no nulos.

  Comando `larm-daemon launch-profile-hash --file` para validar un perfil.
- **Corrección de P107**: `launch_profile_hash` de la rebanada Broker usaba `serde_json::to_string` (no canónico). Ahora usa `profile_hash` o, si el perfil no es un `LaunchParameters`, el sha256 RFC 8785.

## Oráculos

| Prompt | Resultado |
|---|---|
| Mismo input, mismo hash | PASS: huella de hardware y `profile_hash` |
| Orden distinto, mismo hash | PASS: dispositivos, ISA y claves de objeto reordenadas |
| Cambio de driver, hash distinto | PASS: el diff nombra `devices[GPU:…].driver_version` |
| Cambio de RAM libre, hash igual | PASS: sin diff |
| Contrato 11: un cambio exclusivo de argumentos, opciones, entorno o límite cambia el hash; reordenar propiedades no | PASS en unidad y frente al oráculo Python |

## Prueba real

- **`launch`**: `larm-daemon launch-profile-hash` coincide con una implementación JCS independiente en Python, con sus propios números ECMAScript. Cubre:
  - el fixture, que lleva 0,7, 1,0, 0,000001, 1e21 y claves con ñ y €;
  - nueve variantes de un único cambio cada una, todas con un hash distinto;
  - la reordenación de todas las claves, con el mismo hash;
  - cinco rechazos con su motivo.
- **`hardware`**: la huella del PC IA es la misma en dos procesos con distinta RAM libre. Recalculada en Python, coincide. Otro driver la cambia; RAM libre, LUID y orden no.
- **`serve`**: con el daemon real, el primer perfil tiene versión 1 y no tiene anterior; tras rearrancar es el mismo perfil, con `row_version` 2 y el diff conservado.

## Pendiente

- Un cambio de driver real en el PC IA no se ha provocado: instalar drivers está fuera de lo autorizado. El cambio se prueba con snapshots tipados.
- Validar `runtime_arguments` y `adapter_options` contra los esquemas del perfil cualificado del adaptador, y resolver credenciales en el worker: P034.
- ExecutionFingerprint (§76, contrato 05) con sus componentes: P110–P112.
