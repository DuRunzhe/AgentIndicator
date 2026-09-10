//! Conversation titles from Codex's own state database.
//!
//! The desktop app names its sidebar tabs from the first user message and keeps
//! them in `~/.codex/state_<n>.sqlite`. Titles are decoration only: a missing
//! database, an unknown thread or a future schema simply yields no title.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// How long a resolved title (and the discovered database path) stays valid.
/// The app names a conversation once it has a first user message, so a short
/// cache keeps labels fresh without opening SQLite on every scan.
const CACHE_TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct CodexTitles {
    database: Option<(PathBuf, Instant)>,
    cached: HashMap<String, (Instant, Option<String>)>,
}

impl CodexTitles {
    pub fn title(&mut self, thread: &str) -> Option<String> {
        if let Some((at, title)) = self.cached.get(thread) {
            if at.elapsed() < CACHE_TTL {
                return title.clone();
            }
        }
        let title = self.lookup(thread);
        self.cached
            .insert(thread.to_owned(), (Instant::now(), title.clone()));
        title
    }

    fn lookup(&mut self, thread: &str) -> Option<String> {
        let database = self.database()?.clone();
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .ok()?;
        connection.busy_timeout(Duration::from_millis(250)).ok()?;
        let mut statement = connection
            .prepare_cached("select title from threads where id = ?1")
            .ok()?;
        let title: Option<String> = statement
            .query_row([thread], |row| row.get(0))
            .optional()
            .ok()?;
        title
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|title| !title.is_empty())
    }

    /// Codex versions the file name (`state_5.sqlite`), so discover the newest.
    fn database(&mut self) -> Option<&PathBuf> {
        if let Some((path, at)) = &self.database {
            if at.elapsed() < CACHE_TTL && path.is_file() {
                return self.database.as_ref().map(|(path, _)| path);
            }
        }
        let root = dirs::home_dir()?.join(".codex");
        let path = std::fs::read_dir(root)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("state_") && name.ends_with(".sqlite"))
            })
            .max_by_key(|path| state_version(path).unwrap_or(0))?;
        self.database = Some((path, Instant::now()));
        self.database.as_ref().map(|(path, _)| path)
    }
}

fn state_version(path: &Path) -> Option<u64> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("state_")?
        .parse()
        .ok()
}
