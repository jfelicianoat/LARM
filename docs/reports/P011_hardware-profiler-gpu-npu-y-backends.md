# P011 — Hardware Profiler: GPU, NPU y backends

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 32/32 PASS (6 reales); solo bloqueado porque su dependencia P010 no tiene REAL_PASS. [evidence.json](P011_hardware-profiler-gpu-npu-y-backends.evidence.json) · [operación](../operations/hardware.md) · [ADR-0010](../adr/0010-excepcion-unsafe-sondeo-nativo.md)

## Qué se construyó

- **Sondeo nativo en un proceso hijo** (`larm-daemon probe-accelerators`, ADR-0010). Hace todo lo que carga código del driver o de un fabricante, con timeout de 30 s y muerte del proceso si vence:
  - DXGI: adaptadores, LUID, IDs PCI y memoria;
  - D3D12: dispositivo real y UMA;
  - DirectML: `IDMLDevice` real, con la DLL cargada en ejecución;
  - HIP: `hipGetDeviceCount`;
  - Windows ML: `LearningModelDevice` DirectX;
  - Configuration Manager: dispositivos presentes de las clases Display y ComputeAccelerator;
  - `VS_FIXEDFILEINFO` de las DLL encontradas.

  El informe del hijo tiene versión y un schema cerrado; un hijo inexistente, colgado, roto, que devuelve basura o que es de otra versión deja lo afectado UNKNOWN con su motivo.
- **Herramientas con timeout**: `vulkaninfo --summary` y `clinfo`, con analizadores probados contra sus salidas reales.
- **Búsqueda de componentes**: las DLL de las doce APIs de la spec §22 en System32, el `PATH` y las raíces de su SDK, con sha256.
- **Montaje puro** (`accel.rs`), con fixtures:
  - GPU físicas: DXGI sin WARP; los adaptadores con la misma identidad PCI se unen en una.
  - NPU a partir de los dispositivos PnP presentes.
  - Drivers desde `Control\Class`.
  - Matriz VERIFIED/UNKNOWN/ABSENT con evidencia y motivo.
  - En cada dispositivo, `backends` solo con las APIs verificadas en él.
  - Las DLL encontradas van a `dependencies`, y por tanto a la huella.
- **`POST /api/v1/hardware/refresh`** (scope `admin`, `Idempotency-Key`, `Probe`):
  - responde 202 con `JobReceipt`;
  - el job se guarda en `operation_journal` (`kind=hardware.refresh`);
  - si ya hay un refresco en curso, se reutiliza su job;
  - el estado se consulta en `GET /jobs/{id}`, visible para su autor o un administrador;
  - un job interrumpido por un reinicio pasa a FAILED `job_interrupted`.

  El sondeo de arranque usa el mismo camino.
- **Escritorio**:
  - tarjetas de GPU y NPU con su driver, su memoria (con nota de memoria unificada) y las APIs que funcionan;
  - tabla de las doce APIs, con su estado en texto;
  - componentes de runtime;
  - botón «Volver a sondear», que sigue el job y avisa si falta permiso o si el sondeo falla.

## Defectos encontrados al probar

| Defecto | Cómo apareció | Corrección |
|---|---|---|
| Versión de fichero falseada: `amdhip64_6.dll` 10.0.3652.0 y `winml.dll` 10.0.26100.1150 se leían como 6.2.x | El oráculo `VersionInfo` no coincidía con LARM | «Version lie» de Windows para procesos sin manifiesto de compatibilidad. Se añade `larm-daemon.manifest` (Windows 10/11, `asInvoker`), incrustado por `build.rs`. |
| Una API demostrada por su prueba nativa salía ABSENT si la búsqueda de DLL no la encontraba | Al diseñar la prueba con `SystemRoot` y `PATH` vacíos | En DirectML, Windows ML y HIP manda la prueba nativa. Hay una prueba unitaria y otra real (`sin-dll`). |
| La GPU aparecía dos veces | Sondeo real: la pantalla remota (RDP) duplica el adaptador DXGI con otro LUID | Se une por identidad PCI y se conservan los dos LUID como evidencia. |
| WARP sin marca de software | Sondeo real: un «Microsoft Basic Render Driver» llega con `Flags=0` | Se excluye por su identidad (1414:008C). |
| 25 adaptadores de pantalla remota fantasma en el registro de clases | Lectura del registro | La presencia sale de Configuration Manager, no del registro. |
| `environment.lock.json` sin `windows-sys` 0.61.2 (dependencia de P009) | `lock-coherence` solo comprobaba las dependencias que ya estaban en el lock | Se añade la entrada. Ahora toda dependencia fijada en el workspace debe estar en el lock, y una prueba de mutación lo confirma. |

## Prueba real en el PC IA

`tools/p011_real.py windows`: 16 comprobaciones contra oráculos implementados aparte, todas iguales.

- **Dispositivos y drivers**: los mismos que `Get-PnpDevice -PresentOnly` con `DEVPKEY_Device_DriverVersion`. GPU con driver 32.0.21030.2001; NPU con 32.0.20101.3760.
- **Memoria de la GPU**: la dedicada (64 GiB) coincide con `HardwareInformation.qwMemorySize` del registro, leído con `winreg`. UMA verificada, con 47,6 GiB compartidos del pool `system-ram`.
- **APIs** comprobadas con ctypes propios:
  - Vulkan: `vkEnumeratePhysicalDevices` ve 1002:1586, igual que LARM.
  - OpenCL: `clGetDeviceIDs(GPU)` cuenta 1.
  - HIP: `hipGetDeviceCount` cuenta 1; LARM informa la versión 6.4.50101.
  - DirectML y Windows ML: verificadas creando un dispositivo.
- **Ausencias**: CUDA, QNN y OpenVINO no aparecen en una búsqueda propia, igual que en LARM.
- **Dependencias**: 5 DLL con sha256 (`hashlib`) y versión (`VersionInfo`) idénticos.
- **Huella**:
  - reproducible entre sondeos;
  - recalculada en Python (RFC 8785 + sha256) da el mismo valor;
  - otro LUID, otro orden o métricas distintas no la cambian; otro driver sí.

`sin-dll`: con `SystemRoot` y `PATH` apuntando a carpetas vacías, las nueve APIs que solo se buscan quedan ABSENT. DirectML, Windows ML y HIP siguen VERIFIED, `dependencies` queda vacío y la huella cambia.

`serve`, con el daemon real:

- 202 `JobReceipt`; una segunda petición durante el sondeo devuelve el mismo job;
- replay idempotente, y 409 con la misma clave y otros bytes;
- el job termina COMPLETED con un perfil de 2 dispositivos y 12 APIs, conforme a `hardware_profileList` y `JobState`;
- un lector recibe 403 al refrescar y 404 al pedir un job ajeno;
- matando el daemon con un job encolado, al volver ese job está FAILED `job_interrupted` y es reintentable.

## Estado del PC IA

| API | Estado |
|---|---|
| Vulkan | VERIFIED (1.4.315) |
| ROCm (HIP) | VERIFIED (6.4.50101) |
| DirectML | VERIFIED (1.15.5) |
| Windows ML | VERIFIED |
| OpenCL | VERIFIED (2.1 AMD-APP 3652.0) |
| CUDA, SYCL, Vitis AI, MIGraphX, OpenVINO, QNN, TensorRT | ABSENT |

La NPU se detecta con su driver, pero no tiene ninguna API verificada: Vitis AI (Ryzen AI Software) no está instalado. Instalarlo es decisión del operador y no lo hace el sondeo.

## Pendiente

- Prueba de dispositivo para CUDA, SYCL, OpenVINO, QNN, TensorRT, MIGraphX y Vitis AI. Hoy, si están instalados, quedan UNKNOWN `not_verified`. Los adaptadores de runtime (P020+) aportarán sus smokes.
- Reproducibilidad completa de la huella entre arranques y equipos (P013).
- Métricas de §100 (CPU, RAM, GPU, NPU…) y endpoint Prometheus: corresponden a su prompt de observabilidad.
