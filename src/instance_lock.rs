use fs2::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Seek, Write},
    path::{Path, PathBuf},
};

/// Windows reports a contended `LockFileEx` as `ERROR_LOCK_VIOLATION`, which
/// current Rust categorizes as `Uncategorized` rather than `WouldBlock`; the
/// raw code means the same thing — someone else already holds the lock.
const ERROR_LOCK_VIOLATION: i32 = 33;

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
        Err(error)
            if error.kind() == ErrorKind::WouldBlock
                || error.raw_os_error() == Some(ERROR_LOCK_VIOLATION) =>
        {
            Ok(None)
        }
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
        // Closing the descriptor releases the lock, but a child forked by a
        // parallel test can transiently inherit the open file description
        // before `exec` closes it. Poll instead of demanding immediate release.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let reacquired = loop {
            if let Some(lock) = acquire_at(&path).unwrap() {
                break lock;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "lock was not released within 5s"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        drop(reacquired);
        let _ = fs::remove_file(path);
    }
}
