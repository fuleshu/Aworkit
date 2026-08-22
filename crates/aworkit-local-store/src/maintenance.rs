//! Cross-process maintenance gate shared by every local-store writer.

use std::{
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
};

use fs2::FileExt;

#[derive(Clone, Debug)]
pub(crate) struct MaintenanceGate {
    lock_path: PathBuf,
}

impl MaintenanceGate {
    pub(crate) fn for_root(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        let parent = root.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "store root has no parent")
        })?;
        let name = root
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "store root has no portable final component",
                )
            })?;
        Ok(Self {
            lock_path: parent.join(format!(".{name}.aworkit-maintenance.lock")),
        })
    }

    pub(crate) fn shared(&self) -> io::Result<MaintenanceLease> {
        let file = self.open()?;
        file.lock_shared()?;
        Ok(MaintenanceLease { file })
    }

    pub(crate) fn exclusive(&self) -> io::Result<MaintenanceLease> {
        let file = self.open()?;
        file.lock_exclusive()?;
        Ok(MaintenanceLease { file })
    }

    fn open(&self) -> io::Result<File> {
        Ok(OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.lock_path)?)
    }
}

pub(crate) struct MaintenanceLease {
    file: File,
}

impl Drop for MaintenanceLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}
