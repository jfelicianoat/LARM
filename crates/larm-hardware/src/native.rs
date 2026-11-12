//! Sondeo nativo de aceleradores (ADR-0010). Único módulo de `larm-hardware`
//! con `unsafe`; se ejecuta en el proceso hijo `larm-daemon probe-accelerators`,
//! nunca dentro del daemon.
//!
//! - DXGI: adaptadores con LUID, IDs PCI y memoria dedicada/compartida.
//! - D3D12: dispositivo real por adaptador y arquitectura UMA.
//! - DirectML: `IDMLDevice` real sobre ese dispositivo (DLL cargada en ejecución).
//! - HIP: `hipGetDeviceCount` real (DLL de System32 o `%HIP_PATH%\bin`).
//! - Windows ML: `LearningModelDevice` DirectX real (API WinRT, segura).
//! - Versión de fichero (`VS_FIXEDFILEINFO`) de las DLL que pida el perfilador.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use windows::core::{Interface, GUID, HRESULT, PCSTR, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_DevNode_Registry_PropertyW, CM_Get_DevNode_Status, CM_Get_Device_ID_ListW,
    CM_Get_Device_ID_List_SizeW, CM_Locate_DevNodeW, CM_DEVNODE_STATUS_FLAGS, CM_DRP_DRIVER,
    CM_GETIDLIST_FILTER_CLASS, CM_GETIDLIST_FILTER_PRESENT, CM_LOCATE_DEVNODE_NORMAL, CM_PROB,
    CR_SUCCESS, DN_HAS_PROBLEM, GUID_DEVCLASS_COMPUTEACCELERATOR, GUID_DEVCLASS_DISPLAY,
};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D12::{
    D3D12CreateDevice, ID3D12Device, D3D12_FEATURE_ARCHITECTURE, D3D12_FEATURE_DATA_ARCHITECTURE,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::AI::MachineLearning::DirectML::{IDMLDevice, DML_CREATE_DEVICE_FLAGS};

use crate::native_report::{
    luid_text, ApiProbe, FileVersion, NativeAdapter, NativeReport, PnpDevice, ProbeStatus,
    NATIVE_REPORT_VERSION,
};

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(std::iter::once(0)).collect()
}

fn hresult(e: &windows::core::Error) -> String {
    format!("{:#010x} {}", e.code().0, e.message().trim())
}

/// Carga una DLL sin importarla en el binario. `System32` primero; con una
/// ruta absoluta, sólo esa ruta y su carpeta para sus dependencias.
fn load(library: &Path) -> Option<HMODULE> {
    let name = wide(library.as_os_str());
    let flags = if library.is_absolute() {
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32
    } else {
        LOAD_LIBRARY_SEARCH_SYSTEM32
    };
    // SAFETY: `name` es una cadena UTF-16 terminada en 0 que vive durante la
    // llamada; sin handle de fichero (None) y con flags de búsqueda válidas.
    unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, flags) }.ok()
}

/// Dirección de una exportación como puntero de función sin tipo.
fn symbol(module: HMODULE, name: &'static [u8]) -> Option<unsafe extern "system" fn() -> isize> {
    // SAFETY: `name` es ASCII terminado en 0 (los literales lo garantizan) y
    // `module` es un handle devuelto por LoadLibraryExW que no se libera.
    unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }
}

type DmlCreateDevice = unsafe extern "system" fn(
    *mut c_void,
    DML_CREATE_DEVICE_FLAGS,
    *const GUID,
    *mut *mut c_void,
) -> HRESULT;

fn directml(device: &ID3D12Device) -> ApiProbe {
    let Some(module) = load(Path::new("DirectML.dll")) else {
        return ApiProbe::new(ProbeStatus::Absent, "DirectML.dll no está en System32");
    };
    let Some(address) = symbol(module, b"DMLCreateDevice\0") else {
        return ApiProbe::new(ProbeStatus::Failed, "DirectML.dll sin DMLCreateDevice");
    };
    // SAFETY: firma documentada de DMLCreateDevice (DirectML.h); la dirección
    // es la de esa exportación.
    let create: DmlCreateDevice = unsafe { std::mem::transmute(address) };
    let mut raw: *mut c_void = std::ptr::null_mut();
    // SAFETY: `device` es un ID3D12Device vivo; pedimos IDMLDevice por su IID
    // y `raw` recibe una referencia que pasa a ser nuestra.
    let hr = unsafe {
        create(
            device.as_raw(),
            DML_CREATE_DEVICE_FLAGS(0),
            &IDMLDevice::IID,
            &mut raw,
        )
    };
    if hr.is_err() || raw.is_null() {
        return ApiProbe::new(
            ProbeStatus::Failed,
            format!("DMLCreateDevice {:#010x}", hr.0),
        );
    }
    // SAFETY: `raw` es un IDMLDevice con una referencia propia; `from_raw` la
    // adopta y la libera al soltarse.
    drop(unsafe { IDMLDevice::from_raw(raw) });
    let mut probe = ApiProbe::new(ProbeStatus::Verified, "IDMLDevice creado");
    probe.library = Some("DirectML.dll".into());
    probe
}

fn adapter(adapter: &IDXGIAdapter1) -> Result<NativeAdapter, String> {
    // SAFETY: llamada COM sobre un adaptador vivo.
    let desc = unsafe { adapter.GetDesc1() }.map_err(|e| hresult(&e))?;
    let end = desc
        .Description
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(desc.Description.len());
    let luid = luid_text(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
    let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0.cast_unsigned() != 0;
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: adaptador vivo, nivel de función válido y `device` recibe la
    // interfaz pedida.
    let created = unsafe { D3D12CreateDevice(adapter, D3D_FEATURE_LEVEL_11_0, &mut device) };
    let (mut d3d12, uma, directml) = match (created, device) {
        (Ok(()), Some(device)) => {
            let mut arch = D3D12_FEATURE_DATA_ARCHITECTURE::default();
            // SAFETY: el puntero y el tamaño son los de la estructura que
            // corresponde a D3D12_FEATURE_ARCHITECTURE.
            let uma = unsafe {
                device.CheckFeatureSupport(
                    D3D12_FEATURE_ARCHITECTURE,
                    std::ptr::from_mut(&mut arch).cast(),
                    u32::try_from(std::mem::size_of::<D3D12_FEATURE_DATA_ARCHITECTURE>())
                        .unwrap_or(0),
                )
            }
            .ok()
            .map(|()| arch.UMA.as_bool());
            (
                ApiProbe::new(ProbeStatus::Verified, "ID3D12Device creado (nivel 11_0)"),
                uma,
                directml(&device),
            )
        }
        (Err(e), _) => (
            ApiProbe::new(
                ProbeStatus::Failed,
                format!("D3D12CreateDevice {}", hresult(&e)),
            ),
            None,
            ApiProbe::new(
                ProbeStatus::Failed,
                "sin dispositivo D3D12 en este adaptador",
            ),
        ),
        (Ok(()), None) => (
            ApiProbe::new(ProbeStatus::Failed, "D3D12CreateDevice sin dispositivo"),
            None,
            ApiProbe::new(
                ProbeStatus::Failed,
                "sin dispositivo D3D12 en este adaptador",
            ),
        ),
    };
    d3d12.luid = Some(luid.clone());
    let mut directml = directml;
    if directml.status == ProbeStatus::Verified {
        directml.luid = Some(luid.clone());
    }
    Ok(NativeAdapter {
        luid,
        vendor_id: desc.VendorId,
        device_id: desc.DeviceId,
        subsys_id: desc.SubSysId,
        revision: desc.Revision,
        description: String::from_utf16_lossy(&desc.Description[..end]),
        dedicated_video_memory: desc.DedicatedVideoMemory as u64,
        dedicated_system_memory: desc.DedicatedSystemMemory as u64,
        shared_system_memory: desc.SharedSystemMemory as u64,
        software,
        d3d12,
        uma,
        directml,
    })
}

fn adapters() -> (ApiProbe, Vec<NativeAdapter>) {
    // SAFETY: crea una fábrica DXGI; sin parámetros de entrada.
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(e) => {
            return (
                ApiProbe::new(
                    ProbeStatus::Failed,
                    format!("CreateDXGIFactory1 {}", hresult(&e)),
                ),
                vec![],
            )
        }
    };
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for index in 0u32.. {
        // SAFETY: llamada COM sobre una fábrica viva; termina con
        // DXGI_ERROR_NOT_FOUND al pasar del último adaptador.
        let Ok(handle) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        match adapter(&handle) {
            Ok(a) => out.push(a),
            Err(e) => errors.push(format!("adaptador {index}: {e}")),
        }
    }
    let probe = if errors.is_empty() {
        let mut p = ApiProbe::new(ProbeStatus::Verified, format!("{} adaptadores", out.len()));
        p.devices = u32::try_from(out.len()).ok();
        p
    } else {
        ApiProbe::new(ProbeStatus::Failed, errors.join("; "))
    };
    (probe, out)
}

type HipIntOut = unsafe extern "C" fn(*mut i32) -> i32;

/// Candidatas de HIP: System32 (driver AMD) y `%HIP_PATH%\bin` (HIP SDK).
fn hip_candidates() -> Vec<PathBuf> {
    let names = ["amdhip64_7.dll", "amdhip64_6.dll", "amdhip64.dll"];
    let mut out: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
    if let Some(root) = std::env::var_os("HIP_PATH") {
        let bin = PathBuf::from(root).join("bin");
        out.extend(names.iter().map(|n| bin.join(n)));
    }
    out
}

fn hip() -> ApiProbe {
    let Some((library, module)) = hip_candidates()
        .into_iter()
        .find_map(|p| load(&p).map(|m| (p, m)))
    else {
        return ApiProbe::new(
            ProbeStatus::Absent,
            "amdhip64*.dll no está en System32 ni en %HIP_PATH%\\bin",
        );
    };
    let library = library.display().to_string();
    let Some(count_fn) = symbol(module, b"hipGetDeviceCount\0") else {
        return ApiProbe::new(
            ProbeStatus::Failed,
            format!("{library} sin hipGetDeviceCount"),
        );
    };
    // SAFETY: firma documentada de hipGetDeviceCount(int*) -> hipError_t.
    let count_fn: HipIntOut = unsafe { std::mem::transmute(count_fn) };
    let mut count = 0i32;
    // SAFETY: `count` es un entero válido para escritura.
    let code = unsafe { count_fn(&mut count) };
    let version = symbol(module, b"hipRuntimeGetVersion\0").and_then(|f| {
        // SAFETY: firma documentada de hipRuntimeGetVersion(int*) -> hipError_t.
        let f: HipIntOut = unsafe { std::mem::transmute(f) };
        let mut v = 0i32;
        // SAFETY: `v` es un entero válido para escritura.
        (unsafe { f(&mut v) } == 0).then(|| v.to_string())
    });
    let mut probe = if code == 0 && count > 0 {
        let mut p = ApiProbe::new(
            ProbeStatus::Verified,
            format!("hipGetDeviceCount = {count}"),
        );
        p.devices = u32::try_from(count).ok();
        p
    } else if code == 0 {
        ApiProbe::new(ProbeStatus::Failed, "hipGetDeviceCount = 0")
    } else {
        ApiProbe::new(
            ProbeStatus::Failed,
            format!("hipGetDeviceCount devolvió hipError {code}"),
        )
    };
    probe.version = version;
    probe.library = Some(library);
    probe
}

fn winml() -> ApiProbe {
    use windows::AI::MachineLearning::{LearningModelDevice, LearningModelDeviceKind};
    match LearningModelDevice::Create(LearningModelDeviceKind::DirectXHighPerformance)
        .and_then(|d| d.AdapterId())
    {
        Ok(id) => {
            let mut p = ApiProbe::new(
                ProbeStatus::Verified,
                "LearningModelDevice DirectXHighPerformance creado",
            );
            p.luid = Some(luid_text(id.HighPart, id.LowPart));
            p.library = Some("Windows.AI.MachineLearning".into());
            p
        }
        Err(e) if e.code() == windows::Win32::Foundation::REGDB_E_CLASSNOTREG => ApiProbe::new(
            ProbeStatus::Absent,
            "Windows ML no está registrado en este sistema",
        ),
        Err(e) => ApiProbe::new(
            ProbeStatus::Failed,
            format!("LearningModelDevice {}", hresult(&e)),
        ),
    }
}

fn file_version(path: &Path) -> FileVersion {
    let name = wide(path.as_os_str());
    let path_text = path.display().to_string();
    let fail = |error: String| FileVersion {
        path: path_text.clone(),
        version: None,
        error: Some(error),
    };
    // SAFETY: `name` es UTF-16 terminado en 0.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(name.as_ptr()), None) };
    if size == 0 {
        return fail("sin recurso de versión".into());
    }
    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` tiene exactamente `size` bytes escribibles.
    if let Err(e) = unsafe {
        GetFileVersionInfoW(
            PCWSTR(name.as_ptr()),
            None,
            size,
            buffer.as_mut_ptr().cast(),
        )
    } {
        return fail(hresult(&e));
    }
    let mut info: *mut c_void = std::ptr::null_mut();
    let mut len = 0u32;
    let root: Vec<u16> = "\\\0".encode_utf16().collect();
    // SAFETY: `buffer` contiene el bloque de versión; `info` apuntará dentro
    // de `buffer`, que sigue vivo mientras se lee.
    let found = unsafe {
        VerQueryValueW(
            buffer.as_ptr().cast(),
            PCWSTR(root.as_ptr()),
            &mut info,
            &mut len,
        )
    };
    if !found.as_bool()
        || info.is_null()
        || (len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
    {
        return fail("VS_FIXEDFILEINFO ausente".into());
    }
    // SAFETY: comprobado arriba que `info` apunta a un VS_FIXEDFILEINFO completo.
    let fixed = unsafe { &*info.cast::<VS_FIXEDFILEINFO>() };
    FileVersion {
        path: path_text,
        version: Some(format!(
            "{}.{}.{}.{}",
            fixed.dwFileVersionMS >> 16,
            fixed.dwFileVersionMS & 0xffff,
            fixed.dwFileVersionLS >> 16,
            fixed.dwFileVersionLS & 0xffff
        )),
        error: None,
    }
}

/// Instancias presentes de una clase de dispositivo.
fn present_in_class(class: &GUID) -> Result<Vec<String>, String> {
    let filter = wide(std::ffi::OsStr::new(&format!("{{{class:?}}}")));
    let flags = CM_GETIDLIST_FILTER_CLASS | CM_GETIDLIST_FILTER_PRESENT;
    let mut len = 0u32;
    // SAFETY: `filter` es UTF-16 terminado en 0; `len` recibe el tamaño.
    let cr = unsafe { CM_Get_Device_ID_List_SizeW(&mut len, PCWSTR(filter.as_ptr()), flags) };
    if cr != CR_SUCCESS {
        return Err(format!("CM_Get_Device_ID_List_SizeW CONFIGRET {}", cr.0));
    }
    let mut buffer = vec![0u16; len as usize];
    // SAFETY: `buffer` tiene el tamaño que acaba de pedir Configuration Manager.
    let cr = unsafe { CM_Get_Device_ID_ListW(PCWSTR(filter.as_ptr()), &mut buffer, flags) };
    if cr != CR_SUCCESS {
        return Err(format!("CM_Get_Device_ID_ListW CONFIGRET {}", cr.0));
    }
    Ok(buffer
        .split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}

fn pnp_device(class: &str, instance_id: String) -> PnpDevice {
    let id = wide(std::ffi::OsStr::new(&instance_id));
    let mut node = 0u32;
    // SAFETY: `id` es UTF-16 terminado en 0; `node` recibe el devnode.
    let located =
        unsafe { CM_Locate_DevNodeW(&mut node, PCWSTR(id.as_ptr()), CM_LOCATE_DEVNODE_NORMAL) };
    if located != CR_SUCCESS {
        return PnpDevice {
            class: class.into(),
            instance_id,
            driver_key: None,
            problem: None,
        };
    }
    let mut status = CM_DEVNODE_STATUS_FLAGS(0);
    let mut problem = CM_PROB(0);
    // SAFETY: punteros a variables locales y devnode recién localizado.
    let status_ok =
        unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, node, 0) } == CR_SUCCESS;
    let mut buffer = [0u16; 260];
    let mut bytes = u32::try_from(std::mem::size_of_val(&buffer)).unwrap_or(0);
    // SAFETY: `buffer` tiene `bytes` bytes escribibles; CM escribe un REG_SZ.
    let driver = unsafe {
        CM_Get_DevNode_Registry_PropertyW(
            node,
            CM_DRP_DRIVER,
            None,
            Some(buffer.as_mut_ptr().cast()),
            &mut bytes,
            0,
        )
    } == CR_SUCCESS;
    let driver_key = driver.then(|| {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    });
    PnpDevice {
        class: class.into(),
        instance_id,
        driver_key,
        problem: (status_ok && status.0 & DN_HAS_PROBLEM.0 != 0).then_some(problem.0),
    }
}

fn pnp() -> (ApiProbe, Vec<PnpDevice>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (name, guid) in [
        ("DISPLAY", GUID_DEVCLASS_DISPLAY),
        ("COMPUTE_ACCELERATOR", GUID_DEVCLASS_COMPUTEACCELERATOR),
    ] {
        match present_in_class(&guid) {
            Ok(ids) => out.extend(ids.into_iter().map(|id| pnp_device(name, id))),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    let probe = if errors.is_empty() {
        let mut p = ApiProbe::new(
            ProbeStatus::Verified,
            format!("{} dispositivos presentes", out.len()),
        );
        p.devices = u32::try_from(out.len()).ok();
        p
    } else {
        ApiProbe::new(ProbeStatus::Failed, errors.join("; "))
    };
    (probe, out)
}

/// Ejecuta el sondeo nativo completo. Pensado para el proceso hijo.
#[must_use]
pub fn probe(files: &[PathBuf]) -> NativeReport {
    let (dxgi, adapters) = adapters();
    let (pnp_probe, pnp) = pnp();
    NativeReport {
        report_version: NATIVE_REPORT_VERSION,
        dxgi,
        adapters,
        hip: hip(),
        winml: winml(),
        pnp_probe,
        pnp,
        files: files.iter().map(|p| file_version(p)).collect(),
    }
}

// ---------------------------------------------------------------------------
// Almacenamiento (P012): volúmenes locales y rutas de red.
// ---------------------------------------------------------------------------

/// Texto UTF-16 hasta el primer 0.
fn until_nul(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// `(etiqueta, sistema de ficheros, solo lectura)` de una raíz (`C:\`,
/// `\\?\Volume{…}\` o `\\servidor\recurso\`).
fn volume_information(root: &[u16]) -> Result<(Option<String>, Option<String>, bool), String> {
    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    let mut flags = 0u32;
    // SAFETY: `root` termina en 0; los búferes y `flags` son locales y escribibles.
    unsafe {
        windows::Win32::Storage::FileSystem::GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            Some(&mut label),
            None,
            None,
            Some(&mut flags),
            Some(&mut fs),
        )
    }
    .map_err(|e| format!("GetVolumeInformationW {}", hresult(&e)))?;
    let text = |b: &[u16]| Some(until_nul(b)).filter(|s| !s.is_empty());
    let read_only = flags & windows::Win32::System::SystemServices::FILE_READ_ONLY_VOLUME != 0;
    Ok((text(&label), text(&fs), read_only))
}

/// `(capacidad, libres)` con enteros de 64 bits. «Libres» es lo disponible
/// para este usuario (respeta cuotas), no el total del disco.
fn disk_space(root: &[u16]) -> Result<(u64, u64), String> {
    let (mut available, mut total) = (0u64, 0u64);
    // SAFETY: `root` termina en 0; los contadores son locales.
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            PCWSTR(root.as_ptr()),
            Some(&mut available),
            Some(&mut total),
            None,
        )
    }
    .map_err(|e| format!("GetDiskFreeSpaceExW {}", hresult(&e)))?;
    Ok((total, available))
}

/// Consulta `IOCTL_STORAGE_QUERY_PROPERTY` sobre el volumen y devuelve los
/// bytes crudos de la respuesta (se analizan por offsets, sin reinterpretar
/// memoria del kernel como tipos de Rust).
fn storage_property(
    device: windows::Win32::Foundation::HANDLE,
    property: i32,
) -> Result<Vec<u8>, String> {
    // STORAGE_PROPERTY_QUERY: PropertyId (i32), QueryType (i32 = estándar), AdditionalParameters.
    let mut query = [0u8; 12];
    query[..4].copy_from_slice(&property.to_le_bytes());
    let mut out = vec![0u8; 1024];
    let mut returned = 0u32;
    // SAFETY: búfer de entrada y salida locales con sus tamaños reales; handle abierto.
    unsafe {
        windows::Win32::System::IO::DeviceIoControl(
            device,
            windows::Win32::System::Ioctl::IOCTL_STORAGE_QUERY_PROPERTY,
            Some(query.as_ptr().cast()),
            u32::try_from(query.len()).unwrap_or(0),
            Some(out.as_mut_ptr().cast()),
            u32::try_from(out.len()).unwrap_or(0),
            Some(&mut returned),
            None,
        )
    }
    .map_err(|e| format!("IOCTL_STORAGE_QUERY_PROPERTY({property}) {}", hresult(&e)))?;
    out.truncate(returned as usize);
    Ok(out)
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    bytes
        .get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn ascii_at(bytes: &[u8], offset: usize) -> Option<String> {
    if offset == 0 || offset >= bytes.len() {
        return None;
    }
    let end = bytes[offset..]
        .iter()
        .position(|b| *b == 0)
        .map_or(bytes.len(), |p| offset + p);
    Some(
        String::from_utf8_lossy(&bytes[offset..end])
            .trim()
            .to_string(),
    )
    .filter(|s| !s.is_empty())
}

/// Bus, penalización de búsqueda, medio extraíble y modelo del dispositivo.
fn device_info(
    volume_path: &str,
    errors: &mut Vec<String>,
) -> (Option<u32>, Option<bool>, Option<bool>, Option<String>) {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    // CreateFileW sobre el volumen exige el nombre sin la barra final.
    let device = wide(std::ffi::OsStr::new(volume_path.trim_end_matches('\\')));
    // SAFETY: nombre terminado en 0; acceso 0 (solo consulta), sin plantilla.
    let handle = match unsafe {
        CreateFileW(
            PCWSTR(device.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    } {
        Ok(h) => h,
        Err(e) => {
            errors.push(format!("CreateFileW {}", hresult(&e)));
            return (None, None, None, None);
        }
    };
    // STORAGE_DEVICE_DESCRIPTOR: RemovableMedia @10, ProductIdOffset @16, BusType @28.
    let (bus, removable, product) = match storage_property(handle, 0) {
        Ok(d) => (
            u32_at(&d, 28),
            d.get(10).map(|b| *b != 0),
            u32_at(&d, 16).and_then(|o| ascii_at(&d, o as usize)),
        ),
        Err(e) => {
            errors.push(e);
            (None, None, None)
        }
    };
    // DEVICE_SEEK_PENALTY_DESCRIPTOR: IncursSeekPenalty @8.
    let seek = match storage_property(handle, 7) {
        Ok(d) => d.get(8).map(|b| *b != 0),
        Err(e) => {
            errors.push(e);
            None
        }
    };
    // SAFETY: handle abierto arriba y no usado después.
    let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
    (bus, seek, removable, product)
}

fn mount_points(volume: &[u16]) -> Result<Vec<String>, String> {
    use windows::Win32::Storage::FileSystem::GetVolumePathNamesForVolumeNameW;
    let mut len = 0u32;
    // SAFETY: primera llamada solo para conocer el tamaño necesario.
    let _ = unsafe { GetVolumePathNamesForVolumeNameW(PCWSTR(volume.as_ptr()), None, &mut len) };
    let mut buffer = vec![0u16; len.max(1) as usize];
    // SAFETY: `buffer` tiene el tamaño que pidió la llamada anterior.
    unsafe {
        GetVolumePathNamesForVolumeNameW(PCWSTR(volume.as_ptr()), Some(&mut buffer), &mut len)
    }
    .map_err(|e| format!("GetVolumePathNamesForVolumeNameW {}", hresult(&e)))?;
    let mut points: Vec<String> = buffer
        .split(|c| *c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect();
    points.sort();
    Ok(points)
}

fn local_volume(volume_path: String) -> crate::native_report::NativeVolume {
    let mut errors = Vec::new();
    let name = wide(std::ffi::OsStr::new(&volume_path));
    let mounts = mount_points(&name).unwrap_or_else(|e| {
        errors.push(e);
        Vec::new()
    });
    // SAFETY: nombre de volumen terminado en 0.
    let drive_type =
        unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(PCWSTR(name.as_ptr())) };
    let (label, filesystem, read_only) = volume_information(&name).unwrap_or_else(|e| {
        errors.push(e);
        (None, None, false)
    });
    let (capacity, free) = match disk_space(&name) {
        Ok((c, f)) => (Some(c), Some(f)),
        Err(e) => {
            errors.push(e);
            (None, None)
        }
    };
    let (bus_type, seek_penalty, removable_media, product) = device_info(&volume_path, &mut errors);
    crate::native_report::NativeVolume {
        volume_path,
        mount_points: mounts,
        label,
        filesystem,
        read_only,
        drive_type,
        capacity_bytes: capacity,
        free_bytes: free,
        bus_type,
        seek_penalty,
        removable_media,
        product,
        errors,
    }
}

fn share(unc_path: String, drive_letter: Option<String>) -> crate::native_report::NativeShare {
    let mut errors = Vec::new();
    let root = format!("{}\\", unc_path.trim_end_matches('\\'));
    let name = wide(std::ffi::OsStr::new(&root));
    let space = disk_space(&name);
    let info = volume_information(&name);
    let available = space.is_ok();
    let (capacity, free) = match space {
        Ok((c, f)) => (Some(c), Some(f)),
        Err(e) => {
            errors.push(e);
            (None, None)
        }
    };
    let (filesystem, read_only) = match info {
        Ok((_, fs, ro)) => (fs, ro),
        Err(e) => {
            errors.push(e);
            (None, false)
        }
    };
    crate::native_report::NativeShare {
        unc_path,
        drive_letter,
        available,
        filesystem,
        read_only,
        capacity_bytes: capacity,
        free_bytes: free,
        errors,
    }
}

/// Unidades de red mapeadas (`Z:` → `\\servidor\recurso`).
fn mapped_drives() -> Vec<(String, String)> {
    // SAFETY: sin parámetros.
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u8 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = format!("{}:", char::from(b'A' + i));
        let root = wide(std::ffi::OsStr::new(&format!("{letter}\\")));
        // SAFETY: raíz terminada en 0.
        let kind =
            unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(PCWSTR(root.as_ptr())) };
        if kind != 4 {
            continue; // DRIVE_REMOTE
        }
        let local = wide(std::ffi::OsStr::new(&letter));
        let mut buffer = [0u16; 1024];
        let mut len = u32::try_from(buffer.len()).unwrap_or(0);
        // SAFETY: `buffer` tiene `len` caracteres escribibles.
        let rc = unsafe {
            windows::Win32::NetworkManagement::WNet::WNetGetConnectionW(
                PCWSTR(local.as_ptr()),
                Some(windows::core::PWSTR(buffer.as_mut_ptr())),
                &mut len,
            )
        };
        if rc.0 == 0 {
            out.push((letter, until_nul(&buffer)));
        }
    }
    out
}

/// Sondeo de almacenamiento: volúmenes locales, unidades de red mapeadas y
/// las raíces UNC pedidas (las de `storage_location`).
#[must_use]
pub fn probe_storage(unc_roots: &[String]) -> crate::native_report::StorageReport {
    use windows::Win32::Storage::FileSystem::{FindFirstVolumeW, FindNextVolumeW, FindVolumeClose};
    let mut volumes = Vec::new();
    let mut buffer = [0u16; 260];
    // SAFETY: `buffer` local y escribible.
    let volumes_probe = match unsafe { FindFirstVolumeW(&mut buffer) } {
        Ok(handle) => {
            loop {
                volumes.push(local_volume(until_nul(&buffer)));
                // SAFETY: handle de búsqueda válido y búfer local.
                if unsafe { FindNextVolumeW(handle, &mut buffer) }.is_err() {
                    break;
                }
            }
            // SAFETY: handle de búsqueda válido, no usado después.
            let _ = unsafe { FindVolumeClose(handle) };
            let mut p = ApiProbe::new(
                ProbeStatus::Verified,
                format!("{} volúmenes", volumes.len()),
            );
            p.devices = u32::try_from(volumes.len()).ok();
            p
        }
        Err(e) => ApiProbe::new(
            ProbeStatus::Failed,
            format!("FindFirstVolumeW {}", hresult(&e)),
        ),
    };
    let mut shares: Vec<crate::native_report::NativeShare> = mapped_drives()
        .into_iter()
        .map(|(letter, unc)| share(unc, Some(letter)))
        .collect();
    for root in unc_roots {
        if !shares.iter().any(|s| s.unc_path.eq_ignore_ascii_case(root)) {
            shares.push(share(root.clone(), None));
        }
    }
    crate::native_report::StorageReport {
        report_version: crate::native_report::STORAGE_REPORT_VERSION,
        volumes_probe,
        volumes,
        shares,
    }
}
