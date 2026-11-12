# Perfil de hardware

LARM sondea el equipo al arrancar el daemon (`--serve-api`) y cada vez que se pide un refresco, y guarda el resultado en `hardware_profile`. El sondeo solo lee: no instala drivers ni cambia nada del sistema. Cubre:

- sistema operativo, CPU y memoria (P010);
- GPU, NPU, drivers, las doce APIs de aceleración de la spec §22 y las DLL de runtime (P011).

## Consultarlo y refrescarlo

- **Leer**: `GET /api/v1/hardware` (scope `read`) devuelve `{items, next_cursor, snapshot_at}` con los perfiles del más antiguo al más reciente. Solo admite `cursor` y `limit` (1..=200, 50 por defecto); cualquier otro parámetro da 400 `filter_not_allowed`.
- **Refrescar**: `POST /api/v1/hardware/refresh` (scope `admin`, `Idempotency-Key` obligatoria), con cuerpo `{"resource_id": null, "profile_id": null}`, responde 202 con un `JobReceipt`. El estado se sigue en `GET /api/v1/jobs/{id}`, que puede ver quien lo pidió o un administrador.
  - Si ya hay un sondeo en curso, se devuelve ese job: nunca hay dos sondeos a la vez.
  - `resource_id` puede nombrar el perfil que se quiere refrescar (404 si no existe).
  - `profile_id` debe ser `null`.
  - Un job que quedó a medias porque el daemon murió pasa a FAILED con `job_interrupted` (reintentable) al volver a arrancar.
  - El sondeo de arranque es un job más, hecho por el actor `larm-daemon`.
- **CLI**: `larm-daemon hardware-probe` imprime el snapshot y su huella sin escribir nada; con `--record` también lo registra.
- **Escritorio**: pantalla **Hardware**. Cada dato muestra su fuente y si está verificado o es desconocido; las GPU y NPU aparecen con sus APIs operativas; hay una tabla de las doce APIs y el botón «Volver a sondear».

## Sistema, CPU y memoria

| Campo | Fuente |
|---|---|
| `os.edition`, `os.build` | Registro `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`: `EditionID`, y `CurrentBuild`.`UBR`. `ProductName` no sirve porque en Windows 11 sigue diciendo «Windows 10». |
| `os.architecture` | `PROCESSOR_ARCHITECTURE` del sistema (`HKLM\SYSTEM\…\Session Manager\Environment`), no la del proceso. |
| `cpu.vendor`, `cpu.model`, núcleos | sysinfo 0.39.6 (`GetSystemInfo`, CPUID y `GetLogicalProcessorInformationEx`). |
| `cpu.instruction_sets` | CPUID en ejecución (`is_x86_feature_detected!`, que también comprueba que el sistema guarde el estado AVX). Nombres de característica de LLVM/Rust: `avx2`, `avx512f`, `avxvnni`… Nunca se deducen del nombre del modelo. |
| `ram.physical_bytes` | RAM instalada en módulos: CIM `Win32_PhysicalMemory`, con un timeout de 10 s. |
| `ram.usable_bytes` | RAM que ve Windows (`GlobalMemoryStatusEx.ullTotalPhys`). |
| `metadata.pagefile.total_bytes` | `CommitLimit − PhysicalTotal` de `GetPerformanceInfo`. |
| `observed_metrics` | RAM libre y uso de CPU en el momento del sondeo. No forman parte de la huella. |

En una APU con memoria de vídeo reservada (carve-out), como el PC IA con 128 GiB instalados y 64 GiB para la GPU, `usable_bytes` es menor que `physical_bytes`. Esa diferencia pertenece a la GPU y no se cuenta como RAM. El pool de la RAM es `system-ram`.

## GPU y NPU

Todo lo que carga código de un driver o de un fabricante corre en un proceso hijo, `larm-daemon probe-accelerators` (ADR-0010). Tiene un timeout de 30 s y se mata si vence; si se cuelga o revienta, lo afectado queda desconocido y el daemon sigue.

| Campo | Fuente |
|---|---|
| GPU | DXGI (`EnumAdapters1`). Se descartan WARP y los adaptadores por software. Los adaptadores con la misma identidad PCI son una sola GPU: Windows la duplica para pantallas remotas o indirectas, y en el PC IA aparece dos veces con dos LUID. |
| NPU y presencia | Configuration Manager: dispositivos **presentes** de las clases Display y ComputeAccelerator. El registro de clases conserva entradas de dispositivos que ya no están, así que no basta. Una controladora PCI con problema que DXGI no ve aparece igual, sin APIs y con su código de problema. |
| `device_id` | `PCI\VEN_xxxx&DEV_xxxx&SUBSYS_xxxxxxxx&REV_xx`, estable entre arranques. |
| `vendor` | Identificador PCI (`1002` y `1022` son AMD, `10de` NVIDIA, `8086` Intel). |
| `luid` | LUID de sesión de DXGI; si es de esa GPU, el que usa Windows ML. No entra en la huella. |
| `driver_version` | `DriverVersion` en `HKLM\SYSTEM\CurrentControlSet\Control\Class\<clave del driver>`. |
| Memoria dedicada y compartida | `DXGI_ADAPTER_DESC1`. |
| `unified_pool_id` | `system-ram` cuando D3D12 informa `UMA`: la memoria compartida sale de la RAM del sistema y la admisión no debe contarla dos veces. |
| `backends` | Solo las APIs **verificadas** en ese dispositivo. |

## APIs de aceleración (`metadata.accelerators`)

Cada una de las doce APIs tiene una fila `{api, status, devices, version, components, evidence, reason}`. Detectado no es lo mismo que operativo.

- **VERIFIED**: una prueba real funcionó en un dispositivo concreto.
- **UNKNOWN**: hay componentes, pero falta la prueba o falló. `reason` dice por qué: `tool_missing`, `tool_timeout`, `not_verified`, `probe_failed`, `no_device`, `native_*`, `ambiguous_binding`, `not_loadable`…
- **ABSENT**: se buscaron sus DLL en System32, el `PATH` y las raíces de su SDK y no están.

| API | Prueba |
|---|---|
| Vulkan | `vulkaninfo --summary` enumera la GPU (vendor y device ID). |
| OpenCL | `clinfo` enumera la GPU (vendor y nombre). |
| DirectML | Se crea un `IDMLDevice` sobre un `ID3D12Device` de la GPU. |
| Windows ML | Se crea un `LearningModelDevice` DirectX; queda ligado por su LUID. |
| ROCm (HIP) | `hipGetDeviceCount` con `amdhip64*.dll` de System32 o `%HIP_PATH%\bin`. |
| CUDA, SYCL, Vitis AI, MIGraphX, OpenVINO, QNN, TensorRT | Solo búsqueda: si están, quedan UNKNOWN `not_verified` hasta que exista una prueba. |

En DirectML, Windows ML y HIP manda la prueba: aunque la búsqueda de DLL no las encuentre, si el dispositivo se crea son VERIFIED.

## Dependencias de runtime

Cada DLL encontrada entra en `snapshot.dependencies` con su versión de fichero y su sha256, y por tanto en la huella: una actualización de DirectML o del runtime HIP crea una configuración nueva.

El daemon lleva un manifiesto de compatibilidad con Windows 10/11. Sin él, Windows aplica el «version lie» y versiones de fichero como la 10.0.3652.0 de `amdhip64_6.dll` se leían como 6.2.3652.0.

## Datos desconocidos

Un dato que no se puede observar queda `null`, y `metadata.unknown` explica el motivo: `{field, reason, detail}`. Entre los motivos:

- `wmi_*`;
- `registry_unreadable`;
- `os_api_unavailable`;
- `native_*` (el hijo no respondió, falló, devolvió basura o es de otra versión);
- `dxgi_failed`, `pnp_failed`;
- `file_unreadable`.

Si los dispositivos no se pudieron enumerar, el escritorio lo dice y no afirma que no haya GPU.

## Huella (`hardware_fingerprint`)

sha256 del JSON canónico RFC 8785 del snapshot, en el orden del contrato 04: `instruction_sets` ordenadas; `devices` por kind+device_id+vendor; `dependencies` por name+version+hash; `backends` ordenados. Quedan fuera `observed_metrics` y, por dispositivo, `luid` y `evidence_refs`.

La versión del cálculo es `fingerprint_version = 1` (subirla si cambia qué entra en la huella o cómo se canoniza). Cada configuración nueva guarda en `metadata.fingerprint` `{version, previous_profile_id, previous_fingerprint, changes}`, donde `changes` lista los componentes estables que cambiaron (`os.build`, `devices[GPU:…].driver_version`, `dependencies[directml.dll].version`…) con su valor anterior y el nuevo. La pantalla Hardware los muestra.

Un sondeo con la misma huella que el perfil más reciente actualiza ese perfil (`row_version`+1 y nuevo `probed_at`). Si la huella cambia (otra build de Windows, otro driver, otra DLL de runtime, otra RAM…), se crea un perfil nuevo y el anterior se conserva como historia.

## Estado en el PC IA (2026-10-11)

- **Sistema**: Windows Professional 26200.9457 AMD64.
- **CPU**: AMD RYZEN AI MAX+ 395, 16 núcleos y 32 hilos, AVX-512 incluido.
- **RAM**: 128 GiB instalados, 63,6 GiB visibles; pagefile de 4 GiB.
- **GPU**: AMD Radeon 8060S, driver 32.0.21030.2001, con 64 GiB dedicados y 47,6 GiB compartidos (UMA). APIs verificadas: Vulkan 1.4.315, OpenCL 2.1, DirectML 1.15.5, Windows ML y HIP 6.4.50101.
- **NPU**: AMD NPU Compute Accelerator, driver 32.0.20101.3760. Sin ninguna API verificada: Vitis AI no está instalado.
- **APIs ausentes**: CUDA, SYCL, Vitis AI, MIGraphX, OpenVINO, QNN y TensorRT.

Ningún dato quedó desconocido. Las comprobaciones frente a oráculos independientes están en [docs/reports/P010_runs](../reports/P010_runs/) y [docs/reports/P011_runs](../reports/P011_runs/).
