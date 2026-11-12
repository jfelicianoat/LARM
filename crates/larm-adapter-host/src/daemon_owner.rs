//! Physical single-host ownership of a data root. File contents, a PID and
//! an epoch never prove that a worker tree has drained after a host crash.
use crate::input_files::PinnedDirectory;
use larm_domain::error::{ErrorCode, LarmError};
#[cfg(windows)]
use std::fs::OpenOptions;
use std::{
    fs::File,
    path::{Path, PathBuf},
};

pub const OWNER_FILE: &str = ".larm-daemon.lock";
pub const DATABASE_OWNER_FILE: &str = ".larm-database.lock";

struct Anchor {
    directory: PinnedDirectory,
    lease: File,
}

pub struct DaemonOwnership {
    root: PathBuf,
    epoch: String,
    anchors: Vec<Anchor>,
    database: Option<PathBuf>,
}
fn unavailable(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ResourceUnavailable,
        "No se pudo proteger la carpeta de datos de LARM",
        reason,
    )
}
impl DaemonOwnership {
    /// Retain the SAME native ownership for asynchronous work of this host.
    /// This duplicates live handles; epochs and anchor contents confer nothing.
    pub fn retain(&self) -> Result<Self, LarmError> {
        self.recheck()?;
        let anchors = self
            .anchors
            .iter()
            .map(|anchor| {
                Ok(Anchor {
                    directory: anchor.directory.clone(),
                    lease: anchor.lease.try_clone().map_err(|e| {
                        unavailable("daemon_owner_retention_failed").with_technical(e.to_string())
                    })?,
                })
            })
            .collect::<Result<Vec<_>, LarmError>>()?;
        let retained = Self {
            root: self.root.clone(),
            epoch: self.epoch.clone(),
            anchors,
            database: self.database.clone(),
        };
        retained.recheck()?;
        Ok(retained)
    }
    /// Acquired before logging, database migration or recovery. Keep it until
    /// all serving/consumer tasks close. The anchor is never deleted or used
    /// as a stale lock flag; Windows releases the actual handle on a crash.
    pub fn acquire(root: &Path) -> Result<Self, LarmError> {
        let root = std::path::absolute(root)
            .map_err(|e| unavailable("daemon_owner_root_invalid").with_technical(e.to_string()))?;
        let anchor = Anchor::acquire(&root, OWNER_FILE)?;
        Ok(Self {
            root,
            epoch: uuid::Uuid::now_v7().to_string(),
            anchors: vec![anchor],
            database: None,
        })
    }
    /// Also excludes another root configured against the same database
    /// directory. This covers absolute/parent-relative settings without
    /// relying on their display spelling or a port number.
    pub fn acquire_for_database(root: &Path, database_path: &Path) -> Result<Self, LarmError> {
        let mut owner = Self::acquire(root)?;
        let database = std::path::absolute(owner.root.join(database_path)).map_err(|e| {
            unavailable("daemon_database_path_invalid").with_technical(e.to_string())
        })?;
        let name = database
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| unavailable("daemon_database_path_invalid"))?;
        larm_domain::artifacts::normalize_relative(name)?;
        match std::fs::symlink_metadata(&database) {
            Ok(m) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if m.file_attributes() & 0x400 != 0 {
                        return Err(unavailable("daemon_database_reparse"));
                    }
                }
                if !m.is_file() {
                    return Err(unavailable("daemon_database_path_invalid"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(
                    unavailable("daemon_database_path_invalid").with_technical(e.to_string())
                )
            }
        }
        let parent = database
            .parent()
            .ok_or_else(|| unavailable("daemon_database_path_invalid"))?;
        owner
            .anchors
            .push(Anchor::acquire(parent, DATABASE_OWNER_FILE)?);
        owner.database = Some(database);
        owner.recheck()?;
        Ok(owner)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn epoch(&self) -> &str {
        &self.epoch
    }
    pub fn recheck_database(&self, relative: &Path) -> Result<(), LarmError> {
        let expected = std::path::absolute(self.root.join(relative)).map_err(|e| {
            unavailable("daemon_database_path_invalid").with_technical(e.to_string())
        })?;
        if self.database.as_ref() != Some(&expected) {
            return Err(unavailable("daemon_database_owner_mismatch"));
        }
        self.recheck()
    }
    pub fn recheck(&self) -> Result<(), LarmError> {
        for anchor in &self.anchors {
            anchor.directory.recheck()?;
            if !anchor
                .lease
                .metadata()
                .map_err(|e| {
                    unavailable("daemon_owner_inspection_failed").with_technical(e.to_string())
                })?
                .is_file()
            {
                return Err(unavailable("daemon_owner_not_regular_file"));
            }
        }
        Ok(())
    }
}
impl Anchor {
    fn acquire(root: &Path, name: &str) -> Result<Self, LarmError> {
        #[cfg(not(windows))]
        {
            let _ = (root, name);
            Err(unavailable("daemon_owner_platform_unavailable"))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            std::fs::create_dir_all(root).map_err(|e| {
                unavailable("daemon_owner_root_unavailable").with_technical(e.to_string())
            })?;
            let directory = PinnedDirectory::open(root)?;
            let path = root.join(name);
            let lease = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .custom_flags(0x00200000)
                .open(&path)
                .map_err(|e| {
                    if matches!(e.raw_os_error(), Some(32 | 33)) {
                        LarmError::new(
                            ErrorCode::Conflict,
                            "Otra instancia de LARM ya está usando esta base o carpeta de datos",
                            "daemon_data_root_in_use",
                        )
                    } else {
                        unavailable("daemon_owner_open_failed").with_technical(e.to_string())
                    }
                })?;
            let metadata = lease.metadata().map_err(|e| {
                unavailable("daemon_owner_inspection_failed").with_technical(e.to_string())
            })?;
            if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
                return Err(unavailable("daemon_owner_not_regular_file"));
            }
            directory.recheck()?;
            Ok(Self { directory, lease })
        }
    }
}
