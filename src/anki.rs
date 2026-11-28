use std::env;
use std::error::Error as StdError;
use std::fs;
use std::io;
use std::mem;
use std::num::NonZero;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process;
use std::result::Result as StdResult;
use std::sync::Mutex;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, ScopedJoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{FixedOffset, Local, TimeZone};
use rusqlite::ffi::ErrorCode;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::lock;

pub type Error = Box<dyn StdError>;
pub type Result<T> = StdResult<T, Error>;

const SECONDS_PER_DAY: i64 = 86_400;
const DEFAULT_ROLLOVER_HOUR: i64 = 4;
const MMAP_SIZE: i64 = 1_073_741_824;
const LOCK_WAIT: Duration = Duration::from_millis(25);

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

impl FileStamp {
    const fn same_file(self, other: Self) -> bool {
        #[cfg(unix)]
        let same_inode =
            self.device == other.device && self.inode == other.inode;
        #[cfg(not(unix))]
        let same_inode = true;
        self.len == other.len && same_inode
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

fn due_values(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, i64)> {
    match (row.get_ref(0)?, row.get_ref(1)?) {
        (ValueRef::Integer(queue), ValueRef::Integer(due)) => Ok((queue, due)),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

impl CardIndex {
    fn from_conn(conn: &Connection) -> rusqlite::Result<Self> {
        let mut stmt =
            conn.prepare("SELECT queue, due FROM cards WHERE queue >= 0")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut index = Self::default();
        index.collect(rows)?;
        index.sort();
        Ok(index)
    }

    fn from_range(
        conn: &Connection,
        low: i64,
        high: i64,
    ) -> rusqlite::Result<Self> {
        let mut stmt = conn.prepare(
            "SELECT queue, due FROM cards WHERE id BETWEEN ? AND ? AND queue >= 0",
        )?;
        stmt.raw_bind_parameter(1, low)?;
        stmt.raw_bind_parameter(2, high)?;
        let mut rows = stmt.raw_query();
        let mut index = Self::default();
        while let Some(row) = rows.next()? {
            let (queue, due) = due_values(row)?;
            index.add(queue, due);
        }
        Ok(index)
    }

    fn collect(
        &mut self,
        rows: impl Iterator<Item = rusqlite::Result<(i64, i64)>>,
    ) -> rusqlite::Result<()> {
        for row in rows {
            let (queue, due) = row?;
            self.add(queue, due);
        }
        Ok(())
    }

    fn merge(&mut self, other: Self) {
        self.reviews.extend(other.reviews);
        self.day_learning.extend(other.day_learning);
        self.intraday_learning.extend(other.intraday_learning);
        self.new = self.new.saturating_add(other.new);
    }

    fn sort(&mut self) {
        self.reviews.sort_unstable();
        self.day_learning.sort_unstable();
        self.intraday_learning.sort_unstable();
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

fn read_chunks() -> usize {
    thread::available_parallelism()
        .map_or(4, NonZero::get)
        .min(7)
}

fn chunk_bounds(low: i64, high: i64) -> Vec<(i64, i64)> {
    let chunks = read_chunks();
    let span = high.saturating_sub(low);
    let count = i64::try_from(chunks).unwrap_or(1);
    let step = span.checked_div(count).unwrap_or(0).saturating_add(1);
    (0..chunks)
        .filter_map(|index| {
            let offset = i64::try_from(index).ok()?;
            let start = low.saturating_add(step.saturating_mul(offset));
            let end = if index.saturating_add(1) == chunks {
                high
            } else {
                low.saturating_add(
                    step.saturating_mul(offset.saturating_add(1)),
                )
                .saturating_sub(1)
            };
            (start <= end).then_some((start, end))
        })
        .collect()
}

fn join_chunk<T>(
    handle: ScopedJoinHandle<'_, rusqlite::Result<T>>,
) -> rusqlite::Result<T> {
    handle.join().map_err(|_| rusqlite::Error::InvalidQuery)?
}

type ChunkMessage<T> = (rusqlite::Result<T>, Connection);
type ChunkQuery<T> = fn(&Connection, i64, i64) -> rusqlite::Result<T>;
type ChunkOutput<T> = (Vec<T>, Vec<Connection>);

#[expect(
    clippy::too_many_arguments,
    reason = "the chunk worker carries a reused connection"
)]
fn run_chunk<T>(
    path: &Path,
    bounds: (i64, i64),
    available: Option<Connection>,
    sender: &Sender<ChunkMessage<T>>,
    query: ChunkQuery<T>,
) -> rusqlite::Result<()> {
    let worker = match available {
        Some(worker) => worker,
        None => open_worker(path)?,
    };
    let value = query(&worker, bounds.0, bounds.1);
    let _ = sender.send((value, worker));
    Ok(())
}

#[expect(
    clippy::excessive_nesting,
    reason = "the scoped worker loop is clearer inline"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the chunk runner is shared by both read phases"
)]
fn run_chunks<T: Send + 'static>(
    path: &Path,
    bounds: &[(i64, i64)],
    available: Vec<Connection>,
    query: ChunkQuery<T>,
) -> rusqlite::Result<ChunkOutput<T>> {
    if bounds.is_empty() {
        return Ok((Vec::new(), available));
    }
    let (sender, receiver) = mpsc::channel();
    let (values, workers) = thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut available = available;
        for &(low, high) in bounds.iter().skip(1) {
            let chunk_sender = sender.clone();
            let chunk_worker = available.pop();
            handles.push(scope.spawn(move || {
                run_chunk(path, (low, high), chunk_worker, &chunk_sender, query)
            }));
        }
        if let Some(&(low, high)) = bounds.first() {
            let worker =
                available.pop().map_or_else(|| open_worker(path), Ok)?;
            let value = query(&worker, low, high);
            let _ = sender.send((value, worker));
        }
        drop(sender);
        for handle in handles {
            join_chunk(handle)?;
        }

        let mut values = Vec::with_capacity(bounds.len());
        let mut workers = Vec::with_capacity(bounds.len());
        for _ in 0..bounds.len() {
            let (value, worker) =
                receiver.recv().map_err(|_| rusqlite::Error::InvalidQuery)?;
            values.push(value?);
            workers.push(worker);
        }
        Ok::<ChunkOutput<T>, rusqlite::Error>((values, workers))
    })?;
    Ok((values, workers))
}

#[expect(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "the parallel reader returns its reusable connections"
)]
fn read_in_parallel(
    conn: Connection,
    path: &Path,
    next_day: i64,
    available: Vec<Connection>,
) -> rusqlite::Result<(CardIndex, i64, Vec<Connection>)> {
    let first_card: Option<i64> =
        conn.query_row("SELECT min(id) FROM cards", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?;
    let last_card: Option<i64> =
        conn.query_row("SELECT max(id) FROM cards", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?;

    let card_bounds = match (first_card, last_card) {
        (Some(first), Some(last)) => chunk_bounds(first, last),
        _ => Vec::new(),
    };
    let mut available = available;
    available.push(conn);
    let (parts, workers) =
        run_chunks(path, &card_bounds, available, CardIndex::from_range)?;
    let mut cards = CardIndex::default();
    for part in parts {
        cards.merge(part);
    }
    cards.sort();

    let (completed, reused) =
        completed_count_parallel(path, next_day, workers)?;
    Ok((cards, completed, reused))
}

fn completed_count_parallel(
    path: &Path,
    next_day: i64,
    mut workers: Vec<Connection>,
) -> rusqlite::Result<(i64, Vec<Connection>)> {
    if workers.is_empty() {
        workers.push(open_worker(path)?);
    }
    let threshold = completed_threshold(next_day);
    let worker = workers
        .first()
        .ok_or_else(|| rusqlite::Error::InvalidQuery)?;
    let last_review: Option<i64> =
        worker.query_row("SELECT max(id) FROM revlog", [], |row| {
            row.get::<_, Option<i64>>(0)
        })?;
    let review_bounds = match last_review {
        Some(last) if last >= threshold => chunk_bounds(threshold, last),
        _ => Vec::new(),
    };
    let (counts, reused) =
        run_chunks(path, &review_bounds, workers, completed_count_range)?;
    let completed = counts.into_iter().fold(0_i64, i64::saturating_add);
    Ok((completed, reused))
}

struct SampleData {
    config: CollectionConfig,
    cards: CardIndex,
    completed_day: i64,
    completed: i64,
}

impl SampleData {
    #[expect(
        clippy::too_many_arguments,
        clippy::type_complexity,
        reason = "the sample returns its reusable connections"
    )]
    fn from_conn(
        conn: Connection,
        path: &Path,
        query: Query,
        workers: Vec<Connection>,
    ) -> Result<(Self, Data, Vec<Connection>)> {
        let config = CollectionConfig::read(&conn)?;
        let (days, next_day) =
            timing_from(&config, query.now, query.current_west)?;
        let (cards, completed, reused) =
            read_in_parallel(conn, path, next_day, workers)?;
        let data = cards.data(query, (days, completed));
        Ok((
            Self {
                config,
                cards,
                completed_day: next_day,
                completed,
            },
            data,
            reused,
        ))
    }
}

struct Cache {
    path: PathBuf,
    identity: CollectionIdentity,
    data: SampleData,
    workers: Vec<Connection>,
    workers_stamp: Option<FileStamp>,
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

fn open_worker(path: &Path) -> rusqlite::Result<Connection> {
    let flags =
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    conn.busy_timeout(LOCK_WAIT)?;
    let _ = conn.pragma_update(None, "mmap_size", MMAP_SIZE);
    Ok(conn)
}

fn open_readonly(path: &Path) -> rusqlite::Result<Connection> {
    let conn = open_worker(path)?;
    // Reading the row also rejects a file that is not an Anki collection.
    conn.query_row("select 1 from col limit 1", [], |_| Ok(()))?;
    Ok(conn)
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
    let mut guard = lock(&CACHE);
    let Some(cache) = guard.as_mut() else {
        return Ok(None);
    };
    if cache.path != path || cache.identity != identity {
        return Ok(None);
    }
    let (days, next_day) =
        timing_from(&cache.data.config, query.now, query.current_west)?;
    if next_day != cache.data.completed_day {
        let workers = mem::take(&mut cache.workers);
        let (completed, reused) =
            match completed_count_parallel(path, next_day, workers) {
                Ok(result) => result,
                Err(error) if is_locked(&error) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
        cache.workers = reused;
        if collection_identity(path).ok().as_ref() != Some(&cache.identity) {
            return Ok(None);
        }
        cache.data.completed_day = next_day;
        cache.data.completed = completed;
    }
    Ok(Some(
        cache.data.cards.data(query, (days, cache.data.completed)),
    ))
}
fn sample_without_cache(path: &Path, query: Query) -> Result<Data> {
    let mut identity = collection_identity(path)?;
    let mut guard = lock(&CACHE);
    let (mut workers, mut workers_stamp) = match guard.take() {
        Some(cache) => {
            let stamp = file_stamp(path).ok().flatten();
            let reusable = cache.path == path
                && cache.workers_stamp.is_some_and(|saved| {
                    stamp.is_some_and(|now| saved.same_file(now))
                });
            if reusable {
                (cache.workers, cache.workers_stamp)
            } else {
                (Vec::new(), None)
            }
        }
        None => (Vec::new(), None),
    };
    let mut attempt: usize = 0;
    loop {
        let conn = match open_readonly(path) {
            Ok(conn) => conn,
            Err(error) if is_locked(&error) => {
                return sample_copy_cached(
                    path,
                    query,
                    (identity, &mut guard, workers, workers_stamp),
                );
            }
            Err(error) => return Err(error.into()),
        };
        let (data, sample, next_workers) =
            match SampleData::from_conn(conn, path, query, workers) {
                Ok(result) => result,
                Err(error) if is_sample_locked(&error) => {
                    return sample_copy_cached(
                        path,
                        query,
                        (identity, &mut guard, Vec::new(), None),
                    );
                }
                Err(error) => return Err(error),
            };
        workers = next_workers;

        let observed = collection_identity(path).ok();
        if observed.as_ref() == Some(&identity) {
            *guard = Some(Cache {
                path: path.to_path_buf(),
                identity,
                data,
                workers,
                workers_stamp: workers_stamp
                    .or_else(|| file_stamp(path).ok().flatten()),
            });
            return Ok(sample);
        }
        if attempt >= 1 {
            // The collection changed during the last allowed attempt. Return
            // the counts, but never cache a sample that mixes states.
            drop(workers);
            return Ok(sample);
        }
        if let Some(latest) = observed {
            identity = latest;
        }
        workers.clear();
        workers_stamp = None;
        attempt = attempt.saturating_add(1);
    }
}

fn is_sample_locked(error: &Error) -> bool {
    error
        .downcast_ref::<rusqlite::Error>()
        .is_some_and(is_locked)
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

type CopyCache<'a> = (
    CollectionIdentity,
    &'a mut Option<Cache>,
    Vec<Connection>,
    Option<FileStamp>,
);

fn sample_copy_cached(
    path: &Path,
    query: Query,
    state: CopyCache<'_>,
) -> Result<Data> {
    let (identity, guard, workers, workers_stamp) = state;
    let (sample, data) = sample_copy(path, query)?;
    if collection_identity(path).ok().as_ref() == Some(&identity) {
        *guard = Some(Cache {
            path: path.to_path_buf(),
            identity,
            data: sample,
            workers,
            workers_stamp,
        });
    }
    Ok(data)
}

/// Counts a copy of the collection, for when Anki holds the write lock.
fn sample_copy(path: &Path, query: Query) -> Result<(SampleData, Data)> {
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
    sample_pair(&conn, query)
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

fn sample_pair(conn: &Connection, query: Query) -> Result<(SampleData, Data)> {
    let config = CollectionConfig::read(conn)?;
    let (days, next_day) = timing_from(&config, query.now, query.current_west)?;
    let cards = CardIndex::from_conn(conn)?;
    let completed = completed_count(conn, next_day)?;
    let data = cards.data(query, (days, completed));
    Ok((
        SampleData {
            config,
            cards,
            completed_day: next_day,
            completed,
        },
        data,
    ))
}

#[cfg(test)]
fn sample_connection(conn: &Connection, query: Query) -> Result<Data> {
    let (_, data) = sample_pair(conn, query)?;
    Ok(data)
}

fn completed_count(conn: &Connection, next_day: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT count(*) FROM revlog WHERE id >= ? AND type BETWEEN 0 AND 3",
        (completed_threshold(next_day),),
        |row| row.get::<_, i64>(0),
    )
}

fn completed_count_range(
    conn: &Connection,
    low: i64,
    high: i64,
) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT count(*) FROM revlog
         WHERE id BETWEEN ? AND ? AND type BETWEEN 0 AND 3",
        (low, high),
        |row| row.get::<_, i64>(0),
    )
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
    static TEST_CACHE_SERIAL: Mutex<()> = Mutex::new(());

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
               (1000099000, 0), (1000099001, 3), (1000099002, 4),
               (1000099003, -1), (1, 0);",
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
        let _serial = lock(&TEST_CACHE_SERIAL);
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
                 CREATE TABLE cards (id INTEGER PRIMARY KEY, queue INTEGER, due INTEGER);
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
            .execute("INSERT INTO cards (id, queue, due) VALUES (1, 0, 0)", [])
            .expect("card");
        drop(writer);

        let first = sample(&path, true, now).expect("first sample");
        assert_eq!(first.new, 1);
        let cached = sample(&path, true, now).expect("cached sample");
        assert_eq!(cached.new, 1);

        let writer = Connection::open(&path).expect("collection");
        writer
            .execute("INSERT INTO cards (id, queue, due) VALUES (2, 0, 0)", [])
            .expect("new card");
        drop(writer);

        let updated = sample(&path, true, now).expect("updated sample");
        assert_eq!(updated.new, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sample_cache_reuses_cards_after_day_rollover() {
        let _serial = lock(&TEST_CACHE_SERIAL);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = Path::new(".tmp")
            .join(format!("banki-rollover-{}-{nanos}", process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("collection.anki2");
        let crt = 1_000_000;
        let next_day = crt + SECONDS_PER_DAY;
        let writer = Connection::open(&path).expect("collection");
        writer
            .execute_batch(
                "CREATE TABLE col (crt INTEGER);
                 CREATE TABLE config (key TEXT PRIMARY KEY, val);
                 CREATE TABLE cards (id INTEGER PRIMARY KEY, queue INTEGER, due INTEGER);
                 CREATE TABLE revlog (id INTEGER, type INTEGER);",
            )
            .expect("schema");
        writer
            .execute("INSERT INTO col (crt) VALUES (?)", (crt,))
            .expect("col row");
        set(&writer, "schedVer", "1");
        writer
            .execute_batch(
                "INSERT INTO cards (id, queue, due) VALUES
                   (1, 2, 0), (2, 2, 1), (3, 2, 2);
                 INSERT INTO revlog (id, type) VALUES (1000000001, 0), (1086400001, 0);",
            )
            .expect("rows");
        drop(writer);

        let before =
            sample(&path, false, next_day + 60).expect("before rollover");
        let after = sample(&path, false, next_day + SECONDS_PER_DAY + 60)
            .expect("after rollover");

        assert_eq!(before.reviews, 2);
        assert_eq!(before.completed, 1);
        assert_eq!(after.reviews, 3);
        assert_eq!(after.completed, 0);
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

    fn collection_with_cards(path: &Path, cards: i64, now: i64) {
        let writer = Connection::open(path).expect("collection");
        writer
            .execute_batch(
                "CREATE TABLE col (crt INTEGER);
                 CREATE TABLE config (key TEXT PRIMARY KEY, val);
                 CREATE TABLE cards (id INTEGER PRIMARY KEY, queue INTEGER, due INTEGER);
                 CREATE TABLE revlog (id INTEGER, type INTEGER);",
            )
            .expect("schema");
        writer
            .execute(
                "INSERT INTO col (crt) VALUES (?)",
                (now.saturating_sub(3600),),
            )
            .expect("col row");
        set(&writer, "schedVer", "1");
        for id in 1..=cards {
            writer
                .execute(
                    "INSERT INTO cards (id, queue, due) VALUES (?1, 0, 0)",
                    [id],
                )
                .expect("card");
        }
    }

    #[test]
    fn test_sample_switching_collections_uses_fresh_connections() {
        let _serial = lock(&TEST_CACHE_SERIAL);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = env::temp_dir()
            .join(format!("banki-paths-{}-{nanos}", process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let left = dir.join("left.anki2");
        let right = dir.join("right.anki2");
        let now = unix_seconds();
        collection_with_cards(&left, 1, now);
        collection_with_cards(&right, 2, now);

        assert_eq!(sample(&left, true, now).expect("left").new, 1);
        assert_eq!(sample(&right, true, now).expect("right").new, 2);
        assert_eq!(sample(&left, true, now).expect("left again").new, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_sample_replaced_collection_uses_fresh_connections() {
        let _serial = lock(&TEST_CACHE_SERIAL);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = env::temp_dir()
            .join(format!("banki-replaced-{}-{nanos}", process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("collection.anki2");
        let replacement = dir.join("replacement.anki2");
        let now = unix_seconds();
        collection_with_cards(&path, 1, now);
        collection_with_cards(&replacement, 2, now);

        assert_eq!(sample(&path, true, now).expect("before").new, 1);
        fs::rename(&replacement, &path).expect("replace collection");
        assert_eq!(sample(&path, true, now).expect("after").new, 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_locked_collection_copy_is_cached() {
        let _serial = lock(&TEST_CACHE_SERIAL);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = env::temp_dir()
            .join(format!("banki-locked-{}-{nanos}", process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("collection.anki2");
        let now = unix_seconds();
        collection_with_cards(&path, 1, now);
        let writer = Connection::open(&path).expect("writer");
        writer
            .execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE;")
            .expect("exclusive lock");
        writer
            .execute("UPDATE col SET crt = crt", [])
            .expect("hold lock");

        let first = sample(&path, true, now).expect("first sample");
        {
            let guard = lock(&CACHE);
            let cached = guard.as_ref().expect("cached locked copy");
            assert_eq!(cached.path, path);
        }
        let second = sample(&path, true, now).expect("cached sample");

        assert_eq!(first, second);
        drop(writer);
        let _ = fs::remove_dir_all(&dir);
    }
}
