//! Recorrido de una carpeta para importar modelos (P017): lista los
//! ficheros con su tamaño y una cabecera breve, sin seguir enlaces ni
//! junctions y sin ejecutar nada. La clasificación es de `larm-models`.

use std::io::Read;
use std::path::{Path, PathBuf};

/// Bytes de cabecera que se leen de cada fichero.
pub const HEAD_BYTES: usize = 64 * 1024;
/// Límite de ficheros por recorrido (una carpeta de modelos no tiene más).
pub const MAX_FILES: usize = 100_000;

/// Fichero encontrado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Ruta POSIX relativa a la carpeta recorrida.
    pub relative: String,
    pub path: PathBuf,
    pub size: u64,
    /// Primeros bytes (hasta [`HEAD_BYTES`]).
    pub head: Vec<u8>,
}

/// Lo que el recorrido no sigue ni lee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub relative: String,
    pub reason: String,
}

fn is_reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn count_scanned(count: &mut usize) -> std::io::Result<()> {
    *count = count
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("demasiados elementos en la carpeta"))?;
    if *count > MAX_FILES {
        return Err(std::io::Error::other("demasiados ficheros en la carpeta"));
    }
    Ok(())
}

fn head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; HEAD_BYTES];
    let mut file = std::fs::File::open(path)?;
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Recorre `dir` en orden determinista. Enlaces simbólicos y junctions no se
/// siguen (se informan); un fichero ilegible se informa y se sigue.
///
/// # Errors
/// La carpeta no existe o no es legible, o supera [`MAX_FILES`].
pub fn walk(dir: &Path) -> std::io::Result<(Vec<Found>, Vec<Skipped>)> {
    let mut found = Vec::new();
    let mut skipped = Vec::new();
    let mut files_seen = 0;
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((current, prefix)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&current)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let meta = std::fs::symlink_metadata(entry.path())?;
            if is_reparse(&meta) {
                count_scanned(&mut files_seen)?;
                skipped.push(Skipped {
                    relative,
                    reason: "reparse_point_not_followed".into(),
                });
            } else if meta.is_dir() {
                stack.push((entry.path(), relative));
            } else if meta.is_file() {
                count_scanned(&mut files_seen)?;
                match head(&entry.path()) {
                    Ok(head) => found.push(Found {
                        relative,
                        path: entry.path(),
                        size: meta.len(),
                        head,
                    }),
                    Err(e) => skipped.push(Skipped {
                        relative,
                        reason: format!("unreadable: {}", e.kind()),
                    }),
                }
            }
        }
    }
    found.sort_by(|a, b| a.relative.cmp(&b.relative));
    skipped.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok((found, skipped))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn recorrido_determinista_sin_seguir_junctions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("b/sub")).unwrap();
        std::fs::write(dir.path().join("b/sub/z.gguf"), b"GGUF").unwrap();
        std::fs::write(dir.path().join("a.txt"), vec![7u8; HEAD_BYTES + 10]).unwrap();
        let (found, skipped) = walk(dir.path()).unwrap();
        let names: Vec<_> = found.iter().map(|f| f.relative.as_str()).collect();
        assert_eq!(names, vec!["a.txt", "b/sub/z.gguf"]);
        assert_eq!(found[0].head.len(), HEAD_BYTES, "solo la cabecera");
        assert_eq!(found[0].size, (HEAD_BYTES + 10) as u64);
        assert!(skipped.is_empty());
        #[cfg(windows)]
        {
            let link = dir.path().join("enlace");
            let ok = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(dir.path().join("b"))
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "mklink /J no requiere privilegios");
            let (found, skipped) = walk(dir.path()).unwrap();
            assert_eq!(found.len(), 2, "la junction no se sigue");
            assert_eq!(skipped[0].reason, "reparse_point_not_followed");
        }
        assert!(walk(&dir.path().join("no-existe")).is_err());
    }

    #[test]
    fn limite_incluye_ficheros_encontrados_y_elementos_omitidos() {
        let mut count = MAX_FILES - 1;
        count_scanned(&mut count).unwrap();
        assert_eq!(count, MAX_FILES);
        assert!(count_scanned(&mut count).is_err());
    }
}
