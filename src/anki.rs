use std::env;
use std::error::Error as StdError;
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process;
use std::result::Result as StdResult;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{FixedOffset, Local, TimeZone};
use rusqlite::ffi::ErrorCode;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::lock;

pub type Error = Box<dyn StdError>;
pub type Result<T> = StdResult<T, Error>;

const SECONDS_PER_DAY: i64 = 86_400;
const DEFAULT_ROLLOVER_HOUR: i64 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Data {
    pub reviews: i64,
    pub learning: i64,
    pub new: i64,
    pub remaining: i64,
    pub completed: i64,
    pub total: i64,
    pub at: i64,
}

#[derive(Clone, Copy)]
struct Query {
    include_new: bool,
    now: i64,
    current_west: i64,
}

/// The parts of `col` and `config` the counts depend on. Kept with the card
/// index so an unchanged collection is not read twice.
#[derive(Clone, Eq, PartialEq)]
struct CollectionConfig {
    crt: i64,
    rollover: i64,
    creation_offset: Option<String>,
    scheduler_one: bool,
}

impl CollectionConfig {
    fn read(conn: &Connection) -> Result<Self> {
        let crt =
            conn.query_row("SELECT crt FROM col", [], |row| row.get(0))?;
        let scheduler_one = config(conn, "schedVer")?.as_deref() == Some("1");
        let rollover = rollover_hour(conn)?;
        let creation_offset = if scheduler_one {
            None
        } else {
            config(conn, "creationOffset")?
        };
        Ok(Self {
            crt,
            rollover,
            creation_offset,
            scheduler_one,
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct CollectionIdentity {
    database: FileStamp,
    wal: Option<FileStamp>,
}

fn file_stamp(path: &Path) -> io::Result<Option<FileStamp>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(FileStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn collection_identity(path: &Path) -> io::Result<CollectionIdentity> {
    let Some(database) = file_stamp(path)? else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "collection not found",
        ));
    };
    Ok(CollectionIdentity {
        database,
        wal: file_stamp(&sidecar(path, "-wal"))?,
    })
}

/// Due times of every card, sorted so a count is a binary search.
#[derive(Default)]
struct CardIndex {
    reviews: Vec<i64>,
    day_learning: Vec<i64>,
    intraday_learning: Vec<i64>,
    new: i64,
}

impl CardIndex {
    fn from_conn(conn: &Connection) -> Result<Self> {
        let mut index = Self::default();
        let mut stmt =
            conn.prepare("SELECT queue, due FROM cards WHERE queue >= 0")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (queue, due) = row?;
            index.add(queue, due);
        }
        index.reviews.sort_unstable();
        index.day_learning.sort_unstable();
        index.intraday_learning.sort_unstable();
        Ok(index)
    }

    fn add(&mut self, queue: i64, due: i64) {
        match queue {
            0 => self.new = self.new.saturating_add(1),
            1 | 4 => self.intraday_learning.push(due),
            2 => self.reviews.push(due),
            3 => self.day_learning.push(due),
            _ => {}
        }
    }

    fn counts(&self, days: i64, now: i64) -> (i64, i64, i64, i64) {
        (
            count_at(&self.reviews, days),
            count_at(&self.day_learning, days),
            count_at(&self.intraday_learning, now),
            self.new,
        )
    }

    fn data(&self, query: Query, counts: (i64, i64)) -> Data {
        let (days, completed) = counts;
        let (reviews, day_learning, intraday_learning, new_cards) =
            self.counts(days, query.now);
        let new = if query.include_new { new_cards } else { 0 };
        let learning = intraday_learning.saturating_add(day_learning);
        let remaining = reviews.saturating_add(learning).saturating_add(new);
        Data {
            reviews,
            learning,
            new,
            remaining,
            completed,
            total: remaining.saturating_add(completed),
            at: query.now,
        }
    }
}

fn count_at(due: &[i64], limit: i64) -> i64 {
    i64::try_from(due.partition_point(|value| *value <= limit))
        .unwrap_or(i64::MAX)
}

struct SampleData {
    config: CollectionConfig,
    cards: CardIndex,
    completed_day: i64,
    completed: i64,
}

impl SampleData {
    fn from_conn(conn: &Connection, query: Query) -> Result<(Self, Data)> {
        let config = CollectionConfig::read(conn)?;
        let (days, next_day) =
            timing_from(&config, query.now, query.current_west)?;
        let cards = CardIndex::from_conn(conn)?;
        let completed = completed_count(conn, next_day)?;
        let data = cards.data(query, (days, completed));
        Ok((
            Self {
                config,
                cards,
                completed_day: next_day,
                completed,
            },
            data,
        ))
    }

    fn sample(&self, query: Query) -> Result<Option<Data>> {
        let (days, next_day) =
            timing_from(&self.config, query.now, query.current_west)?;
        if next_day != self.completed_day {
            return Ok(None);
        }
        Ok(Some(self.cards.data(query, (days, self.completed))))
    }
}

struct Cache {
    path: PathBuf,
    identity: CollectionIdentity,
    data: SampleData,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

pub fn collection_path(raw: &str, plugin_dir: &Path) -> Result<PathBuf> {
    let path = expand_path(raw, plugin_dir);
    if !path.as_os_str().is_empty() {
        return Ok(if path.is_dir() {
            path.join("collection.anki2")
        } else {
            path
        });
    }

    let base = profiles_dir()?;
    let mut found = Vec::new();
    if let Ok(entries) = fs::read_dir(&base) {
        found.extend(
            entries
                .flatten()
                .map(|entry| entry.path().join("collection.anki2"))
                .filter(|candidate| candidate.is_file()),
        );
    }
    found.sort();
    match found.len() {
        0 => Err("no Anki collection found".into()),
        1 => Ok(found.remove(0)),
        _ => Err("multiple Anki profiles; set collection_path".into()),
    }
}

fn profiles_dir() -> Result<PathBuf> {
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".local/share/Anki2"))
}

pub fn expand_path(raw: &str, plugin_dir: &Path) -> PathBuf {
    let path = raw.trim();
    if path.is_empty() {
        return PathBuf::new();
    }
    if let Some(expanded) = home_relative(path) {
        return expanded;
    }
    let value = Path::new(path);
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        plugin_dir.join(value)
    }
}

/// Expands `~` and `~/...`. Returns `None` for every other path.
fn home_relative(path: &str) -> Option<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME")?);
    if path == "~" {
        return Some(home);
    }
    path.strip_prefix("~/").map(|rest| home.join(rest))
}

pub fn sample(path: &Path, include_new: bool, now: i64) -> Result<Data> {
    let query = Query {
        include_new,
        now,
        current_west: current_west(now).ok_or("invalid current time")?,
    };
    match sample_with_cache(path, query) {
        Ok(Some(data)) => Ok(data),
        Ok(None) => sample_without_cache(path, query),
        Err(error) => Err(error),
    }
}

fn sample_with_cache(path: &Path, query: Query) -> Result<Option<Data>> {
    let Ok(identity) = collection_identity(path) else {
        return Ok(None);
    };
    let guard = lock(&CACHE);
    let Some(cache) = guard.as_ref() else {
        return Ok(None);
    };
    if cache.path != path || cache.identity != identity {
        return Ok(None);
    }
    cache.data.sample(query)
}

fn sample_without_cache(path: &Path, query: Query) -> Result<Data> {
    let identity = collection_identity(path)?;
    let mut guard = lock(&CACHE);
    let _ = guard.take();
    let conn = match open_readonly(path) {
        Ok(conn) => conn,
        Err(error) if is_locked(&error) => return sample_copy(path, query),
        Err(error) => return Err(error.into()),
    };
    let (sample, data) = match SampleData::from_conn(&conn, query) {
        Ok(result) => result,
        Err(error) if is_sample_locked(&error) => {
            drop(conn);
            return sample_copy(path, query);
        }
        Err(error) => return Err(error),
    };
    drop(conn);
    *guard = Some(Cache {
        path: path.to_path_buf(),
        identity,
        data: sample,
    });
    Ok(data)
}

fn is_sample_locked(error: &Error) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .is_some_and(is_locked)
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let flags =
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(1))?;
    // Reading the row also rejects a file that is not an Anki collection.
    conn.query_row("select 1 from col limit 1", [], |_| Ok(()))?;
    Ok(conn)
}

fn is_locked(error: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(inner, message) = error {
        matches!(
            inner.code,
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
        ) || message
            .as_deref()
            .is_some_and(|text| text.contains("locked"))
    } else {
        false
    }
}

/// Counts a copy of the collection, for when Anki holds the write lock.
fn sample_copy(path: &Path, query: Query) -> Result<Data> {
    let directory = TempDir::new()?;
    let name = path.file_name().ok_or("collection path has no file name")?;
    let target = directory.0.join(name);
    copy_writable(path, &target)?;
    let wal = sidecar(path, "-wal");
    if wal.exists() {
        copy_writable(&wal, &sidecar(&target, "-wal"))?;
    }

    let conn = Connection::open(&target)?;
    conn.busy_timeout(Duration::from_secs(1))?;
    sample_connection(&conn, query)
}

fn copy_writable(from: &Path, to: &Path) -> io::Result<()> {
    fs::copy(from, to)?;
    make_writable(to)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            env::temp_dir().join(format!("banki-{}-{nanos}", process::id()));
        create_private_directory(&path)?;
        Ok(Self(path))
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sample_connection(conn: &Connection, query: Query) -> Result<Data> {
    let (_, data) = SampleData::from_conn(conn, query)?;
    Ok(data)
}

fn completed_count(conn: &Connection, next_day: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT count(*) FROM revlog WHERE id >= ? AND type BETWEEN 0 AND 3",
        (completed_threshold(next_day),),
        |row| row.get::<_, i64>(0),
    )?)
}

#[cfg(unix)]
fn make_writable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn make_writable(path: &Path) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)
}

const fn completed_threshold(next_day: i64) -> i64 {
    next_day
        .saturating_sub(SECONDS_PER_DAY)
        .saturating_mul(1000)
}

/// Minutes west of UTC, the unit Anki stores time zones in.
fn current_west(now: i64) -> Option<i64> {
    Local.timestamp_opt(now, 0).earliest().map(|local| {
        i64::from(local.offset().local_minus_utc())
            .checked_div(-60)
            .unwrap_or(0)
    })
}

#[cfg(test)]
fn timing(
    conn: &Connection,
    now: i64,
    current_west: i64,
) -> Result<(i64, i64)> {
    let config = CollectionConfig::read(conn)?;
    timing_from(&config, now, current_west)
}

/// Days since the collection was created and the start of the next Anki day.
fn timing_from(
    config: &CollectionConfig,
    now: i64,
    current_west: i64,
) -> Result<(i64, i64)> {
    if config.scheduler_one {
        let days = now.saturating_sub(config.crt).div_euclid(SECONDS_PER_DAY);
        let next_day = days
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_DAY)
            .saturating_add(config.crt);
        return Ok((days, next_day));
    }

    let Some(raw_offset) = config.creation_offset.as_deref() else {
        let start = rollover_stamp(config.crt, current_west, config.rollover)?;
        let today = rollover_stamp(now, current_west, config.rollover)?;
        let next_day = if today < now {
            today.saturating_add(SECONDS_PER_DAY)
        } else {
            today
        };
        let days = now.saturating_sub(start).div_euclid(SECONDS_PER_DAY);
        return Ok((days, next_day));
    };

    let created_west = raw_offset.parse::<i64>()?;
    let created = fixed_offset(created_west)?
        .timestamp_opt(config.crt, 0)
        .single()
        .ok_or("invalid collection creation time")?;
    let local = fixed_offset(current_west)?
        .timestamp_opt(now, 0)
        .single()
        .ok_or("invalid current time")?;
    let today = rollover_stamp(now, current_west, config.rollover)?;
    let elapsed = local
        .date_naive()
        .signed_duration_since(created.date_naive())
        .num_days();
    let (days, next_day) = if today <= now {
        (elapsed, today.saturating_add(SECONDS_PER_DAY))
    } else {
        (elapsed.saturating_sub(1), today)
    };
    Ok((days.max(0), next_day))
}

fn rollover_hour(conn: &Connection) -> Result<i64> {
    match config(conn, "rollover")? {
        Some(value) if !value.is_empty() => Ok(value.parse()?),
        _ => Ok(DEFAULT_ROLLOVER_HOUR),
    }
}

fn fixed_offset(minutes_west: i64) -> Result<FixedOffset> {
    let offset_seconds = minutes_west
        .checked_mul(-60)
        .ok_or("invalid timezone offset")?;
    let offset =
        i32::try_from(offset_seconds).map_err(|_| "invalid timezone offset")?;
    FixedOffset::east_opt(offset)
        .ok_or_else(|| "invalid timezone offset".into())
}

fn rollover_stamp(timestamp: i64, minutes_west: i64, hour: i64) -> Result<i64> {
    let zone = fixed_offset(minutes_west)?;
    let local = zone
        .timestamp_opt(timestamp, 0)
        .single()
        .ok_or("invalid timestamp")?;
    let hour_of_day = u32::try_from(hour.rem_euclid(24)).unwrap_or(0);
    let naive = local
        .date_naive()
        .and_hms_opt(hour_of_day, 0, 0)
        .ok_or("invalid rollover hour")?;
    zone.from_local_datetime(&naive)
        .single()
        .map(|value| value.timestamp())
        .ok_or_else(|| "invalid rollover time".into())
}

fn config(conn: &Connection, key: &str) -> Result<Option<String>> {
    let value: Option<Value> = conn
        .query_row("SELECT val FROM config WHERE key = ?", (key,), |row| {
            row.get(0)
        })
        .optional()?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(Value::Text(text)) => Some(text),
        Some(Value::Integer(number)) => Some(number.to_string()),
        Some(Value::Real(number)) => Some(number.to_string()),
        Some(Value::Blob(bytes)) => {
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
    })
}

#[cfg(test)]
mod tests {
    use noctalia_rs::unix_seconds;

    use super::*;

    fn database() -> Connection {
        let conn = Connection::open_in_memory().expect("memory database");
        conn.execute_batch(
            "CREATE TABLE col (crt INTEGER);
             CREATE TABLE config (key TEXT PRIMARY KEY, val);
             CREATE TABLE cards (queue INTEGER, due INTEGER);
             CREATE TABLE revlog (id INTEGER, type INTEGER);",
        )
        .expect("schema");
        conn
    }

    fn set(conn: &Connection, key: &str, value: &str) {
        conn.execute(
            "INSERT INTO config (key, val) VALUES (?, ?)",
            (key, value),
        )
        .expect("config row");
    }

    fn query(include_new: bool, now: i64) -> Query {
        Query {
            include_new,
            now,
            current_west: 0,
        }
    }

    #[test]
    fn test_timing_scheduler_one() {
        let conn = database();
        conn.execute("INSERT INTO col (crt) VALUES (1000000)", [])
            .expect("col row");
        set(&conn, "schedVer", "1");

        let (days, next) =
            timing(&conn, 1_000_000 + 2 * 86_400 + 5, 0).expect("timing");

        assert_eq!(days, 2);
        assert_eq!(next, 1_000_000 + 3 * 86_400);
    }

    #[test]
    fn test_timing_creation_offset_respects_rollover() {
        let conn = database();
        let crt = 1_704_067_200 + 5 * 3600;
        conn.execute("INSERT INTO col (crt) VALUES (?)", (crt,))
            .expect("col row");
        set(&conn, "creationOffset", "0");
        set(&conn, "rollover", "4");

        let before = 1_704_067_200 + 86_400 + 3 * 3600;
        let (days, next) = timing(&conn, before, 0).expect("before rollover");
        assert_eq!(days, 0);
        assert_eq!(next, 1_704_168_000);

        let after = 1_704_168_000 + 10;
        let (days, next) = timing(&conn, after, 0).expect("after rollover");
        assert_eq!(days, 1);
        assert_eq!(next, 1_704_254_400);
    }

    #[test]
    fn test_timing_without_creation_offset() {
        let conn = database();
        let crt = 1_704_067_200 + 5 * 3600;
        conn.execute("INSERT INTO col (crt) VALUES (?)", (crt,))
            .expect("col row");
        set(&conn, "rollover", "4");

        let now = 1_704_168_000 + 10;
        let (days, next) = timing(&conn, now, 0).expect("timing");

        assert_eq!(days, (now - 1_704_081_600).div_euclid(86_400));
        assert_eq!(next, 1_704_254_400);
    }

    #[test]
    fn test_sample_counts_and_completion() {
        let conn = database();
        let now = 1_000_000;
        let crt = now - 3600;
        conn.execute("INSERT INTO col (crt) VALUES (?)", (crt,))
            .expect("col row");
        set(&conn, "schedVer", "1");
        conn.execute_batch(
            "INSERT INTO cards (queue, due) VALUES
               (2, 0), (2, 1), (3, 0), (1, 999999), (1, 1000001),
               (0, 0), (0, 0), (-1, 0);",
        )
        .expect("cards");
        conn.execute_batch(
            "INSERT INTO revlog (id, type) VALUES
               (1000099000, 0), (1000099001, 3), (1000099002, 4), (1, 0);",
        )
        .expect("revlog");

        let data = sample_connection(&conn, query(true, now)).expect("sample");

        assert_eq!(data.reviews, 1);
        assert_eq!(data.learning, 2);
        assert_eq!(data.new, 2);
        assert_eq!(data.remaining, 5);
        assert_eq!(data.completed, 2);
        assert_eq!(data.total, 7);
        assert_eq!(data.at, now);
    }

    #[test]
    fn test_include_new_only_changes_new_count() {
        let conn = database();
        let now = 1_000_000;
        conn.execute("INSERT INTO col (crt) VALUES (?)", (now - 3600,))
            .expect("col row");
        set(&conn, "schedVer", "1");
        conn.execute("INSERT INTO cards (queue, due) VALUES (0, 0)", [])
            .expect("card");

        let data = sample_connection(&conn, query(false, now)).expect("sample");

        assert_eq!(data.new, 0);
        assert_eq!(data.remaining, 0);
    }

    #[test]
    fn test_preview_repeat_counts_as_learning() {
        let conn = database();
        let now = 1_000_000;
        conn.execute("INSERT INTO col (crt) VALUES (?)", (now - 3600,))
            .expect("col row");
        set(&conn, "schedVer", "1");
        conn.execute_batch(
            "INSERT INTO cards (queue, due) VALUES
               (4, 999999), (4, 1000001), (-1, 0);",
        )
        .expect("cards");

        let data = sample_connection(&conn, query(true, now)).expect("sample");

        assert_eq!(data.learning, 1);
        assert_eq!(data.remaining, 1);
    }

    #[test]
    fn test_make_writable_clears_read_only() {
        let dir = env::temp_dir().join(format!("banki-perm-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("collection.anki2");
        fs::write(&path, b"x").expect("write");
        let mut permissions = fs::metadata(&path).expect("meta").permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&path, permissions).expect("readonly");

        make_writable(&path).expect("writable");

        assert!(!fs::metadata(&path).expect("meta").permissions().readonly());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_expand_path_home() {
        let Some(home) = env::var_os("HOME") else {
            return;
        };
        let home_path = PathBuf::from(home);
        assert_eq!(expand_path("~", Path::new("/plugin")), home_path);
        assert_eq!(
            expand_path("~/collection.anki2", Path::new("/plugin")),
            home_path.join("collection.anki2")
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_temporary_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let mode =
            fs::metadata(&dir.0).expect("meta").permissions().mode() & 0o777;

        assert_eq!(mode, 0o700);
    }

    #[test]
    fn test_expand_path_leaves_absolute_paths_alone() {
        assert_eq!(
            expand_path("/tmp/collection.anki2", Path::new("/plugin")),
            PathBuf::from("/tmp/collection.anki2")
        );
    }

    #[test]
    fn test_card_index_counts_due_cards() {
        let conn = database();
        conn.execute_batch(
            "INSERT INTO cards (queue, due) VALUES
               (2, 0), (2, 2), (3, 1), (3, 3), (1, 999), (1, 1001),
               (0, 0), (-1, 0);",
        )
        .expect("cards");

        let index = CardIndex::from_conn(&conn).expect("index");

        assert_eq!(index.counts(1, 1000), (1, 1, 1, 1));
        assert_eq!(index.counts(4, 2000), (2, 2, 2, 1));
    }

    #[test]
    fn test_sample_cache_tracks_collection_changes() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = env::temp_dir()
            .join(format!("banki-cache-{}-{nanos}", process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("collection.anki2");
        let now = unix_seconds();
        let writer = Connection::open(&path).expect("collection");
        writer
            .execute_batch(
                "CREATE TABLE col (crt INTEGER);
                 CREATE TABLE config (key TEXT PRIMARY KEY, val);
                 CREATE TABLE cards (queue INTEGER, due INTEGER);
                 CREATE TABLE revlog (id INTEGER, type INTEGER);",
            )
            .expect("schema");
        writer
            .execute_batch("PRAGMA journal_mode=WAL;")
            .expect("wal");
        writer
            .execute("INSERT INTO col (crt) VALUES (?)", (now - 3600,))
            .expect("col row");
        set(&writer, "schedVer", "1");
        writer
            .execute("INSERT INTO cards (queue, due) VALUES (0, 0)", [])
            .expect("card");
        drop(writer);

        let first = sample(&path, true, now).expect("first sample");
        assert_eq!(first.new, 1);
        let cached = sample(&path, true, now).expect("cached sample");
        assert_eq!(cached.new, 1);

        let writer = Connection::open(&path).expect("collection");
        writer
            .execute("INSERT INTO cards (queue, due) VALUES (0, 0)", [])
            .expect("new card");
        drop(writer);

        let updated = sample(&path, true, now).expect("updated sample");
        assert_eq!(updated.new, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_scheduler_one_ignores_malformed_creation_offset() {
        let conn = database();
        conn.execute("INSERT INTO col (crt) VALUES (1000000)", [])
            .expect("col row");
        set(&conn, "schedVer", "1");
        set(&conn, "creationOffset", "not a number");

        let (days, next) =
            timing(&conn, 1_000_000 + 2 * 86_400 + 5, 0).expect("timing");

        assert_eq!(days, 2);
        assert_eq!(next, 1_000_000 + 3 * 86_400);
    }

    #[test]
    fn test_scheduler_one_skips_invalid_creation_offset_text() {
        let conn = database();
        let now = 1_000_000;
        conn.execute("INSERT INTO col (crt) VALUES (?)", (now - 3600,))
            .expect("col row");
        set(&conn, "schedVer", "1");
        conn.execute(
            "INSERT INTO config (key, val) VALUES ('creationOffset', CAST(x'FF' AS TEXT))",
            [],
        )
        .expect("invalid config text");

        let data = sample_connection(&conn, query(false, now)).expect("sample");

        assert_eq!(data.at, now);
    }
}
