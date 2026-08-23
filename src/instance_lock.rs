use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Seek, Write},
    path::{Path, PathBuf},
};

/// Keeps exactly one tray process active per user. The OS releases this lock
/// even after a crash, so a stale path can never block a future launch.
pub fn acquire() -> io::Result<Option<File>> {
    let path =
        lock_path().ok_or_else(|| io::Error::other("configuration directory unavailable"))?;
    acquire_at(&path)
}

fn lock_path() -> Option<PathBuf> {
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .map(|directory| directory.join("agent-status-indicator/instance.lock"))
}

fn acquire_at(path: &Path) -> io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    match file.try_lock_exclusive() {
        Ok(()) => {
            file.set_len(0)?;
            file.rewind()?;
            write!(file, "{}\n", std::process::id())?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_process_cannot_acquire_the_same_lock() {
        let path = std::env::temp_dir().join(format!(
            "agent-status-indicator-lock-{}",
            std::process::id()
        ));
        let first = acquire_at(&path).unwrap().expect("first lock");
        assert!(acquire_at(&path).unwrap().is_none());
        drop(first);
        assert!(acquire_at(&path).unwrap().is_some());
        let _ = fs::remove_file(path);
    }
}
