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

use crate::rift::Copy;
use crate::transcript::Status;

/// The database file's name inside the Newjen folder.
pub const DB_FILE: &str = "newjen.db";

/// How long [`Store::flush`] may wait at quit. GPUI gives the whole app about 200 ms to quit,
/// and stopping claude processes needs some of that.
pub const QUIT_FLUSH_TIMEOUT: Duration = Duration::from_millis(100);

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
     );",
];

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
    /// The id of Claude's own conversation (`--session-id` / `--resume`).
    pub session_id: String,
    /// True once claude has reported (with `system/init`) that the conversation exists, so the
    /// next claude must `--resume` it rather than start it.
    pub session_started: bool,
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

/// One operation for the writer thread.
enum Write {
    UpsertProject {
        path: String,
        now: i64,
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
}

impl Store {
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
        spawn_writer(write, queue, errors_sender.clone(), unsaved.clone());
        Ok(Store {
            inner: Arc::new(Inner {
                path,
                read: Mutex::new(read),
                writes,
                errors,
                errors_sender,
                unsaved,
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

    /// Waits until every write queued so far is done, or `timeout` passes. Returns true if
    /// everything was written (or failed and was reported) in time.
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
    /// project's id; it runs once, at startup.
    pub fn open_project(&self, path: &Path) -> Result<Project> {
        let path_text = path_text(path);
        self.queue(Write::UpsertProject {
            path: path_text.clone(),
            now: now_ms(),
        });
        self.flush(OPEN_PROJECT_TIMEOUT);
        self.reader()
            .query_row(
                "SELECT id, path, created_at, last_opened_at FROM projects WHERE path = ?1",
                params![path_text],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        path: PathBuf::from(row.get::<_, String>(1)?),
                        created_at: row.get(2)?,
                        last_opened_at: row.get(3)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| anyhow!("could not save the project {}", path.display()))
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
        if self.inner.writes.try_send(write).is_err() {
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
const THREAD_COLUMNS: &str = "SELECT id, project_id, title, provider, session_id, session_started,
        status, unread, threads.created_at, updated_at, archived_at, threads.workdir,
        copies.path, copies.workdir, source, rift_id, base
     FROM threads LEFT JOIN copies ON copies.thread_id = threads.id";

fn thread_row(row: &Row<'_>) -> rusqlite::Result<ThreadRow> {
    Ok(ThreadRow {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        provider: row.get(3)?,
        session_id: row.get(4)?,
        session_started: row.get(5)?,
        status: Status::from_saved(&row.get::<_, String>(6)?),
        unread: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        archived_at: row.get(10)?,
        workdir: PathBuf::from(row.get::<_, String>(11)?),
        copy: saved_copy(row, 12)?,
    })
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
) {
    thread::spawn(move || {
        while let Ok(write) = queue.recv_blocking() {
            let lost = lost_by(&write);
            if let Err(error) = run_write(&mut connection, write) {
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
        Write::UpsertProject { .. }
        | Write::Archive { .. }
        | Write::CopyRemoved { .. }
        | Write::Flush(_) => return None,
    };
    Some((thread_id.clone(), lost))
}

fn run_write(connection: &mut Connection, write: Write) -> rusqlite::Result<()> {
    match write {
        Write::UpsertProject { path, now } => {
            connection.execute(
                "INSERT INTO projects (path, created_at, last_opened_at) VALUES (?1, ?2, ?2)
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
        Write::Flush(done) => {
            let _ = done.send(());
        }
    }
    Ok(())
}

/// Adds or updates a thread's row (see [`Store::save_thread`]), and adds its copy's row the
/// first time. Run inside a transaction, so the two go together.
fn save_thread_row(connection: &Connection, row: &ThreadRow) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO threads (id, project_id, title, provider, session_id, session_started,
                              status, unread, created_at, updated_at, archived_at, workdir)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (id) DO UPDATE SET
             title = excluded.title,
             provider = excluded.provider,
             session_id = excluded.session_id,
             session_started = excluded.session_started,
             status = excluded.status,
             unread = excluded.unread,
             updated_at = excluded.updated_at",
        params![
            row.id,
            row.project_id,
            row.title,
            row.provider,
            row.session_id,
            row.session_started,
            row.status.as_str(),
            row.unread,
            row.created_at,
            row.updated_at,
            row.archived_at,
            path_text(&row.workdir),
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

    fn row(id: &str, project_id: i64) -> ThreadRow {
        ThreadRow {
            id: id.to_string(),
            project_id,
            workdir: PathBuf::from("/tmp/project"),
            title: id.to_string(),
            provider: "claude".to_string(),
            session_id: format!("session-{id}"),
            session_started: false,
            status: Status::Done,
            unread: false,
            created_at: 1,
            updated_at: 1,
            archived_at: None,
            copy: None,
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
        assert_eq!(user_version(&connection), 2);
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
}
