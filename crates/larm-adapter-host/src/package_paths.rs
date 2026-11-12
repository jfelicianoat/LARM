//! Scoped path ownership, using safe std handles only (ADR-0011/0012).
//! Clones share handles, never metadata or byte digests. Every checkpoint reads
//! fresh attributes. Public hashes use a new owner; qualified IO may retain its
//! already-owned handles until that qualification closes.

use larm_domain::error::LarmError;
use std::path::Path;

#[cfg(windows)]
#[derive(Clone, Copy)]
enum Sharing {
    Immutable,
    AllocationDirectory,
    InputFile,
}

#[derive(Default, Clone)]
pub(crate) struct PinnedPaths {
    #[cfg(windows)]
    files: std::collections::BTreeMap<std::path::PathBuf, std::sync::Arc<std::fs::File>>,
    #[cfg(windows)]
    fallback: bool,
}

impl PinnedPaths {
    #[cfg(all(test, windows))]
    pub(crate) fn force_test_fallback(&mut self, path: &Path) -> Result<(), LarmError> {
        self.check_with_limit(path, 0)
    }
    #[cfg(windows)]
    pub(crate) fn owned_file(&self, path: &Path) -> Option<std::sync::Arc<std::fs::File>> {
        if self.fallback {
            None
        } else {
            self.files.get(path).cloned()
        }
    }

    #[cfg(not(windows))]
    pub(crate) fn owned_file(&self, _: &Path) -> Option<std::sync::Arc<std::fs::File>> {
        None
    }
    #[cfg(not(windows))]
    pub(crate) fn check(&mut self, path: &Path) -> Result<(), LarmError> {
        crate::package::check_managed_path(path)
    }

    #[cfg(windows)]
    pub(crate) fn check(&mut self, path: &Path) -> Result<(), LarmError> {
        self.check_with_limit(path, 8192)
    }

    /// An allocation root permits child namespace writes while still denying
    /// DELETE/replacement of the root itself. Immutable inputs/package paths
    /// continue to use check and never opt into directory write sharing.
    pub(crate) fn check_allocation_directory(&mut self, path: &Path) -> Result<(), LarmError> {
        #[cfg(windows)]
        {
            self.check_with_sharing(path, 8192, Sharing::AllocationDirectory)
        }
        #[cfg(not(windows))]
        {
            crate::package::check_managed_path(path)
        }
    }

    /// Only the file's bytes are immutable. Its directories deny replacement
    /// but allow child namespace writes, including outputs beside that input.
    pub(crate) fn check_input_file(&mut self, path: &Path) -> Result<(), LarmError> {
        #[cfg(windows)]
        {
            self.check_with_sharing(path, 8192, Sharing::InputFile)
        }
        #[cfg(not(windows))]
        {
            crate::package::check_managed_path(path)
        }
    }

    #[cfg(windows)]
    fn check_with_limit(&mut self, path: &Path, limit: usize) -> Result<(), LarmError> {
        self.check_with_sharing(path, limit, Sharing::Immutable)
    }
    #[cfg(windows)]
    fn check_with_sharing(
        &mut self,
        path: &Path,
        limit: usize,
        sharing: Sharing,
    ) -> Result<(), LarmError> {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        // Preserve the original validation/error behavior before any open.
        if self.fallback
            || !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return crate::package::check_managed_path(path);
        }
        // Parents are acquired first, so a child lookup cannot cross a parent
        // that has already been replaced. Every handle opens the reparse point
        // itself and denies data writes/DELETE until the last owner closes.
        // Allocation directories and input-file ancestors share namespace
        // writes, never DELETE. Input-file bytes and package paths stay strict.
        // FILE_WRITE_ATTRIBUTES is not covered by sharing restrictions, so
        // every held handle's attributes MUST be re-read at each checkpoint.
        let invalid_reparse = || {
            LarmError::new(
                larm_domain::error::ErrorCode::ValidationFailed,
                "El paquete del adaptador no es válido",
                "package_reparse_rejected",
            )
        };
        let ancestors: Vec<_> = path.ancestors().collect();
        for ancestor in ancestors.into_iter().rev() {
            if let Some(file) = self.files.get(ancestor) {
                let metadata = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(_) => {
                        self.fallback = true;
                        self.files.clear();
                        return crate::package::check_managed_path(path);
                    }
                };
                if metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0 {
                    return Err(invalid_reparse());
                }
                continue;
            }
            if self.files.len() >= limit {
                self.fallback = true;
                self.files.clear();
                return crate::package::check_managed_path(path);
            }
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(match sharing {
                    Sharing::Immutable => 1,
                    Sharing::AllocationDirectory => 3,
                    Sharing::InputFile if ancestor != path => 3,
                    Sharing::InputFile => 1,
                })
                .custom_flags(0x0020_0000 | 0x0200_0000) // OPEN_REPARSE_POINT | BACKUP_SEMANTICS.
                .open(ancestor)
                .and_then(|file| {
                    let metadata = file.metadata()?;
                    Ok((file, metadata))
                });
            match opened {
                Ok((file, metadata)) => {
                    if metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
                    {
                        return Err(invalid_reparse());
                    }
                    self.files
                        .insert(ancestor.to_path_buf(), std::sync::Arc::new(file));
                }
                // A missing path is never cached; a later checkpoint inspects
                // it again. The SDK's actual read retains its own IO checks.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                // Sharing/access/UNC limitations use the original full walk
                // for this and every subsequent checkpoint, without a cache.
                Err(_) => {
                    self.fallback = true;
                    self.files.clear();
                    return crate::package::check_managed_path(path);
                }
            }
        }
        Ok(())
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn pinned_paths_match_original_fence_for_path_matrix_and_long_paths() {
        let root = tempfile::tempdir().unwrap();
        let unicode = root.path().join("área 模型");
        std::fs::create_dir(&unicode).unwrap();
        std::fs::write(unicode.join("file.bin"), b"data").unwrap();
        let mut long = unicode.clone();
        for _ in 0..8 {
            long.push("directory_with_a_long_name_for_windows");
        }
        std::fs::create_dir_all(&long).unwrap();
        let long_file = long.join("payload.bin");
        std::fs::write(&long_file, b"long path").unwrap();
        for path in [
            root.path().to_path_buf(),
            unicode.clone(),
            unicode.join("file.bin"),
            unicode.join("missing/child.bin"),
            unicode.join("./file.bin"),
            unicode.join("../file.bin"),
            Path::new("relative/file.bin").to_path_buf(),
            unicode.join("file.bin\0ignored"),
            long_file.clone(),
            long_file.canonicalize().unwrap(),
            long.join("missing.bin"),
        ] {
            let expected = crate::package::check_managed_path(&path).map_err(|e| e.details.reason);
            assert_eq!(
                PinnedPaths::default()
                    .check(&path)
                    .map_err(|e| e.details.reason),
                expected,
                "{path:?}"
            );
        }
    }

    #[test]
    fn ancestor_and_file_replacement_are_denied_until_the_scan_ends() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let input = parent.join("input.bin");
        std::fs::write(&input, b"all bytes remain readable").unwrap();
        let mut paths = PinnedPaths::default();
        paths.check(&input).unwrap();
        assert!(!paths.fallback);
        assert!(std::fs::write(&input, b"mutation").is_err());
        assert!(std::fs::rename(&input, parent.join("moved.bin")).is_err());
        assert!(std::fs::rename(&parent, root.path().join("moved")).is_err());
        assert_eq!(std::fs::read(&input).unwrap(), b"all bytes remain readable");
        paths.check(&input).unwrap();
        drop(paths);
        std::fs::write(&input, b"mutation after close").unwrap();
        std::fs::rename(&parent, root.path().join("moved")).unwrap();
    }

    #[test]
    fn cloned_ownership_keeps_file_and_ancestor_fences_until_the_last_owner_closes() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("file.bin");
        std::fs::write(&path, b"owned bytes").unwrap();
        let mut first = PinnedPaths::default();
        first.check(&path).unwrap();
        let mut last = first.clone();
        drop(first);
        last.check(&path).unwrap();
        assert!(std::fs::write(&path, b"changed").is_err());
        assert!(std::fs::rename(&parent, root.path().join("moved")).is_err());
        drop(last);
        std::fs::write(&path, b"changed after close").unwrap();
        std::fs::rename(&parent, root.path().join("moved")).unwrap();
    }

    #[test]
    fn scoped_hashes_rewalk_inventory_and_read_complete_bytes_without_reusing_a_digest() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(root.path().join("nested/weights.bin"), vec![7_u8; 3 << 20]).unwrap();
        std::fs::write(root.path().join("manifest.json"), b"ignored manifest").unwrap();
        let mut owner = PinnedPaths::default();
        let control = crate::package::PackageControl::default();
        let first =
            crate::package::controlled_hash_with_paths(root.path(), &control, &mut owner).unwrap();
        assert_eq!(
            first,
            larm_adapter_sdk::permissions::package_hash(root.path(), &["manifest.json"]).unwrap()
        );
        assert!(!owner.fallback);
        // Directory pins do not stand in for an inventory scan. A new file is
        // included in the next complete hash, even through cloned ownership.
        std::fs::write(root.path().join("added.bin"), b"new entry").unwrap();
        let mut next = owner.clone();
        let second =
            crate::package::controlled_hash_with_paths(root.path(), &control, &mut next).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            second,
            larm_adapter_sdk::permissions::package_hash(root.path(), &["manifest.json"]).unwrap()
        );
        drop(owner);
        drop(next);
        std::fs::write(root.path().join("nested/weights.bin"), b"new content").unwrap();
        assert_ne!(
            second,
            crate::package::controlled_hash(root.path(), &control).unwrap()
        );
    }

    #[test]
    fn bounded_handle_fallback_releases_ownership_and_keeps_validating_paths() {
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("input.bin");
        std::fs::write(&input, b"before").unwrap();
        let mut paths = PinnedPaths::default();
        paths.check_with_limit(&input, 1).unwrap();
        assert!(paths.fallback);
        assert!(paths.files.is_empty());
        std::fs::write(&input, b"after fallback").unwrap();
        let invalid = root.path().join("../escape.bin");
        assert_eq!(
            paths.check(&invalid).unwrap_err().details.reason,
            "package_root_invalid"
        );
    }

    #[test]
    fn write_attributes_cannot_turn_a_previously_checked_directory_into_an_admitted_link() {
        use std::os::windows::process::CommandExt;
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("empty");
        let target = root.path().join("target");
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("sentinel.bin"), b"target remains untouched").unwrap();
        let mut paths = PinnedPaths::default();
        paths.check(&directory).unwrap();
        assert!(!paths.fallback);
        // A retained qualification and its per-check clones must still read
        // FILE_WRITE_ATTRIBUTES changes from the live handle, never metadata
        // captured when that handle was first acquired.
        let mut retained = paths.clone();
        drop(paths);
        retained.check(&directory).unwrap();
        let mut paths = retained.clone();
        drop(retained);
        // FILE_WRITE_ATTRIBUTES does not participate in FILE_SHARE_WRITE.
        // Exercise the real kernel operation via the already pinned Python
        // test runtime; no Rust FFI exception or user-owned path is involved.
        let probe = r#"
import ctypes, struct, sys
from ctypes import wintypes
kernel = ctypes.WinDLL('kernel32', use_last_error=True)
kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p]
kernel.CreateFileW.restype = ctypes.c_void_p
kernel.DeviceIoControl.argtypes = [ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p]
kernel.DeviceIoControl.restype = wintypes.BOOL
kernel.CloseHandle.argtypes = [ctypes.c_void_p]
directory, target = sys.argv[1:]
substitute = ('\\??\\' + target).encode('utf-16le')
display = target.encode('utf-16le')
names = substitute + b'\0\0' + display + b'\0\0'
data = struct.pack('<IHHHHHH', 0xA0000003, 8 + len(names), 0, 0, len(substitute), len(substitute)+2, len(display)) + names
buffer = ctypes.create_string_buffer(data)
handle = kernel.CreateFileW(directory, 0x100, 7, None, 3, 0x02200000, None)
if handle == ctypes.c_void_p(-1).value:
    raise ctypes.WinError(ctypes.get_last_error())
try:
    returned = wintypes.DWORD()
    if not kernel.DeviceIoControl(handle, 0x900A4, buffer, len(data), None, 0, ctypes.byref(returned), None):
        raise ctypes.WinError(ctypes.get_last_error())
finally:
    kernel.CloseHandle(handle)
"#;
        let python =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.venv-tools/Scripts/python.exe");
        let output = std::process::Command::new(python)
            .args(["-I", "-B", "-c", probe])
            .arg(&directory)
            .arg(&target)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "write-attributes probe: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            paths.check(&directory).unwrap_err().details.reason,
            "package_reparse_rejected"
        );
        drop(paths);
        std::fs::remove_dir(&directory).unwrap(); // Owned junction only.
        assert_eq!(
            std::fs::read(target.join("sentinel.bin")).unwrap(),
            b"target remains untouched"
        );
    }
}
