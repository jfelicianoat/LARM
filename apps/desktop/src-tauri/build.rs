use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn files(root: &Path, dir: &str) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    for entry in std::fs::read_dir(root.join(dir)).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let relative = format!("{dir}/{}", entry.file_name().to_string_lossy());
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            result.extend(files(root, &relative)?);
        } else if kind.is_file() && relative != "dist/ui-build.json" {
            result.push(relative);
        }
    }
    Ok(result)
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hashes(root: &Path, names: &[String]) -> Result<serde_json::Value, String> {
    let mut result = BTreeMap::new();
    for name in names {
        println!("cargo:rerun-if-changed={}", root.join(name).display());
        let bytes = std::fs::read(root.join(name)).map_err(|e| e.to_string())?;
        result.insert(name, hash(&bytes));
    }
    serde_json::to_value(result).map_err(|e| e.to_string())
}

fn verify_ui(root: &Path) -> Result<String, String> {
    let stamp = root.join("dist/ui-build.json");
    println!("cargo:rerun-if-changed={}", stamp.display());
    println!("cargo:rerun-if-changed={}", root.join("src").display());
    println!("cargo:rerun-if-changed={}", root.join("dist").display());
    let bytes = std::fs::read(&stamp).map_err(|e| e.to_string())?;
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let mut sources = files(root, "src")?;
    sources.extend(
        [
            "package.json",
            "index.html",
            "vite.config.ts",
            "tsconfig.json",
            "tsconfig.node.json",
            "tsconfig.app.json",
            "build-ui-stamp.mjs",
        ]
        .map(String::from),
    );
    if manifest["schema_version"] != 1
        || manifest["sources"] != hashes(root, &sources)?
        || manifest["assets"] != hashes(root, &files(root, "dist")?)?
    {
        return Err("los fuentes o assets no coinciden con la interfaz compilada".into());
    }
    Ok(hash(&bytes))
}

fn main() {
    let fingerprint = if tauri_build::is_dev() {
        "DEV_SERVER".into()
    } else {
        let root =
            PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default()).join("..");
        match verify_ui(&root) {
            Ok(hash) => hash,
            Err(error) => {
                eprintln!("frontend_desactualizado: {error}. Ejecuta npm run build antes de compilar el escritorio.");
                std::process::exit(1);
            }
        }
    };
    println!("cargo:rustc-env=LARM_UI_MANIFEST_SHA256={fingerprint}");
    tauri_build::build();
}
