//! Componentes de cada API de aceleración en disco: DLL buscadas en
//! `System32`, en el `PATH` y en las raíces que declaran sus SDK. Encontrar
//! una DLL es «detectado», nunca «operativo».

use std::path::{Path, PathBuf};

/// Cómo se reconoce el fichero de un componente.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pattern {
    /// Nombre exacto (sin distinguir mayúsculas).
    Exact(&'static str),
    /// `prefijo*.dll` (versión en el nombre: `nvinfer_10.dll`, `sycl8.dll`).
    Prefix(&'static str),
}

impl Pattern {
    fn matches(self, file_name: &str) -> bool {
        let name = file_name.to_ascii_lowercase();
        match self {
            Self::Exact(n) => name == n.to_ascii_lowercase(),
            Self::Prefix(p) => {
                name.starts_with(&p.to_ascii_lowercase())
                    && Path::new(&name).extension().is_some_and(|e| e == "dll")
            }
        }
    }
}

/// Componentes de una API: el loader o runtime que la hace posible.
#[derive(Debug, Clone, Copy)]
pub struct ApiComponents {
    pub api: &'static str,
    pub patterns: &'static [Pattern],
    /// Variables de entorno de su SDK; se busca en `<raíz>` y `<raíz>\bin`.
    pub sdk_env: &'static [&'static str],
}

/// Las doce APIs de la spec §22, en su orden.
pub const APIS: &[ApiComponents] = &[
    ApiComponents {
        api: "vulkan",
        patterns: &[Pattern::Exact("vulkan-1.dll")],
        sdk_env: &["VULKAN_SDK"],
    },
    ApiComponents {
        api: "cuda",
        patterns: &[Pattern::Exact("nvcuda.dll")],
        sdk_env: &["CUDA_PATH"],
    },
    ApiComponents {
        api: "rocm",
        patterns: &[
            Pattern::Exact("amdhip64_7.dll"),
            Pattern::Exact("amdhip64_6.dll"),
            Pattern::Exact("amdhip64.dll"),
        ],
        sdk_env: &["HIP_PATH"],
    },
    ApiComponents {
        api: "directml",
        patterns: &[Pattern::Exact("DirectML.dll")],
        sdk_env: &[],
    },
    ApiComponents {
        api: "winml",
        patterns: &[Pattern::Exact("winml.dll")],
        sdk_env: &[],
    },
    ApiComponents {
        api: "opencl",
        patterns: &[Pattern::Exact("OpenCL.dll")],
        sdk_env: &[],
    },
    ApiComponents {
        api: "sycl",
        patterns: &[Pattern::Prefix("sycl")],
        sdk_env: &["ONEAPI_ROOT", "CMPLR_ROOT"],
    },
    ApiComponents {
        api: "vitisai",
        patterns: &[Pattern::Exact("onnxruntime_vitisai_ep.dll")],
        sdk_env: &["RYZEN_AI_INSTALLATION_PATH"],
    },
    ApiComponents {
        api: "migraphx",
        patterns: &[Pattern::Prefix("migraphx")],
        sdk_env: &["HIP_PATH"],
    },
    ApiComponents {
        api: "openvino",
        patterns: &[Pattern::Exact("openvino.dll")],
        sdk_env: &["INTEL_OPENVINO_DIR"],
    },
    ApiComponents {
        api: "qnn",
        patterns: &[Pattern::Exact("QnnHtp.dll")],
        sdk_env: &["QNN_SDK_ROOT"],
    },
    ApiComponents {
        api: "tensorrt",
        patterns: &[Pattern::Prefix("nvinfer")],
        sdk_env: &["TENSORRT_PATH"],
    },
];

/// Carpetas donde buscar: `System32`, el `PATH` y las raíces de los SDK.
/// Sin duplicados y en ese orden (el de búsqueda de Windows).
#[must_use]
pub fn search_dirs(
    system32: &Path,
    path_var: Option<&std::ffi::OsStr>,
    env: &dyn Fn(&str) -> Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut dirs = vec![system32.to_path_buf()];
    if let Some(path) = path_var {
        dirs.extend(std::env::split_paths(path));
    }
    for api in APIS {
        for var in api.sdk_env {
            if let Some(root) = env(var) {
                dirs.push(root.join("bin"));
                dirs.push(root);
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    dirs.retain(|d| {
        !d.as_os_str().is_empty() && seen.insert(d.to_string_lossy().to_ascii_lowercase())
    });
    dirs
}

/// Fichero encontrado para una API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub api: &'static str,
    pub path: PathBuf,
}

/// Primera coincidencia de cada patrón de cada API, en orden de carpetas.
#[must_use]
pub fn scan(dirs: &[PathBuf]) -> Vec<Found> {
    let listings: Vec<(PathBuf, Vec<String>)> = dirs
        .iter()
        .filter_map(|d| {
            let names = std::fs::read_dir(d)
                .ok()?
                .filter_map(Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            Some((d.clone(), names))
        })
        .collect();
    let mut found = Vec::new();
    for api in APIS {
        for pattern in api.patterns {
            let hit = listings.iter().find_map(|(dir, names)| {
                let mut matches: Vec<&String> =
                    names.iter().filter(|n| pattern.matches(n)).collect();
                matches.sort();
                matches.first().map(|n| dir.join(n))
            });
            if let Some(path) = hit {
                found.push(Found { api: api.api, path });
            }
        }
    }
    found
}

/// sha256 de un fichero, en streaming.
///
/// # Errors
/// Error de lectura.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn patrones_exactos_y_por_prefijo() {
        assert!(Pattern::Exact("DirectML.dll").matches("directml.DLL"));
        assert!(!Pattern::Exact("DirectML.dll").matches("DirectML.Debug.dll"));
        assert!(Pattern::Prefix("nvinfer").matches("nvinfer_10.dll"));
        assert!(!Pattern::Prefix("nvinfer").matches("nvinfer_10.lib"));
        assert!(Pattern::Prefix("sycl").matches("sycl8.dll"));
    }

    #[test]
    fn busca_en_carpetas_en_orden_y_detecta_ausencias() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("vulkan-1.dll"), b"a").unwrap();
        std::fs::write(b.path().join("vulkan-1.dll"), b"b").unwrap();
        std::fs::write(b.path().join("nvinfer_10.dll"), b"t").unwrap();
        std::fs::create_dir(b.path().join("OpenCL.dll")).unwrap(); // carpeta, no fichero
        let found = scan(&[
            a.path().to_path_buf(),
            b.path().to_path_buf(),
            PathBuf::from("Z:\\no-existe"),
        ]);
        let by_api = |api: &str| {
            found
                .iter()
                .filter(|f| f.api == api)
                .map(|f| f.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(by_api("vulkan"), [a.path().join("vulkan-1.dll")]);
        assert_eq!(by_api("tensorrt"), [b.path().join("nvinfer_10.dll")]);
        assert!(by_api("opencl").is_empty());
        assert!(by_api("cuda").is_empty());
        assert_eq!(
            sha256_file(&a.path().join("vulkan-1.dll")).unwrap(),
            "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb"
        );
    }

    #[test]
    fn carpetas_sin_duplicados_con_raices_de_sdk() {
        let path = std::env::join_paths(["C:\\Windows\\System32", "C:\\tools", ""]).unwrap();
        let env = |v: &str| (v == "HIP_PATH").then(|| PathBuf::from("C:\\ROCm\\6.4"));
        let dirs = search_dirs(Path::new("C:\\WINDOWS\\system32"), Some(&path), &env);
        assert_eq!(
            dirs,
            [
                PathBuf::from("C:\\WINDOWS\\system32"),
                PathBuf::from("C:\\tools"),
                PathBuf::from("C:\\ROCm\\6.4\\bin"),
                PathBuf::from("C:\\ROCm\\6.4"),
            ]
        );
    }
}
