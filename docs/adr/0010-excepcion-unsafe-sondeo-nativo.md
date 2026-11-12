# ADR-0010 — Segunda excepción a `unsafe`: sondeo nativo de aceleradores en un proceso hijo

**Estado**: aceptado · **Fecha**: 2026-10-11 · **Prompt**: P011

## Contexto

La spec §22 y P011 piden enumerar los adaptadores gráficos «vía APIs Windows/DXGI» y detectar DirectML y Windows ML «por disponibilidad real». El contrato 04 exige que un backend detectado no cuente como operable sin una prueba. Estas APIs son COM o FFI y, en el crate `windows`, son funciones `unsafe`:

- `CreateDXGIFactory1`, `EnumAdapters1` y `GetDesc1`;
- `D3D12CreateDevice` y `CheckFeatureSupport`;
- `DMLCreateDevice`, cargado dinámicamente;
- `hipGetDeviceCount`;
- `GetFileVersionInfoW`.

No existe una alternativa segura que dé LUID, memoria dedicada y compartida, UMA y la creación real de un dispositivo DirectML. Además, al cargar la DLL de un fabricante (HIP) o el UMD del driver, un fallo de ese código tumba el proceso que lo carga.

ADR-0009 dejó una sola excepción y dijo que cualquier otra necesitaría su propio ADR.

## Decisión

- Segunda y última excepción: `crates/larm-hardware/src/native.rs`. Es el único módulo con `#[allow(unsafe_code)]`, solo existe en Windows y cada bloque lleva su `// SAFETY:`.
- El daemon **no** ejecuta ese código en su propio proceso. Lo lanza como `larm-daemon probe-accelerators`, un proceso hijo con timeout que se mata si vence. El hijo imprime un JSON y el perfilador lo valida. Si el hijo se cuelga, revienta o devuelve basura, los backends que dependen de él quedan `UNKNOWN` con su motivo y el resto del sondeo sigue.
- Las DLL opcionales (`DirectML.dll`, `amdhip64*.dll`) se cargan con `LoadLibraryExW`, sin importarlas en el binario. Así un equipo sin ellas sigue arrancando el daemon.
- `tools/verify_p001.py policies` admite exactamente estos dos ficheros y exige los dos ADR.

## Consecuencias

- El riesgo de memoria queda confinado en un proceso de vida corta y sin estado del daemon.
- Cada sondeo cuesta un proceso, unos cientos de milisegundos. Solo ocurre al arrancar y con `POST /hardware/refresh`.
- Las pruebas del hijo usan procesos reales que se cuelgan, fallan o mienten; no son mocks del contrato.

## Ampliación (P012, 2026-10-11)

`native.rs` también enumera el almacenamiento, en el proceso hijo `larm-daemon probe-storage`:

- `FindFirstVolumeW` / `FindNextVolumeW` y `GetVolumePathNamesForVolumeNameW` para los volúmenes y sus puntos de montaje;
- `GetVolumeInformationW` y `GetDiskFreeSpaceExW` para el sistema de ficheros, la capacidad y el espacio libre;
- `IOCTL_STORAGE_QUERY_PROPERTY` para el bus y la penalización de búsqueda;
- `WNetGetConnectionW` para las unidades de red mapeadas.

Las respuestas de los IOCTL se leen como bytes en offsets documentados; no se reinterpreta memoria del kernel como tipos de Rust.

No se abre una tercera excepción: sigue siendo el mismo módulo, con las mismas reglas. El timeout del hijo es de 60 s, porque comprobar un servidor SMB caído tarda decenas de segundos.
