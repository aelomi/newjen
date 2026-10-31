use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// Exclusive ownership of a Newjen data folder until this guard is dropped or its process exits.
/// The lock file stays on disk so every launch locks the same file.
#[derive(Debug)]
#[must_use = "keep the guard alive while using the data folder"]
pub struct InstanceLock {
    _file: File,
    directory: PathBuf,
}

impl InstanceLock {
    /// Creates the data folder if needed and tries to own it without waiting.
    /// Returns `None` when another owner holds it, or an error when it cannot be protected.
    pub fn try_acquire(directory: &Path) -> Result<Option<Self>> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("could not create {}", directory.display()))?;
        let directory = directory
            .canonicalize()
            .with_context(|| format!("could not resolve {}", directory.display()))?;
        let path = directory.join("instance.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("could not open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                _file: file,
                directory,
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("could not lock {}", path.display()))
            }
        }
    }

    /// The absolute, symlink-free data folder protected by this guard.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}
