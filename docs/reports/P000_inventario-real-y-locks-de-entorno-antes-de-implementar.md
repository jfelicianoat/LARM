# P000 — Inventario real y locks de entorno antes de implementar

**Revalidado**: 2026-10-16 · **Host**: A9_MEGA (PC IA, Windows 11 Pro 26200) · **Estado**: **BLOCKED_REAL** — 15 PASS, 0 FAIL, 1 prueba real bloqueada, 2 bloqueos de entorno. [evidence.json](P000_inventario-real-y-locks-de-entorno-antes-de-implementar.evidence.json) (valida contra `PromptEvidence`).

La credencial del Broker facilitada por el operador se enroló mediante la API P009 en Windows Credential Manager. `tools/check_integration_credentials.py --broker-only` recupera exclusivamente esa entrada y verifica `auth/check`: HTTP 200, `authenticated=true`, `auth_required=true`. No imprime el token ni sigue redirecciones. La nueva captura registra `VERIFIED_HTTP_200_AUTHENTICATED_AUTH_REQUIRED`; se conservan las pruebas de captura y determinismo.

Regenerar: `.venv-tools/Scripts/python tools/p000_evidence.py` (venv de herramientas con `jsonschema==4.26.0`, `referencing==0.36.2`, las versiones del paquete).

## Revalidación del trabajo heredado (2026-10-10)

El informe anterior declaraba CODE_PASS 18/18. Al volver a ejecutarlo:

| Hallazgo | Efecto | Corrección |
|---|---|---|
| `verify_p000.py` daba **FAIL 17/18**: TypeScript se bajó de 7.0.2 a 6.0.3 por compatibilidad con typescript-eslint y el lock se quedó sin `integrity` | lock incompleto | integrity tomado de registry.npmjs.org y cruzado con `package-lock.json` (coinciden) |
| La evidencia no seguía `larm.schema.json#/$defs/PromptEvidence` (status `PASS`, campos libres) y el verificador la sobrescribía al ejecutarse | evidencia inválida | `tools/evidence.py` común: ejecuta cada prueba, guarda la transcripción con sha256 y calcula el estado con la regla del contrato 00 |
| `vramBytes = 4293918720`: es la saturación de `Win32_VideoController.AdapterRAM` (uint32), no la VRAM | dato falso presentado como medido | VRAM leída de `HardwareInformation.qwMemorySize` por el enlace exacto `Enum\<PNPDeviceID>\Driver`: **68719476736 (64 GiB)**; el detector de defaults rechaza ahora el valor saturado |
| Volúmenes registrados con el GUID de **partición** GPT, no con el Windows Volume GUID que pide §28 | identidad equivocada (coincidían aquí por casualidad) | captura de `Get-Volume.UniqueId` (`\\?\Volume{…}\`) con el GUID de partición aparte; aparece también la partición Recovery |
| Fixture `onnx_pipeline_smoke` = `Xenova/tiny-random-LlamaForCausalLM@69acc9dd`: esa revisión **no contiene ningún .onnx** | fixture inútil para su propósito | sustituido por `onnx-community/tiny-random-LlamaForCausalLM-ONNX@6c740230` (`onnx/model.onnx`, 41285208 B, sha256 declarado `2e9b85c0…`) |
| Broker capabilities marcado NO_DISPONIBLE «sin credencial» | dato omitido: el endpoint es público (Client API 2.11) | capturado: 200, `contract_version` 2.11, sha256 del cuerpo en la captura cruda |
| Redirección de la consola en página OEM: la captura no era UTF-8 válido | la normalización fallaba con bytes no ASCII | el script escribe el fichero en UTF-8 sin BOM (`-OutFile`) |
| La prueba de determinismo normalizaba **la misma** captura dos veces (tautología) | oráculo sin poder de detección | dos capturas reales independientes → mismo hash `21be0dbc…` |

Comprobado y válido tal cual: pins de Qwen3-0.6B-GGUF `23749fef…` (apache-2.0) y whisper.cpp `5359861c…` (MIT) contra la API de HF; tags `b11539`→`10a60cf3…` y `b11538`→`79e2e74e…` contra la API de GitHub; 4 fixtures del paquete; 39 dependencias usadas coinciden en versión y checksum con `Cargo.lock`/`package-lock.json`.

## Datos reales del PC IA

AMD Ryzen AI MAX+ 395 · 8×16 GiB (128 GiB físicos), 63,6 GiB visibles para Windows y 64 GiB de carve-out como VRAM de la Radeon 8060S (driver 32.0.21030.2001) · NPU `VEN_1022&DEV_17F0` estado OK · volúmenes C: `fb4f4fbc-…` y Recovery `ac318884-…` (sin letra, de sistema) · Broker local en 127.0.0.1:8765 · HTTPS registrado para HF, GitHub, ModelScope, Kaggle, npm y crates (status observado, sin relleno).

## Pruebas

| ID | Tipo | Resultado |
|---|---|---|
| P000-CONTRACT-json / secrets / package-fixtures / llamacpp-two-versions / acceptance-volumes / environment-lock / lock-coherence | CONTRACT | PASS |
| P000-FAULT-unknown-default (4 mutantes, incluido uint32 saturado) / schema-invalid (3 claves) | FAULT | PASS |
| P000-REAL-capture-pcia-a / -b / capture-deterministic / https-topology / broker-capabilities | REAL | PASS |
| P000-REAL-broker-auth-check | REAL | PASS: credencial autorizada por el operador desde Credential Manager; HTTP 200 con autenticación exigida y válida |
| P000-REAL-capture-pc-principal | REAL | BLOCKED: esta sesión solo accede al PC IA; el script es portable |

## Pendientes para REAL_PASS

1. Ejecutar `tools/capture_inventory.ps1 -OutFile <ruta>` en el PC principal.
2. Segundo volumen escribible con GUID propio (G04/G05): segundo disco o VHDX montado, a decisión del operador.

`docs/reports/P000_verify_output.json` es la salida del verificador anterior y queda sustituida por esta evidencia.
