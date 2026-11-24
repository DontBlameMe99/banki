use std::env;
use std::error::Error as StdError;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::result::Result as StdResult;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{FixedOffset, Local, TimeZone};
use rusqlite::ffi::ErrorCode;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

pub type Error = Box<dyn StdError>;
pub type Result<T> = StdResult<T, Error>;

const SECONDS_PER_DAY: i64 = 86_400;

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

pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

pub fn collection_path(raw: &str, plugin_dir: &Path) -> Result<PathBuf> {
    let path = expand_path(raw, plugin_dir);
    if !path.as_os_str().is_empty() {
        if path.is_dir() {
            return Ok(path.join("collection.anki2"));
        }
        return Ok(path);
    }

    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let base = PathBuf::from(home).join(".local/share/Anki2");
    let mut matches = Vec::new();
    if let Ok(entries) = fs::read_dir(&base) {
        matches.extend(
            entries
                .flatten()
                .map(|entry| entry.path().join("collection.anki2"))
                .filter(|candidate| candidate.is_file()),
        );
    }
    matches.sort();
    match matches.len() {
        0 => Err("no Anki collection found".into()),
        1 => Ok(matches.remove(0)),
        _ => Err("multiple Anki profiles; set collection_path".into()),
    }
}

pub fn expand_path(raw: &str, plugin_dir: &Path) -> PathBuf {
    let path = raw.trim();
    if path.is_empty() {
        return PathBuf::new();
    }
    if (path == "~" || path.starts_with("~/"))
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(path.trim_start_matches("~/"));
    }
    let value = Path::new(path);
    if value.is_absolute() {
        value.to_path_buf()
    } else {
        plugin_dir.join(value)
    }
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let flags =
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(Duration::from_secs(1))?;
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

pub fn sample(path: &Path, include_new: bool, now: i64) -> Result<Data> {
    let query = Query {
        include_new,
        now,
        current_west: current_west(now),
    };
    match open_readonly(path) {
        Ok(conn) => sample_connection(&conn, query),
        Err(error) if is_locked(&error) => sample_copy(path, query),
        Err(error) => Err(error.into()),
    }
}

fn sample_copy(path: &Path, query: Query) -> Result<Data> {
    let directory = TemporaryDirectory::new()?;
    let name = path.file_name().ok_or("collection path has no file name")?;
    let target = directory.0.join(name);
    fs::copy(path, &target)?;
    let mut wal_source = path.as_os_str().to_os_string();
    wal_source.push("-wal");
    let wal_path = PathBuf::from(wal_source);
    if wal_path.exists() {
        let mut wal_target = target.as_os_str().to_os_string();
        wal_target.push("-wal");
        fs::copy(&wal_path, PathBuf::from(wal_target))?;
    }

    let conn = Connection::open(&target)?;
    conn.busy_timeout(Duration::from_secs(1))?;
    sample_connection(&conn, query)
}

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    #[expect(
        clippy::excessive_nesting,
        reason = "the retry loop reads clearer than an iterator"
    )]
    fn new() -> io::Result<Self> {
        let base = env::temp_dir();
        for attempt in 0..100 {
            let path = base.join(format!("banki-{}-{attempt}", process::id()));
            if create_directory(&path)? {
                return Ok(Self(path));
            }
        }
        Err(io::Error::other("could not create a temporary directory"))
    }
}

fn create_directory(path: &Path) -> io::Result<bool> {
    match fs::create_dir(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sample_connection(conn: &Connection, query: Query) -> Result<Data> {
    let (days, next_day) = timing(conn, query.now, query.current_west)?;
    let (reviews, day_learning, intraday_learning, mut new): (
        i64,
        i64,
        i64,
        i64,
    ) = conn.query_row(
        "SELECT
           coalesce(sum(CASE WHEN queue = 2 AND due <= ? THEN 1 ELSE 0 END), 0),
           coalesce(sum(CASE WHEN queue = 3 AND due <= ? THEN 1 ELSE 0 END), 0),
           coalesce(sum(CASE WHEN queue = 1 AND due <= ? THEN 1 ELSE 0 END), 0),
           coalesce(sum(CASE WHEN queue = 0 THEN 1 ELSE 0 END), 0)
         FROM cards WHERE queue >= 0",
        (days, days, query.now),
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let completed = conn.query_row(
        "SELECT count(*) FROM revlog WHERE id >= ? AND type BETWEEN 0 AND 3",
        (completed_threshold(next_day),),
        |row| row.get::<_, i64>(0),
    )?;

    if !query.include_new {
        new = 0;
    }
    let learning = intraday_learning.saturating_add(day_learning);
    let remaining = reviews.saturating_add(learning).saturating_add(new);
    Ok(Data {
        reviews,
        learning,
        new,
        remaining,
        completed,
        total: remaining.saturating_add(completed),
        at: query.now,
    })
}

const fn completed_threshold(next_day: i64) -> i64 {
    next_day
        .saturating_sub(SECONDS_PER_DAY)
        .saturating_mul(1000)
}

fn current_west(now: i64) -> i64 {
    Local.timestamp_opt(now, 0).single().map_or(0, |local| {
        i64::from(local.offset().local_minus_utc())
            .checked_div(-60)
            .unwrap_or(0)
    })
}

fn timing(
    conn: &Connection,
    now: i64,
    current_west: i64,
) -> Result<(i64, i64)> {
    let crt: i64 =
        conn.query_row("SELECT crt FROM col", [], |row| row.get(0))?;
    let rollover = match config(conn, "rollover")? {
        Some(value) if !value.is_empty() => value.parse::<i64>()?,
        _ => 4,
    };

    if config(conn, "schedVer")?.as_deref() == Some("1") {
        let days = now.saturating_sub(crt).div_euclid(SECONDS_PER_DAY);
        let next_day = days
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_DAY)
            .saturating_add(crt);
        return Ok((days, next_day));
    }

    let mut planned_next_day = None;
    let days =
        if let Some(created_west_config) = config(conn, "creationOffset")? {
            let created_west = created_west_config.parse::<i64>()?;
            let created = fixed_offset(created_west)?
                .timestamp_opt(crt, 0)
                .single()
                .ok_or("invalid collection creation time")?;
            let local = fixed_offset(current_west)?
                .timestamp_opt(now, 0)
                .single()
                .ok_or("invalid current time")?;
            let today_rollover = rollover_stamp(now, current_west, rollover)?;
            let rollover_passed = today_rollover <= now;
            let mut days = local
                .date_naive()
                .signed_duration_since(created.date_naive())
                .num_days();
            if !rollover_passed {
                days = days.saturating_sub(1);
            }
            let candidate = if rollover_passed {
                today_rollover.saturating_add(SECONDS_PER_DAY)
            } else {
                today_rollover
            };
            planned_next_day = Some(candidate);
            days.max(0)
        } else {
            let crt_rollover = rollover_stamp(crt, current_west, rollover)?;
            now.saturating_sub(crt_rollover).div_euclid(SECONDS_PER_DAY)
        };

    let next_day = if let Some(value) = planned_next_day {
        value
    } else {
        let value = rollover_stamp(now, current_west, rollover)?;
        if value < now {
            value.saturating_add(SECONDS_PER_DAY)
        } else {
            value
        }
    };
    Ok((days, next_day))
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
    let rollover_hour = hour.rem_euclid(24);
    let hour_value = u32::try_from(rollover_hour).unwrap_or(0);
    let naive = local
        .date_naive()
        .and_hms_opt(hour_value, 0, 0)
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
    fn test_expand_path_leaves_absolute_paths_alone() {
        assert_eq!(
            expand_path("/tmp/collection.anki2", Path::new("/plugin")),
            PathBuf::from("/tmp/collection.anki2")
        );
    }
}
