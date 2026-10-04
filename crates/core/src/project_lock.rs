//! A project session owns an OS lock. Clones and reloads in this process share
//! it; another process cannot edit or clean up this project's assets.
use crate::CoreError;
use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub(crate) fn acquire(root: &Path) -> Result<Arc<File>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<File>>>> = OnceLock::new();
    let mut locks = LOCKS.get_or_init(Mutex::default).lock().unwrap();
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(root).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("project.lock"))
        .context("Opening project lock")?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(CoreError::ProjectLocked(root.to_owned()).into());
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(error).context("Locking project"),
    }
    let file = Arc::new(file);
    locks.insert(root.to_owned(), Arc::downgrade(&file));
    Ok(file)
}
