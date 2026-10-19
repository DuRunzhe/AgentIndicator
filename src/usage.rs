//! Token totals recomputed from each agent's own records.
//!
//! Nothing here is persisted: the numbers on the tray's usage table are derived
//! fresh from the session files the agents write. That keeps the table honest —
//! there is no second ledger that can drift from the agent's own account — and
//! makes any period cheap enough to recompute: a full history pass is well under
//! a second on a laptop-sized corpus, and every file is skipped by its
//! modification time when it cannot hold a record inside the window.
//!
//! Where a source keeps a cumulative counter (Codex's `total_token_usage`,
//! OpenCode's session rollup, DeepSeek's `tokenUsage`), the counter is treated as
//! a snapshot to difference, never as a value to add up: summing cumulative
//! records is what multiplies a session by the number of turns it wrote.
//!
//! A scan is repeated every [`REFRESH_INTERVAL`], so each parsed file is cached
//! in memory and keyed by its modification time and length (see [`Scanner`]).
//! That cache holds no history of its own — it is a parse cache over the source
//! files, and discarding it costs one re-read, never a wrong number.

use crate::model::RESOURCE_AGENTS;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    sync::{mpsc::RecvTimeoutError, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use time::{Date, Month, OffsetDateTime, Time, UtcOffset};

/// How often the table is recomputed while the tray is running. The scan reads
/// files only, so it is independent of the two-second status cadence: a daily or
/// monthly total does not need to move every two seconds.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// How long a parsed file stays in the scan cache without being used.
const CACHE_TTL: Duration = Duration::from_secs(86_400);
/// Upper bound on cached files, so a machine with years of logs cannot grow the
/// tray's memory without limit.
const CACHE_FILES: usize = 4_000;

/// Which calendar window the table covers, in local time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    Day,
    Week,
    Month,
}

impl Period {
    pub const ALL: [Period; 3] = [Period::Day, Period::Week, Period::Month];

    /// The value stored in the config file.
    pub fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// Read a stored value, falling back to the day view for an unknown key.
    pub fn from_key(key: &str) -> Self {
        match key {
            "week" => Self::Week,
            "month" => Self::Month,
            _ => Self::Day,
        }
    }

    /// The next period in the day → week → month cycle of the switch row.
    pub fn next(self) -> Self {
        match self {
            Self::Day => Self::Week,
            Self::Week => Self::Month,
            Self::Month => Self::Day,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => crate::i18n::text("usage_day"),
            Self::Week => crate::i18n::text("usage_week"),
            Self::Month => crate::i18n::text("usage_month"),
        }
    }
}

/// One agent's token counts for the window.
///
/// The four counters share one definition across agents so the columns mean the
/// same thing in every row:
///
/// * `input` — every input token, cache reads and cache writes included. Sources
///   disagree on whether their own input counter already contains the cached
///   part (Codex's does, Claude's and Pi's do not), so the scanners normalize
///   into this sum rather than exposing the raw field.
/// * `input_cached` — the share of `input` served from the prompt cache.
/// * `output` — generated tokens, reasoning included where the source reports it
///   separately.
/// * `output_cached` — no agent has output caching yet; kept so a source that
///   grows one lands in the table without a column change.
/// * `cost` — a real amount only where the agent records one (OpenCode, Pi).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct TokenCounts {
    pub input: u64,
    pub input_cached: u64,
    pub output: u64,
    pub output_cached: u64,
    pub cost: Option<f64>,
}

impl TokenCounts {
    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.input_cached = self.input_cached.saturating_add(other.input_cached);
        self.output = self.output.saturating_add(other.output);
        self.output_cached = self.output_cached.saturating_add(other.output_cached);
        self.cost = match (self.cost, other.cost) {
            (None, None) => None,
            (left, right) => Some(left.unwrap_or(0.0) + right.unwrap_or(0.0)),
        };
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct AgentUsage {
    pub kind: &'static str,
    /// `None` when the agent has no records in the window, so the row reads as
    /// "no data" instead of a row of zeros.
    pub counts: Option<TokenCounts>,
}

/// The whole table: one row per agent in [`RESOURCE_AGENTS`] order, plus the sum.
#[derive(Clone, Debug, Serialize)]
pub struct UsageSnapshot {
    pub period: Period,
    pub agents: Vec<AgentUsage>,
    pub total: TokenCounts,
    /// True when at least one agent reported something, so the total row knows
    /// the difference between "everything is zero" and "nothing was read".
    pub has_data: bool,
}

impl UsageSnapshot {
    pub fn empty(period: Period) -> Self {
        Self {
            period,
            agents: RESOURCE_AGENTS
                .iter()
                .map(|kind| AgentUsage { kind, counts: None })
                .collect(),
            total: TokenCounts::default(),
            has_data: false,
        }
    }
}

/// Where each agent keeps its records. Injected so the scanners can be tested
/// against a temporary tree instead of the user's home directory.
#[derive(Clone, Debug)]
pub struct Roots {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub opencode: PathBuf,
    pub pi: PathBuf,
    pub deepseek: PathBuf,
}

impl Roots {
    pub fn from_home(home: &Path) -> Self {
        Self {
            claude: home.join(".claude/projects"),
            codex: home.join(".codex/sessions"),
            opencode: home.join(".local/share/opencode/opencode.db"),
            pi: home.join(".pi/agent/sessions"),
            deepseek: home.join(".dsh/storages/session_projcache/sessions"),
        }
    }

    pub fn system() -> Option<Self> {
        let home = dirs::home_dir()?;
        let mut roots = Self::from_home(&home);
        // The desktop application can be pointed at another profile, and this
        // resolver already honours `DSH_HOME`.
        if let Some(projection) = crate::deepseek_desktop::session_cache_root() {
            roots.deepseek = projection;
        }
        Some(roots)
    }
}

/// One-shot scan, as `--diagnose-usage` needs it.
pub fn scan(period: Period) -> UsageSnapshot {
    match Roots::system() {
        Some(roots) => Scanner::new().scan(&roots, period, SystemTime::now()),
        None => UsageSnapshot::empty(period),
    }
}

/// Start of the window in epoch milliseconds, in local time.
pub fn window_start(period: Period, now: SystemTime) -> i64 {
    let now = OffsetDateTime::from_unix_timestamp_nanos(epoch_millis(now) as i128 * 1_000_000)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    let offset = UtcOffset::local_offset_at(now).unwrap_or(UtcOffset::UTC);
    window_start_at(period, now, offset)
}

/// The same boundary for an explicit offset, which is what the tests exercise.
fn window_start_at(period: Period, now: OffsetDateTime, offset: UtcOffset) -> i64 {
    let local = now.to_offset(offset);
    let mut date = local.date();
    match period {
        Period::Day => {}
        Period::Week => {
            // Weeks start on Monday, matching the calendar the menu's other
            // ranges already follow.
            for _ in 0..date.weekday().number_days_from_monday() {
                match date.previous_day() {
                    Some(previous) => date = previous,
                    None => break,
                }
            }
        }
        Period::Month => {
            if let Ok(first) = Date::from_calendar_date(local.year(), local.month(), 1) {
                date = first;
            }
        }
    }
    let midnight = date.with_time(Time::MIDNIGHT);
    // Resolve the offset again at the boundary so a DST change between midnight
    // and "now" cannot slide the window by an hour.
    let offset = UtcOffset::local_offset_at(midnight.assume_utc()).unwrap_or(offset);
    midnight.assume_offset(offset).unix_timestamp() * 1_000
}

/// A scan that keeps its parsed files between calls.
///
/// Every window this table offers starts at local midnight, so each file is
/// reduced to one bucket per local day and the window is a bucket filter. A
/// file that has not changed since the previous scan is then free — one `stat`
/// instead of one parse — which is what keeps a 10-second refresh inside the
/// tray's idle-CPU budget on a machine with hundreds of megabytes of logs.
#[derive(Default)]
pub struct Scanner {
    files: HashMap<PathBuf, CachedFile>,
    last_prune: Option<Instant>,
}

struct CachedFile {
    modified: Option<SystemTime>,
    length: u64,
    bucket: Bucket,
    used: Instant,
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan(&mut self, roots: &Roots, period: Period, now: SystemTime) -> UsageSnapshot {
        let start = window_start(period, now);
        let end = epoch_millis(now);
        let mut offsets = Offsets::default();
        let start_day = offsets.day(start);
        let last_day = offsets.day(end);
        let per_agent = [
            self.jsonl_totals(
                Kind::Claude,
                &roots.claude,
                start,
                start_day,
                last_day,
                &mut offsets,
            ),
            self.jsonl_totals(
                Kind::Codex,
                &roots.codex,
                start,
                start_day,
                last_day,
                &mut offsets,
            ),
            opencode(&roots.opencode, start, end),
            deepseek(&roots.deepseek, start, end),
            self.jsonl_totals(
                Kind::Pi,
                &roots.pi,
                start,
                start_day,
                last_day,
                &mut offsets,
            ),
        ];
        self.prune();
        let mut snapshot = UsageSnapshot::empty(period);
        for (agent, totals) in snapshot.agents.iter_mut().zip(per_agent) {
            if totals.records > 0 {
                agent.counts = Some(totals.counts);
                snapshot.total.add(&totals.counts);
                snapshot.has_data = true;
            }
        }
        snapshot
    }

    fn jsonl_totals(
        &mut self,
        kind: Kind,
        root: &Path,
        start: i64,
        start_day: i32,
        last_day: i32,
        offsets: &mut Offsets,
    ) -> Totals {
        let mut totals = Totals::default();
        for file in jsonl_files(root, 6) {
            if !touched_since(&file, start) {
                continue;
            }
            let (records, counts) = self
                .bucket(kind, &file, offsets)
                .between(start_day, last_day);
            totals.records += records;
            totals.counts.add(&counts);
        }
        totals
    }

    /// The parse of one file, reused while its modification time and length are
    /// unchanged. A growing log is re-parsed rather than continued: the tail of
    /// a live rollout is a few megabytes, and a wrong continuation would be
    /// indistinguishable from a correct number.
    fn bucket(&mut self, kind: Kind, path: &Path, offsets: &mut Offsets) -> &Bucket {
        let metadata = path.metadata().ok();
        let modified = metadata.as_ref().and_then(|value| value.modified().ok());
        let length = metadata.as_ref().map(|value| value.len()).unwrap_or(0);
        let cached = self
            .files
            .get(path)
            .is_some_and(|cached| cached.length == length && cached.modified == modified);
        if !cached {
            let bucket = parse_file(kind, path, offsets);
            self.files.insert(
                path.to_owned(),
                CachedFile {
                    modified,
                    length,
                    bucket,
                    used: Instant::now(),
                },
            );
        }
        let cached = self.files.get_mut(path).expect("cached file");
        cached.used = Instant::now();
        &cached.bucket
    }

    fn prune(&mut self) {
        if self
            .last_prune
            .is_some_and(|at| at.elapsed() < Duration::from_secs(600))
        {
            return;
        }
        self.last_prune = Some(Instant::now());
        self.files
            .retain(|path, cached| path.is_file() && cached.used.elapsed() < CACHE_TTL);
        while self.files.len() > CACHE_FILES {
            let oldest = self
                .files
                .iter()
                .min_by_key(|(_, cached)| cached.used)
                .map(|(path, _)| path.clone());
            match oldest {
                Some(path) => {
                    self.files.remove(&path);
                }
                None => break,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Claude,
    Codex,
    Pi,
}

/// Records of one file, grouped by local day.
#[derive(Default)]
struct Bucket {
    days: BTreeMap<i32, (u64, TokenCounts)>,
}

impl Bucket {
    fn push(&mut self, at: i64, counts: TokenCounts, offsets: &mut Offsets) {
        let day = self.days.entry(offsets.day(at)).or_default();
        day.0 += 1;
        day.1.add(&counts);
    }

    /// Everything inside the inclusive day range, and how many records that was.
    fn between(&self, first_day: i32, last_day: i32) -> (u64, TokenCounts) {
        let mut records = 0;
        let mut counts = TokenCounts::default();
        for (day_records, day_counts) in self.days.range(first_day..=last_day).map(|(_, day)| day) {
            records += day_records;
            counts.add(day_counts);
        }
        (records, counts)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Totals {
    counts: TokenCounts,
    records: u64,
}

/// Local UTC offsets, resolved once per hour of record time. A per-record call
/// would be one system call per token event; a per-file call would misplace the
/// day of anything a DST change sits between.
#[derive(Default)]
struct Offsets {
    by_hour: HashMap<i64, i64>,
}

impl Offsets {
    fn millis(&mut self, at: i64) -> i64 {
        let hour = at.div_euclid(3_600_000);
        *self.by_hour.entry(hour).or_insert_with(|| {
            OffsetDateTime::from_unix_timestamp_nanos(at as i128 * 1_000_000)
                .ok()
                .and_then(|moment| UtcOffset::local_offset_at(moment).ok())
                .map(|offset| i64::from(offset.whole_seconds()) * 1_000)
                .unwrap_or(0)
        })
    }

    /// Days since the Unix epoch in local time, which is the bucket key every
    /// window boundary in this module can be compared against.
    fn day(&mut self, at: i64) -> i32 {
        let offset = self.millis(at);
        (at.saturating_add(offset).div_euclid(86_400_000)) as i32
    }
}

fn parse_file(kind: Kind, path: &Path, offsets: &mut Offsets) -> Bucket {
    match kind {
        Kind::Claude => claude_file(path, offsets),
        Kind::Codex => codex_file(path, offsets),
        Kind::Pi => pi_file(path, offsets),
    }
}

/// Claude Code: one `usage` block per assistant message in the transcript.
///
/// The same message is written two or three times (a streaming update and the
/// final record share an id), so the parse de-duplicates by message id before
/// anything is added up.
fn claude_file(path: &Path, offsets: &mut Offsets) -> Bucket {
    let mut bucket = Bucket::default();
    let mut seen = HashSet::new();
    for value in jsonl(path) {
        if value["type"] != "assistant" {
            continue;
        }
        let Some(at) = value["timestamp"].as_str().and_then(parse_rfc3339_millis) else {
            continue;
        };
        let Some(usage) = value["message"].get("usage") else {
            continue;
        };
        let id = value["message"]["id"]
            .as_str()
            .or_else(|| value["uuid"].as_str())
            .map(str::to_owned);
        if let Some(id) = id {
            if !seen.insert(id) {
                continue;
            }
        }
        let cached = u64_of(usage, "cache_read_input_tokens");
        bucket.push(
            at,
            TokenCounts {
                input: u64_of(usage, "input_tokens")
                    .saturating_add(u64_of(usage, "cache_creation_input_tokens"))
                    .saturating_add(cached),
                input_cached: cached,
                output: u64_of(usage, "output_tokens"),
                output_cached: 0,
                cost: None,
            },
            offsets,
        );
    }
    bucket
}

/// Codex: `token_count` events carry a cumulative usage for the thread, so the
/// table is built from the difference between consecutive events. Deltas that
/// fall before the window are consumed silently, which is what keeps a thread
/// opened last week from reporting its whole history as today's usage.
fn codex_file(path: &Path, offsets: &mut Offsets) -> Bucket {
    let mut bucket = Bucket::default();
    let mut previous: Option<TokenCounts> = None;
    for value in jsonl(path) {
        if value["type"] != "event_msg" || value["payload"]["type"] != "token_count" {
            continue;
        }
        let info = &value["payload"]["info"];
        let Some(usage) = info.get("total_token_usage") else {
            continue;
        };
        let current = TokenCounts {
            input: u64_of(usage, "input_tokens"),
            input_cached: u64_of(usage, "cached_input_tokens"),
            output: u64_of(usage, "output_tokens"),
            output_cached: 0,
            cost: None,
        };
        let delta = match previous {
            // A resumed thread can open its file with the thread's accumulated
            // total, so taking the first cumulative value as the first turn
            // would pour the whole history into that day. The per-turn counter
            // carries this turn alone, which is the most the file can own.
            None => {
                let last_turn = TokenCounts {
                    input: u64_of(&info["last_token_usage"], "input_tokens"),
                    input_cached: u64_of(&info["last_token_usage"], "cached_input_tokens"),
                    output: u64_of(&info["last_token_usage"], "output_tokens"),
                    output_cached: 0,
                    cost: None,
                };
                if last_turn.input == 0 && last_turn.output == 0 {
                    current
                } else {
                    TokenCounts {
                        input: current.input.min(last_turn.input),
                        input_cached: current.input_cached.min(last_turn.input_cached),
                        output: current.output.min(last_turn.output),
                        output_cached: 0,
                        cost: None,
                    }
                }
            }
            Some(previous) => TokenCounts {
                input: current.input.saturating_sub(previous.input),
                input_cached: current.input_cached.saturating_sub(previous.input_cached),
                output: current.output.saturating_sub(previous.output),
                output_cached: 0,
                cost: None,
            },
        };
        previous = Some(current);
        let Some(at) = value["timestamp"].as_str().and_then(parse_rfc3339_millis) else {
            continue;
        };
        bucket.push(at, delta, offsets);
    }
    bucket
}

/// Pi: one usage block per assistant message, with a millisecond timestamp, a
/// response id for de-duplication and the cost Pi itself computed.
fn pi_file(path: &Path, offsets: &mut Offsets) -> Bucket {
    let mut bucket = Bucket::default();
    let mut seen = HashSet::new();
    for value in jsonl(path) {
        if value["type"] != "message" {
            continue;
        }
        let message = &value["message"];
        if message["role"] != "assistant" {
            continue;
        }
        let Some(at) = message["timestamp"].as_i64() else {
            continue;
        };
        let Some(usage) = message.get("usage") else {
            continue;
        };
        if let Some(id) = message["responseId"].as_str() {
            if !seen.insert(id.to_owned()) {
                continue;
            }
        }
        let cached = u64_of(usage, "cacheRead");
        bucket.push(
            at,
            TokenCounts {
                input: u64_of(usage, "input")
                    .saturating_add(u64_of(usage, "cacheWrite"))
                    .saturating_add(cached),
                input_cached: cached,
                output: u64_of(usage, "output"),
                output_cached: 0,
                cost: usage["cost"]["total"].as_f64(),
            },
            offsets,
        );
    }
    bucket
}

/// OpenCode keeps every message in its database, one row per API response, with
/// per-response tokens and the real cost. That makes the period exact — and the
/// database index makes the whole table a single query, so no cache is needed.
fn opencode(database: &Path, start: i64, end: i64) -> Totals {
    let Ok(connection) = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Totals::default();
    };
    let _ = connection.busy_timeout(Duration::from_millis(250));
    let mut statement = match connection.prepare(OPENCODE_SQL) {
        Ok(statement) => statement,
        Err(_) => return Totals::default(),
    };
    let row = statement.query_row([start, end], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, f64>(5)?,
        ))
    });
    let Ok((records, input, cache_read, cache_write, output, cost)) = row else {
        return Totals::default();
    };
    let mut totals = Totals::default();
    if records > 0 {
        totals.records = records as u64;
        totals.counts = TokenCounts {
            input: (input.max(0) as u64)
                .saturating_add(cache_read.max(0) as u64)
                .saturating_add(cache_write.max(0) as u64),
            input_cached: cache_read.max(0) as u64,
            output: output.max(0) as u64,
            output_cached: 0,
            cost: Some(cost),
        };
    }
    totals
}

/// `tokens.input` is the uncached prompt, `cache.read`/`cache.write` are the
/// cached shares, and `cost` is what OpenCode charged for the response.
const OPENCODE_SQL: &str = r#"
SELECT
  count(*),
  coalesce(sum(json_extract(data, '$.tokens.input')), 0),
  coalesce(sum(json_extract(data, '$.tokens.cache.read')), 0),
  coalesce(sum(json_extract(data, '$.tokens.cache.write')), 0),
  coalesce(sum(json_extract(data, '$.tokens.output')), 0),
  coalesce(sum(json_extract(data, '$.cost')), 0)
FROM message
WHERE time_created >= ?1 AND time_created <= ?2
  AND json_extract(data, '$.role') = 'assistant'
"#;

/// DeepSeek Harness: the session projection keeps one cumulative counter per
/// conversation and no per-turn history, so a conversation counts in the window
/// where it was last active. That is the only attribution the source supports;
/// splitting a long conversation across days would require inventing numbers.
fn deepseek(root: &Path, start: i64, end: i64) -> Totals {
    let mut totals = Totals::default();
    let Ok(entries) = fs::read_dir(root) else {
        return totals;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(value) = read_json(&path) else {
            continue;
        };
        let record = &value["record"];
        let totals_row = &record["rows"]["tokenUsage"]["val"]["totals"];
        let Some(active) = record["rows"]["sessionListMetadata"]["val"]["lastPromptAt"]
            .as_i64()
            .or_else(|| record["identity"]["createdAt"].as_i64())
        else {
            continue;
        };
        if active < start || active > end {
            continue;
        }
        let cached = u64_of(totals_row, "cacheReadTokens");
        totals.records += 1;
        totals.counts.add(&TokenCounts {
            input: u64_of(totals_row, "uncachedInputTokens")
                .saturating_add(u64_of(totals_row, "cacheWriteTokens"))
                .saturating_add(cached),
            input_cached: cached,
            output: u64_of(totals_row, "outputTokens"),
            output_cached: 0,
            cost: None,
        });
    }
    totals
}

/// A background thread that keeps one snapshot per period for the menu.
///
/// The menu reads the snapshot; the thread recomputes it when the period
/// changes and every [`REFRESH_INTERVAL`] afterwards. Because the source files
/// are the only state, a restart costs one scan and loses nothing.
pub struct Worker {
    period: std::sync::mpsc::Sender<Period>,
    snapshot: Arc<Mutex<UsageSnapshot>>,
}

impl Worker {
    pub fn spawn(period: Period, wake: impl Fn() + Send + 'static) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let snapshot = Arc::new(Mutex::new(UsageSnapshot::empty(period)));
        let shared = Arc::clone(&snapshot);
        thread::spawn(move || {
            let mut scanner = Scanner::new();
            let mut period = period;
            loop {
                let next = match Roots::system() {
                    Some(roots) => scanner.scan(&roots, period, SystemTime::now()),
                    None => UsageSnapshot::empty(period),
                };
                if let Ok(mut slot) = shared.lock() {
                    *slot = next;
                }
                wake();
                match receiver.recv_timeout(REFRESH_INTERVAL) {
                    Ok(mut requested) => {
                        // The switch is a single click, but coalesce a burst so
                        // the last one wins and only one scan runs per change.
                        while let Ok(newer) = receiver.try_recv() {
                            requested = newer;
                        }
                        period = requested;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self {
            period: sender,
            snapshot,
        }
    }

    pub fn snapshot(&self) -> UsageSnapshot {
        self.snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_else(|error| error.into_inner().clone())
    }

    pub fn set_period(&self, period: Period) {
        let _ = self.period.send(period);
    }
}

fn epoch_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn u64_of(value: &serde_json::Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let file = fs::File::open(path).ok()?;
    serde_json::from_reader(BufReader::new(file)).ok()
}

fn jsonl(path: &Path) -> Vec<serde_json::Value> {
    let Ok(file) = fs::File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect()
}

/// Whether the file was written at or after `start`. A JSONL log is append-only,
/// so a file whose last write precedes the window cannot contain a record inside
/// it; skipping it is what keeps the day view free of a full history scan.
fn touched_since(path: &Path, start: i64) -> bool {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(|modified| epoch_millis(modified) >= start)
        .unwrap_or(false)
}

fn jsonl_files(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_jsonl(root, depth, &mut files);
    files
}

fn collect_jsonl(directory: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, depth - 1, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

/// Parse the RFC 3339 timestamps the Codex and Claude logs write
/// (`2026-08-17T14:20:51.810Z`). `time`'s parser needs two extra features for
/// this, so the fixed shape these logs use is decoded here instead.
fn parse_rfc3339_millis(value: &str) -> Option<i64> {
    if value.len() < 19 {
        return None;
    }
    let year: i32 = value.get(0..4)?.parse().ok()?;
    let month: u8 = value.get(5..7)?.parse().ok()?;
    let day: u8 = value.get(8..10)?.parse().ok()?;
    let hour: u8 = value.get(11..13)?.parse().ok()?;
    let minute: u8 = value.get(14..16)?.parse().ok()?;
    let second: u8 = value.get(17..19)?.parse().ok()?;
    let separators = [4, 7, 10, 13, 16].map(|index| value.as_bytes().get(index).copied());
    if separators[0] != Some(b'-')
        || separators[1] != Some(b'-')
        || !matches!(separators[2], Some(b'T' | b't' | b' '))
        || separators[3] != Some(b':')
        || separators[4] != Some(b':')
    {
        return None;
    }
    let rest = &value[19..];
    let (millis, rest) = match rest.strip_prefix('.') {
        Some(fraction) => {
            let digits = fraction
                .char_indices()
                .take_while(|(_, character)| character.is_ascii_digit())
                .last()
                .map(|(index, _)| index + 1)
                .unwrap_or(0);
            let millis = fraction_millis(&fraction[..digits])?;
            (millis, &fraction[digits..])
        }
        None => (0, rest),
    };
    let offset = match rest {
        "Z" | "z" | "" => 0,
        zone => {
            let sign = match zone.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hours: i64 = zone.get(1..3)?.parse().ok()?;
            let minutes: i64 = zone.get(4..6)?.parse().ok()?;
            sign * (hours * 3600 + minutes * 60)
        }
    };
    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    let seconds = date.with_time(time).assume_utc().unix_timestamp() - offset;
    Some(seconds * 1_000 + millis)
}

/// Milliseconds from a fractional-second digit run: `.8` is 800 ms, `.81` is
/// 810 ms, `.810` is 810 ms and anything past three digits is truncated.
fn fraction_millis(digits: &str) -> Option<i64> {
    if digits.is_empty() {
        return Some(0);
    }
    let mut millis = 0;
    for (index, character) in digits.chars().take(3).enumerate() {
        let digit = character.to_digit(10)? as i64;
        millis += digit * 10i64.pow(2 - index as u32);
    }
    Some(millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("asi-usage-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        let mut file = fs::File::create(path).expect("create");
        file.write_all(text.as_bytes()).expect("write");
    }

    fn utc(text: &str) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(parse_rfc3339_millis(text).unwrap() / 1_000).unwrap()
    }

    fn roots(dir: &Path) -> Roots {
        Roots {
            claude: dir.join("claude"),
            codex: dir.join("codex"),
            opencode: dir.join("opencode.db"),
            pi: dir.join("pi"),
            deepseek: dir.join("deepseek"),
        }
    }

    /// Every JSONL test scans one window that starts at the epoch, so the local
    /// day buckets are compared without depending on the machine's timezone.
    fn totals(kind: Kind, root: &Path, end: i64) -> Totals {
        let mut scanner = Scanner::new();
        let mut offsets = Offsets::default();
        let last_day = offsets.day(end);
        scanner.jsonl_totals(kind, root, 0, 0, last_day, &mut offsets)
    }

    #[test]
    fn day_window_starts_at_local_midnight() {
        let now = utc("2026-10-17T05:30:00Z");
        let offset = UtcOffset::from_hms(8, 0, 0).unwrap();
        let start = window_start_at(Period::Day, now, offset);
        assert_eq!(start, 1_792_166_400_000, "2026-10-17T00:00+08:00");
    }

    #[test]
    fn week_window_starts_on_monday() {
        // 2026-10-17 is a Saturday, so the week starts on Monday the 12th.
        let now = utc("2026-10-17T05:30:00Z");
        let offset = UtcOffset::from_hms(8, 0, 0).unwrap();
        let start = window_start_at(Period::Week, now, offset);
        assert_eq!(start, 1_791_734_400_000, "2026-10-12T00:00+08:00");
    }

    #[test]
    fn month_window_starts_on_the_first() {
        let now = utc("2026-10-17T05:30:00Z");
        let offset = UtcOffset::from_hms(8, 0, 0).unwrap();
        let start = window_start_at(Period::Month, now, offset);
        assert_eq!(start, 1_790_784_000_000, "2026-10-01T00:00+08:00");
    }

    #[test]
    fn a_local_evening_still_belongs_to_the_same_day() {
        // 23:30 local (+08:00) is 15:30 UTC, and its window began at local
        // midnight — the boundary must not be derived from the UTC date.
        let offset = UtcOffset::from_hms(8, 0, 0).unwrap();
        assert_eq!(
            window_start_at(Period::Day, utc("2026-10-17T15:30:00Z"), offset),
            window_start_at(Period::Day, utc("2026-10-17T00:00:01Z"), offset)
        );
    }

    #[test]
    fn periods_cycle_and_round_trip_through_the_config() {
        for period in Period::ALL {
            assert_eq!(Period::from_key(period.key()), period);
            assert!(!period.label().is_empty());
        }
        assert_eq!(Period::Day.next(), Period::Week);
        assert_eq!(Period::Week.next(), Period::Month);
        assert_eq!(Period::Month.next(), Period::Day);
        assert_eq!(Period::from_key("nonsense"), Period::Day);
    }

    #[test]
    fn parses_the_timestamps_the_logs_write() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_rfc3339_millis("2026-08-17T14:20:51.810Z"),
            Some(1_786_976_451_810)
        );
        // A fraction shorter than three digits is a decimal fraction, not
        // milliseconds: `.8` is 800 ms, not 8 ms.
        assert_eq!(
            parse_rfc3339_millis("2026-08-17T14:20:51.8Z"),
            Some(1_786_976_451_800)
        );
        // An explicit offset is honoured.
        assert_eq!(
            parse_rfc3339_millis("2026-08-17T22:20:51.810+08:00"),
            Some(1_786_976_451_810)
        );
        assert_eq!(parse_rfc3339_millis("not a timestamp"), None);
    }

    #[test]
    fn claude_counts_each_message_once() {
        let dir = temp_dir("claude");
        let message = |id: &str, input: u64, cached: u64, output: u64| {
            json!({
                "type": "assistant",
                "timestamp": "2026-10-17T09:00:00.000Z",
                "message": {
                    "id": id,
                    "usage": {
                        "input_tokens": input,
                        "cache_creation_input_tokens": 7,
                        "cache_read_input_tokens": cached,
                        "output_tokens": output
                    }
                }
            })
            .to_string()
        };
        write(
            &dir.join("claude/-Users-me-proj/session.jsonl"),
            &format!(
                "{}\n{}\n{}\n",
                message("m1", 100, 900, 10),
                message("m1", 100, 900, 10),
                message("m2", 5, 0, 3)
            ),
        );
        let totals = totals(
            Kind::Claude,
            &dir.join("claude"),
            parse_rfc3339_millis("2027-01-01T00:00:00Z").unwrap(),
        );
        assert_eq!(totals.records, 2, "the duplicated record is counted once");
        assert_eq!(totals.counts.input, 100 + 7 + 900 + 5 + 7);
        assert_eq!(totals.counts.input_cached, 900);
        assert_eq!(totals.counts.output, 13);
        assert_eq!(totals.counts.cost, None);
    }

    #[test]
    fn codex_differences_the_cumulative_counter() {
        let dir = temp_dir("codex");
        let event = |stamp: &str, input: u64, cached: u64, output: u64| {
            json!({
                "type": "event_msg",
                "timestamp": stamp,
                "payload": {
                    "type": "token_count",
                    "info": { "total_token_usage": {
                        "input_tokens": input,
                        "cached_input_tokens": cached,
                        "output_tokens": output,
                        "total_tokens": input + output
                    }}
                }
            })
            .to_string()
        };
        write(
            &dir.join("codex/2026/10/17/rollout-a.jsonl"),
            &format!(
                "{}\n{}\n{}\n{}\n",
                event("2026-10-16T10:00:00.000Z", 100, 50, 10),
                event("2026-10-16T11:00:00.000Z", 200, 100, 20),
                event("2026-10-17T10:00:00.000Z", 700, 400, 80),
                event("2026-10-17T11:00:00.000Z", 1000, 600, 120),
            ),
        );
        let root = dir.join("codex");
        let end = parse_rfc3339_millis("2027-01-01T00:00:00Z").unwrap();
        let all = totals(Kind::Codex, &root, end);
        assert_eq!(all.records, 4);
        assert_eq!(
            all.counts.input, 1000,
            "cumulative deltas add up to the last"
        );
        assert_eq!(all.counts.output, 120);

        // From the 17th on, only the last two turns count: the earlier deltas
        // stay in their own day bucket instead of inflating today.
        let mut scanner = Scanner::new();
        let mut offsets = Offsets::default();
        let start = parse_rfc3339_millis("2026-10-17T00:00:00Z").unwrap();
        let start_day = offsets.day(start);
        let last_day = offsets.day(end);
        let window =
            scanner.jsonl_totals(Kind::Codex, &root, start, start_day, last_day, &mut offsets);
        assert_eq!(window.records, 2);
        assert_eq!(window.counts.input, 800, "700 + 300 in-window input tokens");
        assert_eq!(window.counts.input_cached, 500);
        assert_eq!(window.counts.output, 100);
    }

    #[test]
    fn a_rollout_that_opens_with_a_thread_total_only_counts_its_own_turn() {
        // The shape a resumed thread can write: the first event already carries
        // the history, while `last_token_usage` describes the turn itself.
        let dir = temp_dir("codex-resume");
        let event = |input: u64, last_input: u64, output: u64, last_output: u64| {
            json!({
                "type": "event_msg",
                "timestamp": "2026-10-17T10:00:00.000Z",
                "payload": {
                    "type": "token_count",
                    "info": {
                        "total_token_usage": { "input_tokens": input, "output_tokens": output },
                        "last_token_usage": { "input_tokens": last_input, "output_tokens": last_output }
                    }
                }
            })
            .to_string()
        };
        write(
            &dir.join("codex/rollout-b.jsonl"),
            &format!(
                "{}\n{}\n",
                event(5_000_000, 1_200, 40_000, 30),
                event(5_003_000, 3_000, 40_100, 100),
            ),
        );
        let totals = totals(
            Kind::Codex,
            &dir.join("codex"),
            parse_rfc3339_millis("2027-01-01T00:00:00Z").unwrap(),
        );
        assert_eq!(totals.records, 2);
        assert_eq!(
            totals.counts.input, 4_200,
            "1_200 for the first turn + the 3_000 its successor added"
        );
        assert_eq!(totals.counts.output, 130);
    }

    #[test]
    fn pi_sums_messages_and_reads_the_cost() {
        let dir = temp_dir("pi");
        let at = 1_791_000_000_000i64;
        let message = |id: &str, input: u64, cached: u64, output: u64, cost: f64| {
            json!({
                "type": "message",
                "message": {
                    "role": "assistant",
                    "responseId": id,
                    "timestamp": at,
                    "usage": {
                        "input": input,
                        "cacheRead": cached,
                        "cacheWrite": 1,
                        "output": output,
                        "cost": { "total": cost }
                    }
                }
            })
            .to_string()
        };
        write(
            &dir.join("pi/-Users-me-proj/session.jsonl"),
            &format!(
                "{}\n{}\n",
                message("r1", 10, 90, 5, 0.25),
                message("r1", 10, 90, 5, 0.25),
            ),
        );
        let totals = totals(Kind::Pi, &dir.join("pi"), at + 1_000);
        assert_eq!(totals.records, 1, "one response, reported once");
        assert_eq!(totals.counts.input, 10 + 90 + 1);
        assert_eq!(totals.counts.input_cached, 90);
        assert_eq!(totals.counts.output, 5);
        assert_eq!(totals.counts.cost, Some(0.25));
    }

    #[test]
    fn a_file_is_reused_until_it_changes() {
        let dir = temp_dir("cache");
        let path = dir.join("cache/-proj/session.jsonl");
        write(
            &path,
            &format!(
                "{}\n",
                json!({
                    "type": "message",
                    "message": {
                        "role": "assistant",
                        "responseId": "r1",
                        "timestamp": 1_791_000_000_000i64,
                        "usage": { "input": 5, "output": 1, "cost": { "total": 0.1 } }
                    }
                })
            ),
        );
        let mut scanner = Scanner::new();
        let mut offsets = Offsets::default();
        assert_eq!(scanner.files.len(), 0);
        let first = scanner
            .bucket(Kind::Pi, &path, &mut offsets)
            .between(0, i32::MAX)
            .1;
        assert_eq!(scanner.files.len(), 1);
        let second = scanner
            .bucket(Kind::Pi, &path, &mut offsets)
            .between(0, i32::MAX)
            .1;
        assert_eq!(first, second);

        // Appending a record invalidates the cached parse.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append");
        writeln!(
            file,
            "{}",
            json!({
                "type": "message",
                "message": {
                    "role": "assistant",
                    "responseId": "r2",
                    "timestamp": 1_791_000_100_000i64,
                    "usage": { "input": 7, "output": 2, "cost": { "total": 0.2 } }
                }
            })
        )
        .expect("write");
        drop(file);
        let third = scanner
            .bucket(Kind::Pi, &path, &mut offsets)
            .between(0, i32::MAX)
            .1;
        assert_eq!(third.input, 12);
        assert_eq!(third.output, 3);
    }

    #[test]
    fn opencode_runs_one_query_over_its_messages() {
        let dir = temp_dir("opencode");
        let database = dir.join("opencode.db");
        let connection = Connection::open(&database).expect("database");
        connection
            .execute_batch(
                "CREATE TABLE message (id text PRIMARY KEY, time_created integer NOT NULL, data text NOT NULL);",
            )
            .expect("schema");
        let insert = |id: &str, at: i64, data: serde_json::Value| {
            connection
                .execute(
                    "INSERT INTO message (id, time_created, data) VALUES (?1, ?2, ?3)",
                    rusqlite::params![id, at, data.to_string()],
                )
                .expect("insert");
        };
        let at = 1_791_000_000_000i64;
        let assistant = |input: u64, read: u64, write: u64, output: u64, cost: f64| {
            json!({
                "role": "assistant",
                "tokens": {
                    "input": input,
                    "output": output,
                    "cache": { "read": read, "write": write }
                },
                "cost": cost
            })
        };
        insert("a1", at, assistant(10, 100, 1, 5, 0.5));
        insert("a2", at - 86_400_000 * 2, assistant(1_000, 0, 0, 50, 9.0));
        insert("u1", at, json!({ "role": "user" }));
        let totals = opencode(&database, at - 1_000, at + 1_000);
        assert_eq!(totals.records, 1);
        assert_eq!(totals.counts.input, 111);
        assert_eq!(totals.counts.input_cached, 100);
        assert_eq!(totals.counts.output, 5);
        assert_eq!(totals.counts.cost, Some(0.5));
    }

    #[test]
    fn deepseek_reads_the_projection_totals_of_active_sessions() {
        let dir = temp_dir("deepseek");
        let at = 1_791_000_000_000i64;
        let session = |last_prompt: Option<i64>, input: u64, read: u64, output: u64| {
            json!({
                "version": 7,
                "record": {
                    "identity": { "createdAt": at - 5_000 },
                    "rows": {
                        "tokenUsage": { "val": { "totals": {
                            "uncachedInputTokens": input,
                            "cacheReadTokens": read,
                            "cacheWriteTokens": 2,
                            "outputTokens": output
                        }}},
                        "sessionListMetadata": { "val": { "lastPromptAt": last_prompt } }
                    }
                }
            })
            .to_string()
        };
        write(
            &dir.join("deepseek/session-a.json"),
            &session(Some(at), 500, 1_000, 60),
        );
        write(
            &dir.join("deepseek/session-b.json"),
            &session(Some(at - 86_400_000), 7, 7, 7),
        );
        write(
            &dir.join("deepseek/session-c.json"),
            &json!({ "version": 99, "record": {} }).to_string(),
        );
        let totals = deepseek(&dir.join("deepseek"), at - 1_000, at + 1_000);
        assert_eq!(totals.records, 1, "only the session active in the window");
        assert_eq!(totals.counts.input, 500 + 1_000 + 2);
        assert_eq!(totals.counts.input_cached, 1_000);
        assert_eq!(totals.counts.output, 60);
    }

    #[test]
    fn a_snapshot_sums_only_the_agents_that_reported() {
        let dir = temp_dir("snapshot");
        let roots = roots(&dir);
        let snapshot = Scanner::new().scan(&roots, Period::Day, SystemTime::now());
        assert!(!snapshot.has_data, "an empty tree reports no data");
        assert!(snapshot.total.cost.is_none());
        assert_eq!(snapshot.agents.len(), RESOURCE_AGENTS.len());
        assert!(snapshot.agents.iter().all(|agent| agent.counts.is_none()));
        for (agent, expected) in snapshot.agents.iter().zip(RESOURCE_AGENTS) {
            assert_eq!(agent.kind, expected);
        }
    }

    #[test]
    fn files_older_than_the_window_are_skipped() {
        let dir = temp_dir("stale");
        let path = dir.join("codex/2026/10/17/rollout-old.jsonl");
        write(&path, "{\"type\":\"event_msg\"}\n");
        // The window starts in the future: nothing on disk can be inside it.
        assert!(!touched_since(&path, i64::MAX / 2));
        assert!(touched_since(&path, 0));
    }
}
