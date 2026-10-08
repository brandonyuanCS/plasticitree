use crate::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

pub(crate) fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::Locked),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

// Unix can sync a directory handle. Windows std does not expose an equivalent
// durability protocol here: only process-crash recovery is claimed on Windows.
pub(crate) fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
