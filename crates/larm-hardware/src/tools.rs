//! Herramientas de consulta de las APIs Vulkan y OpenCL: `vulkaninfo
//! --summary` (Vulkan SDK/runtime) y `clinfo` (ICD de OpenCL). Que la
//! herramienta enumere un dispositivo físico es la prueba de que el loader,
//! el ICD del driver y el dispositivo funcionan juntos.

/// Dispositivo físico de `vulkaninfo --summary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanDevice {
    pub vendor_id: u32,
    pub device_id: u32,
    pub name: String,
    pub device_type: String,
    pub api_version: Option<String>,
    pub driver_info: Option<String>,
}

fn hex(text: &str) -> Option<u32> {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    let t = t.strip_suffix('h').unwrap_or(t);
    u32::from_str_radix(t, 16).ok()
}

/// Bloques `GPUn:` de la sección `Devices:`. Un bloque sin vendorID o
/// deviceID legibles se descarta (no se inventa la identidad).
#[must_use]
pub fn parse_vulkaninfo(text: &str) -> Vec<VulkanDevice> {
    let Some(start) = text.find("\nDevices:") else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    let mut current: Option<Vec<(String, String)>> = None;
    let flush = |fields: Vec<(String, String)>, out: &mut Vec<VulkanDevice>| {
        let get = |k: &str| {
            fields
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        };
        if let (Some(vendor), Some(device)) = (
            get("vendorID").and_then(|v| hex(&v)),
            get("deviceID").and_then(|v| hex(&v)),
        ) {
            out.push(VulkanDevice {
                vendor_id: vendor,
                device_id: device,
                name: get("deviceName").unwrap_or_default(),
                device_type: get("deviceType").unwrap_or_default(),
                api_version: get("apiVersion"),
                driver_info: get("driverInfo"),
            });
        }
    };
    for line in text[start..].lines() {
        let trimmed = line.trim();
        if line.starts_with("GPU") && trimmed.ends_with(':') {
            if let Some(fields) = current.take() {
                flush(fields, &mut devices);
            }
            current = Some(Vec::new());
        } else if let (Some(fields), Some((k, v))) = (current.as_mut(), trimmed.split_once('=')) {
            fields.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    if let Some(fields) = current {
        flush(fields, &mut devices);
    }
    devices
}

/// Dispositivo de `clinfo` (formato «Clave: valor» de AMD y Khronos).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenClDevice {
    pub platform: String,
    pub platform_version: Option<String>,
    pub device_type: String,
    pub vendor_id: Option<u32>,
    pub name: String,
    pub driver_version: Option<String>,
}

/// Dispositivos de `clinfo`. Cada `Device Type:` abre un dispositivo de la
/// última plataforma vista; el nombre es `Board name` si existe, si no `Name`.
#[must_use]
pub fn parse_clinfo(text: &str) -> Vec<OpenClDevice> {
    let mut out: Vec<OpenClDevice> = Vec::new();
    let mut platform = String::new();
    let mut platform_version = None;
    let mut has_board = false;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "Platform Name" => platform = value.to_string(),
            "Platform Version" => platform_version = Some(value.to_string()),
            "Device Type" => {
                has_board = false;
                out.push(OpenClDevice {
                    platform: platform.clone(),
                    platform_version: platform_version.clone(),
                    device_type: value.to_string(),
                    vendor_id: None,
                    name: String::new(),
                    driver_version: None,
                });
            }
            "Vendor ID" => {
                if let Some(d) = out.last_mut() {
                    d.vendor_id = hex(value);
                }
            }
            "Board name" => {
                if let Some(d) = out.last_mut() {
                    d.name = value.to_string();
                    has_board = true;
                }
            }
            "Name" | "Device Name" if !has_board => {
                if let Some(d) = out.last_mut() {
                    d.name = value.to_string();
                }
            }
            "Driver version" => {
                if let Some(d) = out.last_mut() {
                    d.driver_version = Some(value.to_string());
                }
            }
            _ => {}
        }
    }
    out.retain(|d| !d.name.is_empty());
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const VULKAN_PCIA: &str =
        include_str!("../../../tests/fixtures/hardware/vulkaninfo-summary-pcia.txt");
    const CLINFO_PCIA: &str = include_str!("../../../tests/fixtures/hardware/clinfo-pcia.txt");

    #[test]
    fn vulkaninfo_real_del_pc_ia() {
        let devices = parse_vulkaninfo(VULKAN_PCIA);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!((d.vendor_id, d.device_id), (0x1002, 0x1586));
        assert_eq!(d.name, "AMD Radeon(TM) 8060S Graphics");
        assert_eq!(d.device_type, "PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU");
        assert_eq!(d.api_version.as_deref(), Some("1.4.315"));
    }

    #[test]
    fn vulkaninfo_varios_dispositivos_y_bloques_rotos() {
        let text = "WARNING: x\n\nDevices:\n========\nGPU0:\n\tvendorID = 0x10de\n\tdeviceID = 0x2684\n\tdeviceName = NVIDIA GeForce RTX 4090\n\tdeviceType = PHYSICAL_DEVICE_TYPE_DISCRETE_GPU\nGPU1:\n\tdeviceName = sin ids\nGPU2:\n\tvendorID = 0x10005\n\tdeviceID = 0x0000\n\tdeviceName = llvmpipe\n\tdeviceType = PHYSICAL_DEVICE_TYPE_CPU\n";
        let devices = parse_vulkaninfo(text);
        assert_eq!(
            devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
            ["NVIDIA GeForce RTX 4090", "llvmpipe"]
        );
        assert!(parse_vulkaninfo("ERROR: vkCreateInstance failed\n").is_empty());
        assert!(parse_vulkaninfo("").is_empty());
    }

    #[test]
    fn clinfo_real_del_pc_ia() {
        let devices = parse_clinfo(CLINFO_PCIA);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.platform, "AMD Accelerated Parallel Processing");
        assert_eq!(
            d.platform_version.as_deref(),
            Some("OpenCL 2.1 AMD-APP (3652.0)")
        );
        assert_eq!(d.vendor_id, Some(0x1002));
        assert_eq!(d.name, "AMD Radeon(TM) 8060S Graphics");
        assert_eq!(d.device_type, "CL_DEVICE_TYPE_GPU");
        assert_eq!(d.driver_version.as_deref(), Some("3652.0 (PAL,LC)"));
    }

    #[test]
    fn clinfo_sin_plataformas_o_basura() {
        assert!(parse_clinfo("Number of platforms:\t\t\t\t 0\n").is_empty());
        assert!(parse_clinfo("clinfo: error while loading OpenCL.dll").is_empty());
    }
}
