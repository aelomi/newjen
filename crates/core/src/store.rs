//! Newjen's own database (SQLite): projects, their threads, each thread's transcript rows, its
//! "Always allow" grants and its Rift copy. Claude keeps the full conversation in its own files; this is the
//! small copy Newjen needs to show threads again after a relaunch.
//!
//! - **Where:** `$NEWJEN_HOME/newjen.db` if `NEWJEN_HOME` is set, else
//!   `~/Library/Application Support/Newjen/newjen.db` (see [`default_dir`]).
//! - **Reads** (open a project, list threads, load a thread's items and grants) are plain
//!   calls on a read-only connection. They are quick, and happen when a project opens.
//! - **Writes** go to a background *writer thread* through a queue, like the stdin writer in
//!   `session.rs`: each write method puts an operation on the queue and returns at once, so
//!   the UI thread never waits for the disk. [`Store::flush`] waits until the queue is empty
//!   (at quit, and in tests). A failed write never panics: it arrives on
//!   [`Store::write_errors`] for the app to show once, and the store remembers what it lost
//!   for that thread, so the thread's next save writes it again (see `Thread::save`).
//! - **Schema changes** are numbered steps in `MIGRATIONS`. SQLite's `user_version` remembers
//!   how many have run, so each runs once.
//!
//! All times are Unix time in milliseconds (see [`now_ms`]).

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use async_channel::{Receiver, Sender};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, params};

use crate::landing::LandingMode;
use crate::rift::Copy;
use crate::transcript::Status;

/// The database file's name inside the Newjen folder.
pub const DB_FILE: &str = "newjen.db";

/// The database wait budget at quit, separate from SQLite's per-write [`BUSY_TIMEOUT`].
pub const QUIT_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long opening a project may wait for its row to be written. Only at startup.
const OPEN_PROJECT_TIMEOUT: Duration = Duration::from_secs(5);

/// SQLite waits this long for a lock held by another connection before it gives up.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// The schema, one step per version. Step `n` (counting from 1) upgrades a database at
/// `user_version` `n - 1`. Never edit a step that has shipped; add a new one instead.
const MIGRATIONS: &[&str] = &[
    // Version 1: projects, threads, transcript rows and grants.
    "CREATE TABLE projects (
         id INTEGER PRIMARY KEY,
         path TEXT NOT NULL UNIQUE,
         created_at INTEGER NOT NULL,
         last_opened_at INTEGER NOT NULL
     );
     CREATE TABLE threads (
         id TEXT PRIMARY KEY,
         project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
         workdir TEXT NOT NULL,
         title TEXT NOT NULL,
         provider TEXT NOT NULL DEFAULT 'claude',
         session_id TEXT NOT NULL,
         session_started INTEGER NOT NULL DEFAULT 0,
         status TEXT NOT NULL,
         unread INTEGER NOT NULL DEFAULT 0,
         created_at INTEGER NOT NULL,
         updated_at INTEGER NOT NULL,
         archived_at INTEGER
     );
     CREATE INDEX threads_by_project ON threads(project_id, created_at);
     CREATE TABLE thread_items (
         thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
         ix INTEGER NOT NULL,
         schema_version INTEGER NOT NULL,
         json TEXT NOT NULL,
         PRIMARY KEY (thread_id, ix)
     );
     CREATE TABLE grants (
         thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
         rule TEXT NOT NULL,
         label TEXT NOT NULL,
         created_at INTEGER NOT NULL,
         PRIMARY KEY (thread_id, rule)
     );",
    // Version 2: each thread's Rift copy, kept after the thread is archived until the copy is
    // removed.
    "CREATE TABLE copies (
         thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
         path TEXT NOT NULL,
         workdir TEXT NOT NULL,
         source TEXT NOT NULL,
         rift_id TEXT NOT NULL,
         base TEXT,
         created_at INTEGER NOT NULL,
         removed_at INTEGER
     );
     CREATE TABLE pending_copies (
         thread_id TEXT PRIMARY KEY,
         project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
         path TEXT NOT NULL UNIQUE,
         message TEXT,
         state TEXT NOT NULL,
         rift_id TEXT,
         created_at INTEGER NOT NULL
     );
     CREATE TABLE copy_cleanup (
         path TEXT PRIMARY KEY,
         project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
         thread_id TEXT NOT NULL,
         rift_id TEXT
     );",
    "ALTER TABLE threads ADD COLUMN conversation_id TEXT;
     ALTER TABLE threads ADD COLUMN conversation_state TEXT NOT NULL DEFAULT 'uncreated'
         CHECK (conversation_state IN ('uncreated', 'reserved', 'established'));
     ALTER TABLE threads ADD COLUMN model TEXT;
     ALTER TABLE threads ADD COLUMN effort TEXT;
     ALTER TABLE threads ADD COLUMN speed TEXT NOT NULL DEFAULT 'normal'
         CHECK (speed IN ('normal', 'fast'));
     UPDATE threads SET conversation_id = session_id,
         conversation_state = CASE WHEN session_started THEN 'established' ELSE 'reserved' END
         WHERE provider = 'claude';",
    "CREATE TABLE project_agent_choices (
         project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
         config TEXT NOT NULL
     );
     CREATE TABLE thread_limits (
         thread_id TEXT PRIMARY KEY REFERENCES threads(id) ON DELETE CASCADE,
         report TEXT NOT NULL
     );",
    "ALTER TABLE projects ADD COLUMN landing_mode TEXT NOT NULL DEFAULT 'harness_managed'
         CHECK (landing_mode IN ('harness_managed', 'agent_managed'));
     ALTER TABLE threads ADD COLUMN landing_mode TEXT NOT NULL DEFAULT 'harness_managed'
         CHECK (landing_mode IN ('harness_managed', 'agent_managed'));
     ALTER TABLE threads ADD COLUMN review_base TEXT;
     ALTER TABLE threads ADD COLUMN landing_resolved INTEGER NOT NULL DEFAULT 0;
     ALTER TABLE threads ADD COLUMN snapshot_needed INTEGER NOT NULL DEFAULT 0;",
];

/// The `user_version` of a database at the current schema.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// The current time as Unix milliseconds, the unit every time in the store uses.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
}

/// The folder Newjen keeps its database in: `$NEWJEN_HOME` if set, else
/// `~/Library/Application Support/Newjen`.
pub fn default_dir() -> Result<PathBuf> {
    resolve_dir(std::env::var_os("NEWJEN_HOME"), std::env::var_os("HOME"))
        .context("neither NEWJEN_HOME nor HOME is set, so Newjen has nowhere to save threads")
}

/// [`default_dir`] from the two environment values, so it can be tested without changing the
/// process's environment. An empty value counts as unset.
fn resolve_dir(newjen_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let set = |value: Option<OsString>| value.filter(|value| !value.is_empty());
    set(newjen_home).map(PathBuf::from).or_else(|| {
        set(home).map(|home| PathBuf::from(home).join("Library/Application Support/Newjen"))
    })
}

/// A project folder Newjen has opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: i64,
    pub path: PathBuf,
    pub created_at: i64,
    pub last_opened_at: i64,
    pub landing_mode: LandingMode,
}

/// One saved thread (the `threads` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadRow {
    /// A v4 UUID, made by Newjen.
    pub id: String,
    pub project_id: i64,
    /// The folder the thread's agent works in, and always restarts from: the project folder,
    /// or the [`Copy::workdir`] of its Rift copy. It never changes, because Claude ties
    /// `--resume` to the folder a conversation started in.
    pub workdir: PathBuf,
    pub title: String,
    /// Which agent CLI runs the thread. Always `"claude"` for now.
    pub provider: String,
    pub conversation: crate::agent::ConversationState,
    pub model: crate::providers::ModelSelection,
    pub effort: crate::agent::EffortSelection,
    pub speed: crate::agent::Speed,
    pub status: Status,
    /// Something happened while the thread was not on screen.
    pub unread: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// When the thread was archived (hidden). `None` for a visible thread.
    pub archived_at: Option<i64>,
    /// The thread's Rift copy, if it works in one (the `copies` table). Saved with the
    /// thread's first row and never changed after.
    pub copy: Option<Copy>,
    pub landing_mode: LandingMode,
    pub review_base: Option<String>,
    pub landing_resolved: bool,
    pub snapshot_needed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingIntent {
    Restore(String),
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingState {
    MakingCopy,
    Draft,
    Sent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCopy {
    pub thread_id: String,
    pub project_id: i64,
    pub path: PathBuf,
    pub intent: PendingIntent,
    pub state: PendingState,
    pub rift_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyCleanup {
    pub path: PathBuf,
    pub project_id: i64,
    pub thread_id: String,
    pub rift_id: Option<String>,
}

/// One saved transcript row (the `thread_items` table). Read it with `Item::from_saved`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedItem {
    /// The item's position in the transcript, from 0.
    pub ix: usize,
    pub schema_version: u32,
    pub json: String,
}

/// One "Always allow" rule a thread remembers (the `grants` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The rule in the CLI's notation, e.g. `Bash(touch one.txt)` (see
    /// `protocol::SessionRule::cli_rule`). Passed to claude with `--allowedTools`.
    pub rule: String,
    /// What the dock showed for it, e.g. `touch one.txt`.
    pub label: String,
    pub created_at: i64,
}

/// A write that failed. Its `Display` is the sentence the app shows:
/// "Couldn't save threads: …".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteError {
    /// What went wrong, e.g. SQLite's own message.
    pub detail: String,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Couldn't save threads: {}", self.detail)
    }
}

/// What failed writes left unsaved for one thread. Its next save writes these again (see
/// `Thread::save`), so one failed write never leaves a gap in the saved rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Unsaved {
    /// The thread's row (title, session, status, unread flag).
    pub row: bool,
    /// Its transcript rows from this position on.
    pub items_from: Option<usize>,
    /// Its grants.
    pub grants: bool,
}

impl Unsaved {
    /// Adds what another failed write lost.
    fn merge(&mut self, other: Unsaved) {
        self.row |= other.row;
        self.grants |= other.grants;
        self.items_from = match (self.items_from, other.items_from) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Failed writes per thread id, filled by the writer thread (see [`Unsaved`]).
type UnsavedByThread = Arc<Mutex<HashMap<String, Unsaved>>>;
type SaveObservers = Arc<Mutex<Vec<std::sync::Weak<Mutex<Option<String>>>>>>;

/// Failures observed during one save operation, retained independently of retry records.
#[derive(Clone)]
pub struct SaveObservation(Arc<Mutex<Option<String>>>);

impl SaveObservation {
    pub fn failure(&self) -> Option<String> {
        lock(&self.0).clone()
    }
}

/// An immutable receipt for writes covered by a save operation.
pub struct SaveReceipt {
    done: mpsc::Receiver<Result<(), String>>,
}

impl SaveReceipt {
    pub fn wait(self) -> Result<(), String> {
        self.done
            .recv()
            .unwrap_or_else(|_| Err("the database writer stopped".into()))
    }
}

/// One operation for the writer thread.
enum Write {
    #[cfg(test)]
    Hold {
        reached: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
    RememberAgent {
        project_id: i64,
        config: String,
    },
    Limit {
        thread_id: String,
        report: String,
    },
    SavePending {
        pending: PendingCopy,
        done: mpsc::Sender<Result<(), String>>,
    },
    PendingId {
        thread_id: String,
        rift_id: String,
        done: mpsc::Sender<Result<(), String>>,
    },
    DraftMessage {
        thread_id: String,
        message: String,
    },
    DeletePending(String),
    DeleteCopyCleanup(PathBuf),
    DiscardPending(String),
    UpsertProject {
        path: String,
        now: i64,
    },
    SetLandingMode {
        project_id: i64,
        mode: LandingMode,
    },
    SaveThread(ThreadRow),
    ReplaceItems {
        thread_id: String,
        from_ix: usize,
        items: Vec<(u32, String)>,
    },
    /// `SaveThread` and `ReplaceItems` in one transaction.
    Checkpoint {
        row: ThreadRow,
        from_ix: usize,
        items: Vec<(u32, String)>,
    },
    AddGrant {
        thread_id: String,
        grant: Grant,
    },
    RemoveGrant {
        thread_id: String,
        rule: String,
    },
    /// Replaces all of a thread's grants (to redo grant writes that failed).
    ReplaceGrants {
        thread_id: String,
        grants: Vec<Grant>,
    },
    SetUnread {
        thread_id: String,
        unread: bool,
    },
    Archive {
        thread_id: String,
        at: i64,
    },
    CopyRemoved {
        thread_id: String,
        at: i64,
    },
    /// Answered once every operation queued before it is done.
    Flush(mpsc::Sender<()>),
    Receipt {
        done: mpsc::Sender<Result<(), String>>,
        failure: Arc<Mutex<Option<String>>>,
    },
}

/// The database. Cheap to clone: every clone shares the same connections and writer thread,
/// so each thread of the app can keep one.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    /// For reads only (opened read-only). A mutex because a connection must not be used by two
    /// threads at once.
    read: Mutex<Connection>,
    /// The writer thread's queue. When the last `Store` clone is dropped, the queue closes and
    /// the writer finishes what is queued, then exits.
    writes: Sender<Write>,
    /// Failed writes, for the app (see [`Store::write_errors`]).
    errors: Receiver<WriteError>,
    errors_sender: Sender<WriteError>,
    /// What failed writes lost, for each thread to write again (see [`Store::take_unsaved`]).
    unsaved: UnsavedByThread,
    observers: SaveObservers,
}

impl Store {
    pub fn last_agent_choice(&self, project_id: i64) -> Result<Option<crate::AgentConfig>> {
        let json: Option<String> = self
            .reader()
            .query_row(
                "SELECT config FROM project_agent_choices WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json.and_then(|json| serde_json::from_str(&json).ok()))
    }
    pub fn remember_agent_choice(&self, project_id: i64, config: &crate::AgentConfig) {
        self.queue(Write::RememberAgent {
            project_id,
            config: serde_json::to_string(config).expect("agent config serialization"),
        });
    }
    pub fn thread_limit(&self, id: &str) -> Result<Option<crate::agent::RateLimitEvent>> {
        let json: Option<String> = self
            .reader()
            .query_row(
                "SELECT report FROM thread_limits WHERE thread_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(json.and_then(|json| serde_json::from_str(&json).ok()))
    }
    pub fn save_thread_limit(&self, id: &str, report: &crate::agent::RateLimitEvent) {
        self.queue(Write::Limit {
            thread_id: id.into(),
            report: serde_json::to_string(report).expect("limit serialization"),
        });
    }

    /// Opens (or creates) the database in [`default_dir`].
    pub fn open() -> Result<Store> {
        Store::open_in(&default_dir()?)
    }

    /// Opens (or creates) `newjen.db` in `dir`, creating the folder if needed, and brings its
    /// schema up to date.
    pub fn open_in(dir: &Path) -> Result<Store> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
        let path = dir.join(DB_FILE);
        let mut write = Connection::open(&path)
            .with_context(|| format!("could not open {}", path.display()))?;
        // WAL lets the read connection read while the writer writes. NORMAL is safe with WAL:
        // a power cut can lose the last moments of writes, never corrupt the file.
        write.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        write.busy_timeout(BUSY_TIMEOUT)?;
        migrate(&mut write, MIGRATIONS).context("could not update the database's tables")?;

        let read = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("could not open {} for reading", path.display()))?;
        read.execute_batch("PRAGMA foreign_keys = ON;")?;
        read.busy_timeout(BUSY_TIMEOUT)?;

        let (writes, queue) = async_channel::unbounded();
        let (errors_sender, errors) = async_channel::unbounded();
        let unsaved = UnsavedByThread::default();
        let observers = Arc::default();
        spawn_writer(
            write,
            queue,
            errors_sender.clone(),
            unsaved.clone(),
            Arc::clone(&observers),
        );
        Ok(Store {
            inner: Arc::new(Inner {
                path,
                read: Mutex::new(read),
                writes,
                errors,
                errors_sender,
                unsaved,
                observers,
            }),
        })
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Failed writes, in order. Await it (the app) or poll it (tests). There should be one
    /// listener: each error is delivered once. Nothing needs redoing: the thread a failed write
    /// belonged to writes what it lost again at its next save.
    pub fn write_errors(&self) -> Receiver<WriteError> {
        self.inner.errors.clone()
    }

    /// What failed writes left unsaved for `thread_id`, if anything, forgotten as it is
    /// returned: the caller writes it again.
    pub(crate) fn take_unsaved(&self, thread_id: &str) -> Option<Unsaved> {
        lock(&self.inner.unsaved).remove(thread_id)
    }

    /// Whether failed writes left anything unsaved for `thread_id`, without forgetting it.
    pub(crate) fn has_unsaved(&self, thread_id: &str) -> bool {
        lock(&self.inner.unsaved).contains_key(thread_id)
    }

    #[cfg(test)]
    pub(crate) fn hold_writer(&self) -> mpsc::Sender<()> {
        let (reached, wait) = mpsc::channel();
        let (release, held) = mpsc::channel();
        self.queue(Write::Hold {
            reached,
            release: held,
        });
        wait.recv().unwrap();
        release
    }

    /// Observe errors before preparation begins, including failures before the barrier.
    pub fn observe_saves(&self) -> SaveObservation {
        let failure = SaveObservation(Arc::new(Mutex::new(None)));
        lock(&self.inner.observers).push(Arc::downgrade(&failure.0));
        failure
    }

    pub fn save_receipt(&self, failure: SaveObservation) -> SaveReceipt {
        let (done, wait) = mpsc::channel();
        self.queue(Write::Receipt {
            done,
            failure: failure.0,
        });
        SaveReceipt { done: wait }
    }

    pub fn update_pending_draft(&self, thread_id: &str, message: &str) {
        self.queue(Write::DraftMessage {
            thread_id: thread_id.into(),
            message: message.into(),
        });
    }

    /// Waits until every write queued so far is done, or `timeout` passes. Returns true if
    /// everything was written (or failed and was reported) in time, false if `timeout` passed
    /// or the writer stopped.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (done, wait) = mpsc::channel();
        if self.inner.writes.try_send(Write::Flush(done)).is_err() {
            return false;
        }
        wait.recv_timeout(timeout).is_ok()
    }

    // -----------------------------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------------------------

    /// Records that the project at `path` was opened (adding it the first time) and returns it.
    /// Unlike other writes this waits for the disk, because every thread row needs the
    /// project's id. Run it off the UI thread when switching projects.
    pub fn open_project(&self, path: &Path) -> Result<Project> {
        let path_text = path_text(path);
        self.queue(Write::UpsertProject {
            path: path_text.clone(),
            now: now_ms(),
        });
        self.flush(OPEN_PROJECT_TIMEOUT);
        self.reader()
            .query_row(
                "SELECT id, path, created_at, last_opened_at, landing_mode FROM projects WHERE path = ?1",
                params![path_text],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        path: PathBuf::from(row.get::<_, String>(1)?),
                        created_at: row.get(2)?,
                        last_opened_at: row.get(3)?,
                        landing_mode: saved_landing_mode(row, 4)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("could not save the project {}", path.display()))
    }

    pub fn projects(&self) -> Result<Vec<Project>> {
        let read = self.reader();
        let mut statement = read.prepare(
            "SELECT id, path, created_at, last_opened_at, landing_mode FROM projects
             ORDER BY last_opened_at DESC, id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                path: PathBuf::from(row.get::<_, String>(1)?),
                created_at: row.get(2)?,
                last_opened_at: row.get(3)?,
                landing_mode: saved_landing_mode(row, 4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_default_landing_mode(&self, project_id: i64, mode: LandingMode) {
        self.queue(Write::SetLandingMode { project_id, mode });
    }

    /// The project's threads that are not archived, newest first. Activity never reorders
    /// them: a thread keeps its place from when it was created.
    pub fn threads(&self, project_id: i64) -> Result<Vec<ThreadRow>> {
        let read = self.reader();
        let mut statement = read.prepare(&format!(
            "{THREAD_COLUMNS}
             WHERE project_id = ?1 AND archived_at IS NULL
             ORDER BY threads.created_at DESC, threads.rowid DESC"
        ))?;
        let rows = statement.query_map(params![project_id], thread_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One thread, archived or not.
    pub fn thread(&self, thread_id: &str) -> Result<Option<ThreadRow>> {
        let read = self.reader();
        let row = read
            .query_row(
                &format!("{THREAD_COLUMNS} WHERE id = ?1"),
                params![thread_id],
                thread_row,
            )
            .optional()?;
        Ok(row)
    }

    /// A thread's saved transcript rows, in order.
    pub fn items(&self, thread_id: &str) -> Result<Vec<SavedItem>> {
        let read = self.reader();
        let mut statement = read.prepare(
            "SELECT ix, schema_version, json FROM thread_items
             WHERE thread_id = ?1 ORDER BY ix",
        )?;
        let rows = statement.query_map(params![thread_id], |row| {
            Ok(SavedItem {
                ix: row.get::<_, i64>(0)? as usize,
                schema_version: row.get::<_, i64>(1)? as u32,
                json: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The Rift copies of archived threads that are not removed yet, with their threads' ids.
    /// Removing them at startup finishes what an earlier run could not (see `Removal`).
    pub fn copies_to_remove(&self) -> Result<Vec<(String, Copy)>> {
        let read = self.reader();
        let mut statement = read.prepare(
            "SELECT copies.thread_id, path, copies.workdir, source, rift_id, base
             FROM copies JOIN threads ON threads.id = copies.thread_id
             WHERE threads.archived_at IS NOT NULL AND copies.removed_at IS NULL
             ORDER BY copies.created_at",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, saved_copy(row, 1)?.expect("path is NOT NULL")))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The id of the thread whose Rift copy is at `path` and not removed yet, archived or not.
    pub fn copy_owner(&self, path: &Path) -> Result<Option<String>> {
        Ok(self
            .reader()
            .query_row(
                "SELECT thread_id FROM copies WHERE path = ?1 AND removed_at IS NULL",
                params![path_text(path)],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn pending_copies(&self, project_id: i64) -> Result<Vec<PendingCopy>> {
        let read = self.reader();
        let mut statement = read.prepare(
            "SELECT thread_id, project_id, path, message, created_at, state, rift_id FROM pending_copies
             WHERE project_id = ?1 ORDER BY created_at",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok(PendingCopy {
                thread_id: row.get(0)?,
                project_id: row.get(1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                intent: row
                    .get::<_, Option<String>>(3)?
                    .map_or(PendingIntent::Discard, PendingIntent::Restore),
                rift_id: row.get(6)?,
                created_at: row.get(4)?,
                state: match row.get::<_, String>(5)?.as_str() {
                    "making_copy" => PendingState::MakingCopy,
                    "sent" => PendingState::Sent,
                    _ => PendingState::Draft,
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn copy_cleanup(&self, project_id: i64) -> Result<Vec<CopyCleanup>> {
        let read = self.reader();
        let mut statement = read.prepare(
            "SELECT path, project_id, thread_id, rift_id FROM copy_cleanup WHERE project_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([project_id], |row| {
            Ok(CopyCleanup {
                path: PathBuf::from(row.get::<_, String>(0)?),
                project_id: row.get(1)?,
                thread_id: row.get(2)?,
                rift_id: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_copy_cleanup(&self, path: &Path) {
        self.queue(Write::DeleteCopyCleanup(path.to_path_buf()));
    }

    pub fn save_pending_copy(&self, pending: PendingCopy) -> Result<()> {
        let (done, wait) = mpsc::channel();
        self.queue(Write::SavePending { pending, done });
        wait.recv_timeout(OPEN_PROJECT_TIMEOUT)
            .context("could not save the message before making a Rift copy")?
            .map_err(anyhow::Error::msg)
    }

    pub(crate) fn save_pending_id(&self, thread_id: &str, rift_id: &str) -> Result<()> {
        let (done, wait) = mpsc::channel();
        self.queue(Write::PendingId {
            thread_id: thread_id.to_string(),
            rift_id: rift_id.to_string(),
            done,
        });
        wait.recv_timeout(OPEN_PROJECT_TIMEOUT)
            .context("could not save the Rift id")?
            .map_err(anyhow::Error::msg)
    }

    pub fn delete_pending_copy(&self, thread_id: &str) {
        self.queue(Write::DeletePending(thread_id.to_string()));
    }

    pub fn discard_pending_copy(&self, thread_id: &str) {
        self.queue(Write::DiscardPending(thread_id.to_string()));
    }

    /// A thread's grants, oldest first.
    pub fn grants(&self, thread_id: &str) -> Result<Vec<Grant>> {
        let read = self.reader();
        let mut statement = read.prepare(
            "SELECT rule, label, created_at FROM grants
             WHERE thread_id = ?1 ORDER BY created_at, rule",
        )?;
        let rows = statement.query_map(params![thread_id], |row| {
            Ok(Grant {
                rule: row.get(0)?,
                label: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -----------------------------------------------------------------------------------------
    // Writes (queued; each returns at once)
    // -----------------------------------------------------------------------------------------

    /// Adds the thread, or updates its title, session, status, unread flag and `updated_at`.
    /// (Its project, creation time and archive time never change here.)
    pub fn save_thread(&self, row: ThreadRow) {
        self.queue(Write::SaveThread(row));
    }

    /// Replaces the thread's rows from `from_ix` on with `items` (each a schema version and
    /// JSON text, see `Item::to_saved`), in one transaction. Rows at `from_ix` and after that
    /// are not in `items` are deleted, so a shorter transcript never leaves stale rows behind.
    pub fn replace_items(&self, thread_id: &str, from_ix: usize, items: Vec<(u32, String)>) {
        self.queue(Write::ReplaceItems {
            thread_id: thread_id.to_string(),
            from_ix,
            items,
        });
    }

    /// [`Store::save_thread`] and then [`Store::replace_items`] for the same thread, in one
    /// transaction: a crash in between can never leave the row's status ahead of its items
    /// (say "stopped" without the notice that goes with it).
    pub fn save_checkpoint(&self, row: ThreadRow, from_ix: usize, items: Vec<(u32, String)>) {
        self.queue(Write::Checkpoint {
            row,
            from_ix,
            items,
        });
    }

    /// Remembers a grant for the thread (replacing the label if the rule is already there).
    pub fn add_grant(&self, thread_id: &str, grant: Grant) {
        self.queue(Write::AddGrant {
            thread_id: thread_id.to_string(),
            grant,
        });
    }

    /// Forgets a grant.
    pub fn remove_grant(&self, thread_id: &str, rule: &str) {
        self.queue(Write::RemoveGrant {
            thread_id: thread_id.to_string(),
            rule: rule.to_string(),
        });
    }

    /// Replaces all of the thread's grants with `grants`, in one transaction. Redoes grant
    /// writes that failed, so a revoked rule never comes back at the next launch.
    pub(crate) fn replace_grants(&self, thread_id: &str, grants: Vec<Grant>) {
        self.queue(Write::ReplaceGrants {
            thread_id: thread_id.to_string(),
            grants,
        });
    }

    /// Sets the thread's unread flag (without touching `updated_at`).
    pub fn set_unread(&self, thread_id: &str, unread: bool) {
        self.queue(Write::SetUnread {
            thread_id: thread_id.to_string(),
            unread,
        });
    }

    /// Hides the thread from the list. Its rows stay in the database.
    pub fn archive(&self, thread_id: &str, at: i64) {
        self.queue(Write::Archive {
            thread_id: thread_id.to_string(),
            at,
        });
    }

    /// Records that the thread's Rift copy was removed.
    pub fn mark_copy_removed(&self, thread_id: &str, at: i64) {
        self.queue(Write::CopyRemoved {
            thread_id: thread_id.to_string(),
            at,
        });
    }

    fn queue(&self, write: Write) {
        // The writer only stops once every `Store` is gone, so this cannot fail while `self`
        // exists, unless the writer thread itself crashed. Say so rather than lose the write.
        let lost = lost_by(&write);
        if self.inner.writes.try_send(write).is_err() {
            if let Some((id, lost)) = lost {
                lock(&self.inner.unsaved).entry(id).or_default().merge(lost);
            }
            lock(&self.inner.observers).retain(|observer| {
                if let Some(failure) = observer.upgrade() {
                    lock(&failure).get_or_insert_with(|| "the database writer stopped".into());
                    true
                } else {
                    false
                }
            });
            let _ = self.inner.errors_sender.try_send(WriteError {
                detail: "the database writer stopped".to_string(),
            });
        }
    }

    fn reader(&self) -> MutexGuard<'_, Connection> {
        lock(&self.inner.read)
    }
}

/// Locks a mutex. A panic while it was held leaves the data inside fine here (a connection,
/// or a map that is only ever inserted into and removed from), so carry on (see
/// `session::lock`).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Runs the steps of `steps` the database has not had yet, each in its own transaction, and
/// records the new version in `user_version`. A database from a newer Newjen (a higher version
/// than `steps` knows) is left as it is.
fn migrate(connection: &mut Connection, steps: &[&str]) -> rusqlite::Result<()> {
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (index, sql) in steps.iter().enumerate().skip(version.max(0) as usize) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(sql)?;
        transaction.pragma_update(None, "user_version", index as i64 + 1)?;
        transaction.commit()?;
    }
    Ok(())
}

/// The text the store keeps for a path. macOS paths are nearly always UTF-8; any other byte is
/// replaced, which only affects how the path reads back.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The `SELECT … FROM` for [`thread_row`]: a thread's columns, then its copy's.
const THREAD_COLUMNS: &str = "SELECT id, project_id, title, provider, conversation_id, conversation_state,
        status, unread, threads.created_at, updated_at, archived_at, threads.workdir,
        copies.path, copies.workdir, source, rift_id, base, threads.model, threads.effort, threads.speed,
        landing_mode, review_base, landing_resolved, snapshot_needed
     FROM threads LEFT JOIN copies ON copies.thread_id = threads.id";

fn thread_row(row: &Row<'_>) -> rusqlite::Result<ThreadRow> {
    Ok(ThreadRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        provider: row.get(3)?,
        conversation: crate::agent::ConversationState::parse(
            &row.get::<_, String>(5)?,
            row.get(4)?,
        )
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, error.into())
        })?,
        model: row
            .get::<_, Option<String>>(17)?
            .map_or(crate::providers::ModelSelection::CliDefault, |id| {
                crate::providers::ModelSelection::Model { id }
            }),
        effort: row.get::<_, Option<String>>(18)?.map_or(
            crate::agent::EffortSelection::CliDefault,
            crate::agent::EffortSelection::Chosen,
        ),
        speed: match row.get::<_, String>(19)?.as_str() {
            "normal" => crate::agent::Speed::Normal,
            "fast" => crate::agent::Speed::Fast,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        status: Status::from_saved(&row.get::<_, String>(6)?),
        unread: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        archived_at: row.get(10)?,
        workdir: PathBuf::from(row.get::<_, String>(11)?),
        copy: saved_copy(row, 12)?,
        landing_mode: saved_landing_mode(row, 20)?,
        review_base: row.get(21)?,
        landing_resolved: row.get(22)?,
        snapshot_needed: row.get(23)?,
    })
}

fn saved_landing_mode(row: &Row<'_>, at: usize) -> rusqlite::Result<LandingMode> {
    row.get::<_, String>(at)?
        .parse()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

/// The copy in the five columns from `at` (path, workdir, source, Rift id, base), if any.
fn saved_copy(row: &Row<'_>, at: usize) -> rusqlite::Result<Option<Copy>> {
    let Some(path) = row.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    Ok(Some(Copy {
        path: PathBuf::from(path),
        workdir: PathBuf::from(row.get::<_, String>(at + 1)?),
        source: PathBuf::from(row.get::<_, String>(at + 2)?),
        rift_id: row.get(at + 3)?,
        base: row.get(at + 4)?,
    }))
}

/// Starts the writer thread: it runs queued writes in order until the queue closes. A failed
/// write is recorded in `unsaved` for its thread, then reported on `errors`, and the next one
/// still runs.
fn spawn_writer(
    mut connection: Connection,
    queue: Receiver<Write>,
    errors: Sender<WriteError>,
    unsaved: UnsavedByThread,
    observers: SaveObservers,
) {
    thread::spawn(move || {
        while let Ok(write) = queue.recv_blocking() {
            #[cfg(test)]
            if let Write::Hold { reached, release } = write {
                let _ = reached.send(());
                let _ = release.recv();
                continue;
            }
            if let Write::Receipt { done, failure } = write {
                let result = lock(&failure).clone().map_or_else(
                    || {
                        if lock(&unsaved).is_empty() {
                            Ok(())
                        } else {
                            Err("recent thread changes are not saved".into())
                        }
                    },
                    Err,
                );
                let _ = done.send(result);
                continue;
            }
            let lost = lost_by(&write);
            if let Err(error) = run_write(&mut connection, write) {
                lock(&observers).retain(|observer| {
                    if let Some(failure) = observer.upgrade() {
                        lock(&failure).get_or_insert_with(|| error.to_string());
                        true
                    } else {
                        false
                    }
                });
                // Recorded before the error is sent, so whoever sees the error finds it.
                if let Some((thread_id, lost)) = lost {
                    lock(&unsaved).entry(thread_id).or_default().merge(lost);
                }
                let _ = errors.try_send(WriteError {
                    detail: error.to_string(),
                });
            }
        }
    });
}

/// What `write` would leave unsaved, and for which thread, if it failed. An archive is not
/// redone: the thread is hidden for this run anyway, and shows again at the next launch.
fn lost_by(write: &Write) -> Option<(String, Unsaved)> {
    let (thread_id, lost) = match write {
        #[cfg(test)]
        Write::Hold { .. } => return None,
        Write::SaveThread(row) => (
            &row.id,
            Unsaved {
                row: true,
                ..Unsaved::default()
            },
        ),
        Write::ReplaceItems {
            thread_id, from_ix, ..
        } => (
            thread_id,
            Unsaved {
                items_from: Some(*from_ix),
                ..Unsaved::default()
            },
        ),
        Write::Checkpoint { row, from_ix, .. } => (
            &row.id,
            Unsaved {
                row: true,
                items_from: Some(*from_ix),
                ..Unsaved::default()
            },
        ),
        Write::AddGrant { thread_id, .. }
        | Write::RemoveGrant { thread_id, .. }
        | Write::ReplaceGrants { thread_id, .. } => (
            thread_id,
            Unsaved {
                grants: true,
                ..Unsaved::default()
            },
        ),
        // The unread flag is part of the row.
        Write::SetUnread { thread_id, .. } => (
            thread_id,
            Unsaved {
                row: true,
                ..Unsaved::default()
            },
        ),
        // Not redone: the copy is removed again at the next launch, which finds nothing to do
        // and records it then.
        Write::RememberAgent { .. }
        | Write::Limit { .. }
        | Write::SetLandingMode { .. }
        | Write::UpsertProject { .. }
        | Write::Archive { .. }
        | Write::CopyRemoved { .. }
        | Write::SavePending { .. }
        | Write::PendingId { .. }
        | Write::DraftMessage { .. }
        | Write::DeletePending(_)
        | Write::DeleteCopyCleanup(_)
        | Write::DiscardPending(_)
        | Write::Flush(_)
        | Write::Receipt { .. } => return None,
    };
    Some((thread_id.clone(), lost))
}

fn run_write(connection: &mut Connection, write: Write) -> rusqlite::Result<()> {
    match write {
        Write::SetLandingMode { project_id, mode } => {
            connection.execute(
                "UPDATE projects SET landing_mode = ?2 WHERE id = ?1",
                params![project_id, mode.as_str()],
            )?;
        }
        Write::RememberAgent { project_id, config } => {
            connection.execute("INSERT INTO project_agent_choices (project_id, config) VALUES (?1, ?2) ON CONFLICT(project_id) DO UPDATE SET config=excluded.config", params![project_id, config])?;
        }
        Write::Limit { thread_id, report } => {
            connection.execute("INSERT INTO thread_limits (thread_id, report) VALUES (?1, ?2) ON CONFLICT(thread_id) DO UPDATE SET report=excluded.report", params![thread_id, report])?;
        }

        Write::SavePending { pending, done } => {
            let message = match pending.intent {
                PendingIntent::Restore(message) => Some(message),
                PendingIntent::Discard => None,
            };
            let result = (|| {
                let transaction = connection.transaction()?;
                retain_copy_cleanup(&transaction, &pending.thread_id, Some(&pending.path))?;
                transaction.execute(
                    "INSERT INTO pending_copies (thread_id, project_id, path, message, created_at, state, rift_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT (thread_id) DO UPDATE SET message = excluded.message, state = excluded.state, path = excluded.path, rift_id = excluded.rift_id",
                    params![
                        pending.thread_id,
                        pending.project_id,
                        path_text(&pending.path),
                        message,
                        pending.created_at,
                        match pending.state {
                            PendingState::MakingCopy => "making_copy",
                            PendingState::Draft => "draft",
                            PendingState::Sent => "sent",
                        },
                        pending.rift_id,
                    ],
                )?;
                transaction.commit()
            })();
            let _ = done.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
            result?;
        }
        Write::PendingId {
            thread_id,
            rift_id,
            done,
        } => {
            let result = connection.execute(
                "UPDATE pending_copies SET rift_id = ?2 WHERE thread_id = ?1",
                params![thread_id, rift_id],
            );
            let _ = done.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
            result?;
        }
        Write::DeleteCopyCleanup(path) => {
            connection.execute(
                "DELETE FROM copy_cleanup WHERE path = ?1",
                [path_text(&path)],
            )?;
        }
        Write::DeletePending(thread_id) => {
            connection.execute(
                "DELETE FROM pending_copies WHERE thread_id = ?1",
                [thread_id],
            )?;
        }
        Write::DiscardPending(thread_id) => {
            connection.execute(
                "UPDATE pending_copies SET message = NULL WHERE thread_id = ?1",
                [thread_id],
            )?;
        }
        Write::UpsertProject { path, now } => {
            connection.execute(
                "INSERT INTO projects (path, created_at, last_opened_at)
                 VALUES (?1, ?2, MAX(?2, COALESCE((SELECT MAX(last_opened_at) + 1 FROM projects), ?2)))
                 ON CONFLICT (path) DO UPDATE SET last_opened_at = excluded.last_opened_at",
                params![path, now],
            )?;
        }
        Write::SaveThread(row) => {
            let transaction = connection.transaction()?;
            save_thread_row(&transaction, &row)?;
            transaction.commit()?;
        }
        Write::ReplaceItems {
            thread_id,
            from_ix,
            items,
        } => {
            let transaction = connection.transaction()?;
            replace_item_rows(&transaction, &thread_id, from_ix, &items)?;
            transaction.commit()?;
        }
        Write::Checkpoint {
            row,
            from_ix,
            items,
        } => {
            let transaction = connection.transaction()?;
            save_thread_row(&transaction, &row)?;
            replace_item_rows(&transaction, &row.id, from_ix, &items)?;
            transaction.commit()?;
        }
        Write::AddGrant { thread_id, grant } => {
            connection.execute(
                "INSERT INTO grants (thread_id, rule, label, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (thread_id, rule) DO UPDATE SET label = excluded.label",
                params![thread_id, grant.rule, grant.label, grant.created_at],
            )?;
        }
        Write::RemoveGrant { thread_id, rule } => {
            connection.execute(
                "DELETE FROM grants WHERE thread_id = ?1 AND rule = ?2",
                params![thread_id, rule],
            )?;
        }
        Write::ReplaceGrants { thread_id, grants } => {
            let transaction = connection.transaction()?;
            transaction.execute(
                "DELETE FROM grants WHERE thread_id = ?1",
                params![thread_id],
            )?;
            for grant in grants {
                transaction.execute(
                    "INSERT INTO grants (thread_id, rule, label, created_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![thread_id, grant.rule, grant.label, grant.created_at],
                )?;
            }
            transaction.commit()?;
        }
        Write::SetUnread { thread_id, unread } => {
            connection.execute(
                "UPDATE threads SET unread = ?2 WHERE id = ?1",
                params![thread_id, unread],
            )?;
        }
        Write::Archive { thread_id, at } => {
            connection.execute(
                "UPDATE threads SET archived_at = ?2 WHERE id = ?1",
                params![thread_id, at],
            )?;
        }
        Write::CopyRemoved { thread_id, at } => {
            connection.execute(
                "UPDATE copies SET removed_at = ?2 WHERE thread_id = ?1 AND removed_at IS NULL",
                params![thread_id, at],
            )?;
        }
        // Everything queued before this is done (writes run in order).
        Write::DraftMessage { thread_id, message } => {
            connection.execute(
                "UPDATE pending_copies SET message = ?2 WHERE thread_id = ?1 AND state != 'sent'",
                params![
                    thread_id,
                    if message.trim().is_empty() {
                        None
                    } else {
                        Some(message)
                    }
                ],
            )?;
        }
        #[cfg(test)]
        Write::Hold { .. } => unreachable!("handled at the writer boundary"),
        Write::Receipt { .. } => unreachable!("handled at the writer boundary"),
        Write::Flush(done) => {
            let _ = done.send(());
        }
    }
    Ok(())
}

fn retain_copy_cleanup(
    connection: &Connection,
    thread_id: &str,
    keep: Option<&Path>,
) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO copy_cleanup (path, project_id, thread_id, rift_id)
         SELECT path, project_id, thread_id, rift_id FROM pending_copies
         WHERE thread_id = ?1 AND state = 'making_copy' AND (?2 IS NULL OR path != ?2)
         ON CONFLICT (path) DO NOTHING",
        params![thread_id, keep.map(path_text)],
    )?;
    Ok(())
}

/// Adds or updates a thread's row (see [`Store::save_thread`]), and adds its copy's row the
/// first time. Run inside a transaction, so the two go together.
fn save_thread_row(connection: &Connection, row: &ThreadRow) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO threads (id, project_id, title, provider, session_id, session_started,
                              status, unread, created_at, updated_at, archived_at, workdir, conversation_id, conversation_state, model, effort, speed,
                              landing_mode, review_base, landing_resolved, snapshot_needed)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
         ON CONFLICT (id) DO UPDATE SET
             title = excluded.title,
             provider = excluded.provider,
             session_id = excluded.session_id,
             session_started = excluded.session_started,
             conversation_id = excluded.conversation_id,
             conversation_state = excluded.conversation_state,
             model = excluded.model,
             effort = excluded.effort,
             speed = excluded.speed,
             status = excluded.status,
             unread = excluded.unread,
             updated_at = excluded.updated_at,
             landing_mode = excluded.landing_mode,
             review_base = excluded.review_base,
             landing_resolved = excluded.landing_resolved,
             snapshot_needed = excluded.snapshot_needed",
        params![
            row.id,
            row.project_id,
            row.title,
            row.provider,
            if row.provider == "claude" { row.conversation.id().unwrap_or("") } else { "" },
            row.provider == "claude" && matches!(row.conversation, crate::agent::ConversationState::Established(_)),
            row.status.as_str(),
            row.unread,
            row.created_at,
            row.updated_at,
            row.archived_at,
            path_text(&row.workdir),
            row.conversation.id(),
            row.conversation.stored_state(),
            match &row.model { crate::providers::ModelSelection::CliDefault => None, crate::providers::ModelSelection::Model { id } => Some(id) },
            match &row.effort { crate::agent::EffortSelection::CliDefault => None, crate::agent::EffortSelection::Chosen(effort) => Some(effort) },
            match row.speed { crate::agent::Speed::Normal => "normal", crate::agent::Speed::Fast => "fast" },
            row.landing_mode.as_str(),
            row.review_base,
            row.landing_resolved,
            row.snapshot_needed,
        ],
    )?;
    if let Some(copy) = &row.copy {
        connection.execute(
            "INSERT INTO copies (thread_id, path, workdir, source, rift_id, base, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (thread_id) DO NOTHING",
            params![
                row.id,
                path_text(&copy.path),
                path_text(&copy.workdir),
                path_text(&copy.source),
                copy.rift_id,
                copy.base,
                now_ms(),
            ],
        )?;
    }
    retain_copy_cleanup(
        connection,
        &row.id,
        row.copy.as_ref().map(|copy| copy.path.as_path()),
    )?;
    connection.execute("DELETE FROM pending_copies WHERE thread_id = ?1", [&row.id])?;
    Ok(())
}

/// Replaces a thread's item rows from `from_ix` on (see [`Store::replace_items`]). Run inside
/// a transaction, so the delete and the inserts happen together.
fn replace_item_rows(
    connection: &Connection,
    thread_id: &str,
    from_ix: usize,
    items: &[(u32, String)],
) -> rusqlite::Result<()> {
    connection.execute(
        "DELETE FROM thread_items WHERE thread_id = ?1 AND ix >= ?2",
        params![thread_id, from_ix as i64],
    )?;
    let mut insert = connection.prepare(
        "INSERT INTO thread_items (thread_id, ix, schema_version, json) VALUES (?1, ?2, ?3, ?4)",
    )?;
    for (offset, (schema_version, json)) in items.iter().enumerate() {
        let ix = (from_ix + offset) as i64;
        insert.execute(params![thread_id, ix, schema_version, json])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newjen_home_wins_over_home() {
        let dir = resolve_dir(Some("/tmp/nj".into()), Some("/Users/someone".into()));
        assert_eq!(dir, Some(PathBuf::from("/tmp/nj")));
        let dir = resolve_dir(None, Some("/Users/someone".into()));
        assert_eq!(
            dir,
            Some(PathBuf::from(
                "/Users/someone/Library/Application Support/Newjen"
            ))
        );
        // Empty counts as unset.
        let dir = resolve_dir(Some("".into()), Some("/Users/someone".into()));
        assert_eq!(
            dir,
            Some(PathBuf::from(
                "/Users/someone/Library/Application Support/Newjen"
            ))
        );
        assert_eq!(resolve_dir(None, None), None);
    }

    fn user_version(connection: &Connection) -> i64 {
        connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn migrations_run_once_and_only_the_new_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.db");
        let step_one = "CREATE TABLE one (x INTEGER);";
        let step_two = "CREATE TABLE two (y INTEGER);";

        let mut connection = Connection::open(&path).unwrap();
        migrate(&mut connection, &[step_one]).unwrap();
        assert_eq!(user_version(&connection), 1);
        // Running again is a no-op (it would fail if step one ran twice: the table exists).
        migrate(&mut connection, &[step_one]).unwrap();
        // A newer Newjen adds step two: only that one runs.
        migrate(&mut connection, &[step_one, step_two]).unwrap();
        assert_eq!(user_version(&connection), 2);
        // An older Newjen that knows only step one leaves the newer database alone.
        migrate(&mut connection, &[step_one]).unwrap();
        assert_eq!(user_version(&connection), 2);
    }

    #[test]
    fn legacy_conversations_and_rift_metadata_migrate_together() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection, &MIGRATIONS[..1]).unwrap();
        connection.execute_batch("INSERT INTO projects VALUES (1, '/tmp/project', 1, 1);
            INSERT INTO threads VALUES ('reserved', 1, '/tmp/project', 'Draft', 'claude', 'old-r', 0, 'done', 0, 1, 1, NULL);
            INSERT INTO threads VALUES ('established', 1, '/tmp/copy', 'Saved', 'claude', 'old-e', 1, 'done', 0, 1, 1, NULL);").unwrap();
        migrate(&mut connection, &MIGRATIONS[..2]).unwrap();
        connection.execute_batch("INSERT INTO copies VALUES ('established', '/tmp/copy', '/tmp/copy', '/tmp/project', 'rift-id', 'base', 1, NULL);").unwrap();
        migrate(&mut connection, MIGRATIONS).unwrap();
        assert_eq!(user_version(&connection), SCHEMA_VERSION);
        let mut query = connection.prepare(THREAD_COLUMNS).unwrap();
        let rows: Vec<_> = query
            .query_map([], thread_row)
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(
            rows[0].conversation,
            crate::agent::ConversationState::Reserved("old-r".into())
        );
        assert_eq!(
            rows[1].conversation,
            crate::agent::ConversationState::Established("old-e".into())
        );
        assert_eq!(rows[1].copy.as_ref().unwrap().rift_id, "rift-id");
        assert!(rows.iter().all(
            |row| row.model == crate::providers::ModelSelection::CliDefault
                && row.effort == crate::agent::EffortSelection::CliDefault
                && row.speed == crate::agent::Speed::Normal
        ));
        assert!(
            connection
                .execute("UPDATE threads SET speed = 'invented'", [])
                .is_err()
        );
    }

    fn row(id: &str, project_id: i64) -> ThreadRow {
        ThreadRow {
            id: id.to_string(),
            project_id,
            workdir: PathBuf::from("/tmp/project"),
            title: id.to_string(),
            provider: "claude".to_string(),
            conversation: crate::agent::ConversationState::Reserved(format!("session-{id}")),
            model: crate::providers::ModelSelection::CliDefault,
            effort: crate::agent::EffortSelection::CliDefault,
            speed: crate::agent::Speed::Normal,
            status: Status::Done,
            unread: false,
            created_at: 1,
            updated_at: 1,
            archived_at: None,
            copy: None,
            landing_mode: crate::landing::LandingMode::HarnessManaged,
            review_base: None,
            landing_resolved: false,
            snapshot_needed: false,
        }
    }

    fn grant(rule: &str) -> Grant {
        Grant {
            rule: rule.to_string(),
            label: rule.to_string(),
            created_at: 1,
        }
    }

    #[test]
    fn a_failed_write_is_remembered_for_its_thread_until_taken() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).unwrap();
        let project = store.open_project(Path::new("/tmp/project")).unwrap();
        let item = || (1, "{}".to_string());
        store.save_checkpoint(row("t1", project.id), 0, vec![item(), item(), item()]);
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(store.take_unsaved("t1"), None);

        // From here on the disk refuses new items and grants (as a full disk would).
        let other = Connection::open(store.path()).unwrap();
        other
            .execute_batch(
                "CREATE TRIGGER refuse_items BEFORE INSERT ON thread_items
                     BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;
                 CREATE TRIGGER refuse_grants BEFORE INSERT ON grants
                     BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
            )
            .unwrap();
        store.save_checkpoint(row("t1", project.id), 3, vec![item()]);
        store.replace_items("t1", 1, vec![item()]);
        store.add_grant("t1", grant("Bash(ls)"));
        store.save_checkpoint(row("t2", project.id), 0, vec![item()]);
        assert!(store.flush(Duration::from_secs(5)));

        // Each thread's losses, merged: the lowest item position, the row, the grants.
        assert_eq!(
            store.take_unsaved("t1"),
            Some(Unsaved {
                row: true,
                items_from: Some(1),
                grants: true,
            })
        );
        assert_eq!(store.take_unsaved("t1"), None, "handed out once");
        assert_eq!(
            store.take_unsaved("t2"),
            Some(Unsaved {
                row: true,
                items_from: Some(0),
                grants: false,
            })
        );
        let errors = store.write_errors();
        let mut reported = 0;
        while let Ok(error) = errors.try_recv() {
            assert!(error.to_string().starts_with("Couldn't save threads: "));
            reported += 1;
        }
        assert_eq!(reported, 4, "every failure is still reported");
        // The failed checkpoint changed nothing (its row and items are one transaction).
        assert_eq!(store.items("t1").unwrap().len(), 3);
    }

    #[test]
    fn grants_can_be_replaced_all_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).unwrap();
        let project = store.open_project(Path::new("/tmp/project")).unwrap();
        store.save_thread(row("t1", project.id));
        store.add_grant("t1", grant("Bash(a)"));
        store.add_grant("t1", grant("Bash(b)"));
        store.replace_grants("t1", vec![grant("Bash(b)"), grant("Bash(c)")]);
        assert!(store.flush(Duration::from_secs(5)));
        let rules: Vec<String> = store
            .grants("t1")
            .unwrap()
            .into_iter()
            .map(|grant| grant.rule)
            .collect();
        assert_eq!(rules, ["Bash(b)", "Bash(c)"]);
    }

    #[test]
    fn a_version_1_database_gains_copies_and_keeps_its_threads() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut connection = Connection::open(dir.path().join(DB_FILE)).unwrap();
            migrate(&mut connection, &MIGRATIONS[..1]).unwrap();
            connection
                .execute_batch(
                    "INSERT INTO projects (id, path, created_at, last_opened_at)
                         VALUES (1, '/tmp/project', 1, 1);
                     INSERT INTO threads (id, project_id, workdir, title, session_id, status,
                                          created_at, updated_at)
                         VALUES ('t1', 1, '/tmp/project', 'Old', 's1', 'done', 1, 1);",
                )
                .unwrap();
        }

        let store = Store::open_in(dir.path()).unwrap();
        let connection = Connection::open(store.path()).unwrap();
        assert_eq!(user_version(&connection), MIGRATIONS.len() as i64);
        let old = store.thread("t1").unwrap().expect("kept");
        assert_eq!(old.title, "Old");
        assert_eq!(old.copy, None);

        let copy = Copy {
            path: PathBuf::from("/tmp/.rifts/project/c"),
            workdir: PathBuf::from("/tmp/.rifts/project/c"),
            source: PathBuf::from("/tmp/project"),
            rift_id: "01RIFT".to_string(),
            base: None,
        };
        let mut new = row("t2", 1);
        new.copy = Some(copy.clone());
        store.save_thread(new);
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(store.thread("t2").unwrap().unwrap().copy, Some(copy));
    }

    #[test]
    fn a_failed_step_leaves_the_version_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = Connection::open(dir.path().join("m.db")).unwrap();
        let broken = "CREATE TABLE ok (x INTEGER); THIS IS NOT SQL;";
        assert!(migrate(&mut connection, &[broken]).is_err());
        assert_eq!(user_version(&connection), 0);
        // The transaction was rolled back, so the table from the step's first half is gone.
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'ok'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }
    #[test]
    fn save_receipt_reports_writer_disconnection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).unwrap();
        let observed = store.observe_saves();
        store.inner.writes.close();
        let receipt = store.save_receipt(observed.clone());
        assert!(receipt.wait().is_err());
        assert!(observed.failure().is_some());
    }
    #[test]
    fn quit_draft_update_preserves_making_copy_recovery_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).unwrap();
        let project = store.open_project(dir.path()).unwrap();
        store
            .save_pending_copy(PendingCopy {
                thread_id: "unfinished".into(),
                project_id: project.id,
                path: dir.path().join("unfinished-copy"),
                intent: PendingIntent::Restore("held message".into()),
                state: PendingState::MakingCopy,
                rift_id: Some("owned-copy".into()),
                created_at: 1,
            })
            .unwrap();
        let release = store.hold_writer();
        let observed = store.observe_saves();
        store.update_pending_draft("unfinished", "restored composer");
        let receipt = store.save_receipt(observed);
        release.send(()).unwrap();
        assert_eq!(receipt.wait(), Ok(()));
        let reopened = Store::open_in(dir.path()).unwrap();
        let pending = &reopened.pending_copies(project.id).unwrap()[0];
        assert_eq!(pending.state, PendingState::MakingCopy);
        assert_eq!(
            pending.intent,
            PendingIntent::Restore("restored composer".into())
        );
        assert_eq!(pending.rift_id.as_deref(), Some("owned-copy"));
    }
    #[test]
    fn save_receipt_covers_late_loader_failure_after_an_earlier_success() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).unwrap();
        let observed = store.observe_saves();
        assert_eq!(store.save_receipt(observed.clone()).wait(), Ok(()));
        let db = Connection::open(store.path()).unwrap();
        db.execute_batch("CREATE TRIGGER fail_loader BEFORE INSERT ON projects BEGIN SELECT RAISE(ABORT, 'loader write failed'); END;").unwrap();
        store.queue(Write::UpsertProject {
            path: "late-project".into(),
            now: 0,
        });
        assert!(store.save_receipt(observed).wait().is_err());
    }
}
