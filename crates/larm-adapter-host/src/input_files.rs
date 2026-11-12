//! Read-only ownership for a model input, reusing the host's Windows path
//! fence. Unlike package inspection, this API never accepts a fallback walk
//! in place of retained handles. It launches no code and grants no RPC rights.
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::package_paths::PinnedPaths;
use larm_domain::error::{ErrorCode, LarmError};

fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo proteger el archivo de entrada",
        reason,
    )
}
fn io(error: std::io::Error) -> LarmError {
    unavailable("transformation_input_io_failed").with_technical(error.to_string())
}

/// An existing regular file and its ancestors, retained without WRITE/DELETE
/// sharing. Cloning this owner retains the same handles until the last clone
/// closes, including when a consumer cancels an in-progress blocking read.
#[derive(Clone)]
pub struct PinnedReadFile {
    paths: PinnedPaths,
    path: PathBuf,
    file: Arc<File>,
}

impl PinnedReadFile {
    pub fn open(path: &Path) -> Result<Self, LarmError> {
        let mut paths = PinnedPaths::default();
        paths.check_input_file(path)?;
        let file = paths
            .owned_file(path)
            .ok_or_else(|| unavailable("transformation_input_fence_unavailable"))?;
        let owner = Self {
            paths,
            path: path.into(),
            file,
        };
        owner.recheck()?;
        Ok(owner)
    }

    pub fn recheck(&self) -> Result<(), LarmError> {
        let mut paths = self.paths.clone();
        paths.check_input_file(&self.path)?;
        if paths.owned_file(&self.path).is_none() || !self.file.metadata().map_err(io)?.is_file() {
            return Err(unavailable("transformation_input_not_a_pinned_file"));
        }
        Ok(())
    }

    pub fn size(&self) -> Result<u64, LarmError> {
        self.recheck()?;
        Ok(self.file.metadata().map_err(io)?.len())
    }

    /// Offset reads leave the shared file cursor untouched.
    pub fn read_at(&self, offset: u64, bytes: &mut [u8]) -> Result<usize, LarmError> {
        self.recheck()?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            self.file.seek_read(bytes, offset).map_err(io)
        }
        #[cfg(not(windows))]
        {
            let _ = (offset, bytes);
            Err(unavailable("transformation_input_fence_unavailable"))
        }
    }
}

/// An existing directory and its ancestors. Children remain writable while
/// replacement of the owned directory/ancestors is denied on Windows.
#[derive(Clone)]
pub struct PinnedDirectory {
    paths: PinnedPaths,
    path: PathBuf,
    directory: Arc<File>,
}
impl PinnedDirectory {
    pub fn open(path: &Path) -> Result<Self, LarmError> {
        let mut paths = PinnedPaths::default();
        paths.check_allocation_directory(path)?;
        let directory = paths
            .owned_file(path)
            .ok_or_else(|| unavailable("transformation_directory_fence_unavailable"))?;
        let owner = Self {
            paths,
            path: path.into(),
            directory,
        };
        owner.recheck()?;
        Ok(owner)
    }
    pub fn recheck(&self) -> Result<(), LarmError> {
        let mut paths = self.paths.clone();
        paths.check_allocation_directory(&self.path)?;
        if paths.owned_file(&self.path).is_none()
            || !self.directory.metadata().map_err(io)?.is_dir()
        {
            return Err(unavailable("transformation_directory_not_pinned"));
        }
        Ok(())
    }
    pub fn canonical_path(&self) -> Result<PathBuf, LarmError> {
        self.recheck()?;
        let path = std::fs::canonicalize(&self.path).map_err(io)?;
        self.recheck()?;
        Ok(path)
    }
    /// A scoped create/write/delete probe; no user file is opened for writing.
    pub fn check_writable(&self) -> Result<(), LarmError> {
        use std::io::Write;
        self.recheck()?;
        let mut probe = tempfile::Builder::new()
            .prefix(".larm-transform-probe-")
            .tempfile_in(&self.path)
            .map_err(io)?;
        probe.write_all(b"larm").map_err(io)?;
        probe.flush().map_err(io)?;
        probe.close().map_err(io)?;
        self.recheck()
    }
}
