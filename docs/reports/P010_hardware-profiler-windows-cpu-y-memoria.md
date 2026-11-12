# P010 — Hardware Profiler: Windows, CPU y memoria

**Fecha**: 2026-10-11 · **Host**: A9_MEGA (PC IA) · **Estado**: **BLOCKED_REAL**: 27/27 PASS (4 reales); solo bloqueado porque sus dependencias P003 y P004 no tienen REAL_PASS. [evidence.json](P010_hardware-profiler-windows-cpu-y-memoria.evidence.json) · [operación](../operations/hardware.md)

## Qué se construyó

- **Dominio** (`larm-domain/src/hardware.rs`): `HardwareProfile`, `HardwareSnapshot`, `OsInfo`, `CpuInfo`, `MemoryInfo`, `DeviceInfo`, `Dependency` y `ObservedMetrics`. Siguen el schema cerrado `hardware_profile`: los bytes van como string decimal y se rechazan los campos extra. Incluye:
  - la huella del contrato 04 (RFC 8785 + sha256), con el orden canónico y sin `observed_metrics`, LUID ni `evidence_refs`;
  - los invariantes: disponible ≤ visible ≤ instalada y físicos ≤ lógicos.
- **Puertos**: `HardwareProber` y `HardwareRepository`. La API depende solo de ellos (`crate-graph.json` no le deja usar `larm-db` ni `larm-hardware`).
- **Perfilador** (`larm-hardware`):
  - OS desde el registro (`EditionID`, `CurrentBuild.UBR`, `PROCESSOR_ARCHITECTURE`), leído con `windows-registry`, sin `unsafe`;
  - CPU con sysinfo 0.39.6, versión fijada en P000;
  - ISA por CPUID en ejecución;
  - RAM visible y pagefile con sysinfo;
  - RAM instalada por CIM con timeout de 10 s, matando el proceso si vence: es el único dato sin API segura disponible.
- **Persistencia** (`larm-db/src/hardware.rs`): una fila por configuración. La misma huella actualiza el perfil más reciente (`row_version`+1); una huella distinta crea un perfil nuevo. El `UPDATE` toma el bloqueo de escritura antes de decidir, así que 10 sondeos simultáneos dejan un único perfil.
- **API**: `GET /api/v1/hardware` (scope `read`), con lista `{items, next_cursor, snapshot_at}` y cursor HMAC del contrato 07. La paginación de `CursorSigner` pasó a ser genérica y la de `/credentials` sigue pasando sus pruebas.
- **Daemon**: sondeo en segundo plano al arrancar y comando `hardware-probe [--record]`.
- **Escritorio**: pantalla Hardware real. Cada dato lleva fuente y estado (verificado o desconocido con motivo), unidades IEC con los bytes exactos y una nota cuando hay RAM reservada para la GPU. Muestra los estados de carga, vacío y error con reintento. Lo que aún no se sondea se presenta como «todavía no se sondea», nunca como «no hay».

## Decisiones

- La edición es `EditionID` (`Professional`), no `ProductName`, porque en Windows 11 sigue diciendo «Windows 10». La build lleva `UBR`, porque una actualización acumulativa cambia componentes del sistema y debe cambiar la huella.
- Las extensiones del procesador usan los nombres de característica de LLVM/Rust (`avx2`, `avx512f`, `avxvnni`). El paquete no fija una nomenclatura propia.
- `pagefile` y la procedencia van en `metadata`, porque el schema del snapshot es cerrado y no tiene campo para ellos: `profiler {name, version}`, `sources`, `coverage` y `unknown[]`.
- `metadata.coverage.pending = ["devices", "dependencies"]` hasta P011, para no confundir «no sondeado» con «no hay».
- Dependencia nueva: `windows-registry` 0.100.0 (Microsoft, MIT/Apache-2.0), añadida a `environment.lock.json` con su sha256. Evita una segunda excepción de `unsafe` solo para leer el registro.

## Prueba real en el PC IA

`tools/p010_real.py windows`: 12 comprobaciones contra fuentes independientes, todas iguales.

- **CPU**: 32 hilos y 16 núcleos (CIM); AuthenticAMD «AMD RYZEN AI MAX+ 395 w/ Radeon 8060S».
- **Extensiones**: `IsProcessorFeaturePresent` coincide con el sondeo en ssse3, sse4.1, sse4.2, avx, avx2 y avx512f.
- **RAM visible**: 68 340 748 288 B, igual que `Win32_ComputerSystem.TotalPhysicalMemory`.
- **RAM instalada**: 137 438 953 472 B, igual que la suma de `Win32_PhysicalMemory`. Los 64 GiB reservados a la GPU no se cuentan como RAM.
- **Pagefile**: 4 GiB, igual que `Win32_PageFileUsage`.
- **Sistema**: Professional 26200.9457 AMD64, comprobado con `reg.exe` y `Win32_OperatingSystem`.
- **Huella**: idéntica en dos sondeos aunque cambien la RAM libre y el uso de CPU.

`tools/p010_real.py serve`, con el daemon real:

- el perfil se registra al arrancar y la respuesta es conforme a `hardware_profileList`;
- un rearranque con el mismo hardware actualiza el mismo perfil (`row_version` 2);
- 401 sin token, 403 sin scope `read`, 400 con `limit=0` o con un filtro no admitido.

## Fallos de WMI

Los fallos de WMI se prueban con procesos reales:

| Caso | Resultado |
|---|---|
| Programa inexistente | `wmi_unavailable` |
| PowerShell colgado 60 s | `wmi_timeout` en 1,5 s, con el proceso muerto |
| Clase CIM inexistente | `wmi_failed` |
| Salida basura | `wmi_unparseable` |

En los cuatro casos el sondeo continúa: `ram.physical_bytes` queda `null` con su motivo y el resto del perfil se observa igualmente.

## Pendiente

Resuelto en P011 (2026-10-11): GPU, NPU, drivers, backends, dependencias de runtime y `POST /hardware/refresh` como job. La evidencia de P010 se volvió a generar sobre ese código.

- Reproducibilidad completa de la huella con dispositivos (P013).
