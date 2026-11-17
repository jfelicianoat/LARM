# Revisión contractual 3.15.0 — hosts de ejecución (P081)

Fecha: 2026-11-14. Predecesora:3.14.0. Publicación e implementación autorizadas expresamente por el operador al responder «Autorizar revisión e implementación» a la propuesta3.15.0. Contrato normativo; no acredita por sí mismo implementación ni REAL_PASS.

## Problema comprobado

La especificación §5.1 exige distinguir cómo se ejecuta un runtime en Windows (`WINDOWS_NATIVE`, `WINDOWS_PYTHON`, `WINDOWS_ML`, `WSL2`, `CONTAINER`, `EXTERNAL_LOCAL`) y §52 declara las estrategias `WSL` y `CONTAINER` de instalación. El dominio ya tiene el enumerado `INSTALLATION_STRATEGIES` con esos valores, pero no existe un modelo de host de ejecución: la tabla `external_host` cubre sólo motores adoptados con `base_url` HTTP (`EXTERNAL_LOCAL`), un runtime Linux bajo WSL no puede referenciarse desde una instalación, y ninguna operación expone los motores virtualizados observados en el equipo. Un runtime WSL aparecería engañosamente como nativo.

Fuentes: `LARM_SPEC_V1.md` §5.1 y §52; `contracts/09_EXTERNAL_PROFILES.md`; `contracts/api-operations.json`; `schemas/domain.schema.json`, `runtime_installation`. El contrato00 exige resolver con el operador los cambios de alcance, conservando los requisitos.

## DTO nuevo y reglas normativas

Se añade el DTO `runtime_execution_host` (registro de motores virtualizados observados read-only) y el campo opcional `execution_host_id` en `runtime_installation`:

```json
{
  "runtime_execution_host": {
    "id": "uuid-v7",
    "created_at": "RFC3339", "updated_at": "RFC3339", "row_version": 1,
    "metadata": "object",
    "kind": "WSL2 | CONTAINER",
    "name": "nombre del distro/engine (1..256)",
    "engine_version": "string|null",
    "wsl_version": "1|2|null",
    "default": "boolean",
    "status": "ONLINE | OFFLINE | UNKNOWN",
    "evidence_refs": "array de strings"
  }
}
```

Reglas:

1. **Detección read-only**: la observación de WSL (`--status`, `--list --verbose`) y del motor de contenedores (`--version`, `info --format`) nunca instala distros, habilita features de Windows ni muta el estado global del motor. Todo comando de sondeo corre con timeout, kill al vencer, entorno mínimo sin credenciales y salida acotada; la ausencia de una herramienta es una observación válida, no un error.
2. **Plan de ejecución prohibido**: `wsl --install/--uninstall/--update/--set-*/--manage`, `dism`/`wusa`/`netsh`, habilitaciones por PowerShell y verbos de motor que mutan estado (`pull`/`push`/`build`/`save`/`load`/`import`/`commit`) o ejecutan con `--privileged`/`--network host` se rechazan con `execution_host_plan_mutates_host`.
3. **Coherencia instalación↔host**: estrategia `WSL` exige `execution_host_id` con host `WSL2`; `CONTAINER` exige host `CONTAINER`; las estrategias nativas exigen `execution_host_id` null. Violación → `execution_host_reference_missing` / `execution_host_strategy_mismatch`.
4. **Política de puertos**: toda exposición de puerto del huésped se declara explícita y bind sólo loopback (`execution_host_port_must_bind_loopback`); `0` (efímero) prohibido.
5. **Mapeo de rutas**: Windows↔WSL explícito bajo `\\wsl.localhost\<distro>\` (lectura tolera el legado `\\wsl$\`), rutas absolutas y sin subida de directorio; montajes de contenedor con host drive y ruta absoluta POSIX.
6. **Operación de lectura**: `GET /api/v1/execution-hosts` (scope `read`) devuelve `runtime_execution_hostList` sin crear ni promover nada; el registro no acredita compatibilidad ni habilita ejecución.

## Evidencia de implementación (anexa, no contractual)

- Dominio: `ExecutionHostKind` con `allows_strategy`; validación instalación↔host 2×2.
- Hardware: sondeo read-only real (WSL2 Ubuntu v2 y Docker 29.7.2 en el PC IA), parsers tolerantes al locale y a UTF-16LE, ausencia de binarios como observación válida.
- Runtime: mapeo ida/vuelta WSL, política loopback, comando prohibido por clase, 11 pruebas.
- DB/API/UI: migración, repos con journal/evento/outbox, referencia en el DTO sellado, operación GET y pantalla de runtimes.
