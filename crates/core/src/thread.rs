//! One conversation ("thread") and everything that belongs to it, without any UI: its saved
//! row, its [`Transcript`], its `claude` process (started only when needed), its "Always allow"
//! grants and its unread flag. The app keeps one [`Thread`] per conversation, so several can
//! run at once, and everything here can be tested headlessly.
//!
//! **Events.** Each thread has one event channel for its whole life ([`Thread::events`]). Every
//! `claude` it starts gets a small forwarding thread that copies that process's
//! [`SessionEvent`]s into it, tagged with a *generation* number. Whenever the thread stops a
//! process or starts a new one the generation goes up, and [`Thread::apply`] ignores events
//! from older generations. So the app pumps a single channel per thread, and a process the
//! thread already let go of can never change it.
//!
//! **Claude's session id.** Allocated when the thread is created. claude is started with
//! `--session-id <id>` until it reports the conversation exists (`system/init`), and with
//! `--resume <id>` after that. Two failures are repaired once per user message by starting
//! claude again and resending that message (see [`Thread::apply`]):
//! - "Session ID … is already in use" (the conversation exists after all): resume it.
//! - "No conversation found" for this id (Claude no longer has it): start a new one with a new
//!   id, after a quiet row saying so. The earlier messages stay on screen. This also happens
//!   after "Try again", which has no message to resend: the next message starts the new one.
//!
//! A repair applies only while claude is starting (before its first `system/init`), and resends
//! a message only while its turn runs; a Stop pressed in that window ends the turn at once
//! instead (see [`Thread::stop`]).
//!
//! **Where it works.** In the project folder, or in a Rift copy of its own. A draft chooses
//! ([`Thread::set_works_in`]). A draft that wants a copy holds its first message while the copy
//! is made: [`Thread::send`] hands back a [`CopyJob`] for the app to run off the UI thread, and
//! [`Thread::finish_copy`] then moves the thread into the copy and sends the message, once.
//! The copy is the thread's folder from then on, because Claude ties `--resume` to the folder.
//! Archiving the thread hands back a [`Removal`] that deletes the copy.
//!
//! **Saving.** A thread is a *draft* until its first message is sent; drafts are never saved.
//! After that, the row and the changed transcript rows are saved at checkpoints only (a
//! message sent, a reply or tool call finished, an approval asked or answered, the turn's end,
//! an error), never for each streamed word. If a write fails, the store remembers what it
//! lost, and the thread's next save writes that again.
//!
//! **Stopping claude** (idle, revoke, archive, quit) happens on a background thread, because
//! it can take up to about 150 ms and the app must not wait for it. A new `claude` for the
//! thread is only started once the old one is gone. Stopping claude also stops the commands it
//! runs in the background ([`Thread::background_tasks`]), so it is never stopped for being idle
//! while one runs.
//!
//! **Subscription safety.** Nothing here starts a turn on its own. A turn starts only from
//! [`Thread::send`] (or, for a first message held for a Rift copy, [`Thread::finish_copy`]);
//! the two repairs above resend the user's own message once. A copy that could not be made is
//! never tried again by itself. (claude itself starts a turn when a background task it ran
//! finishes; [`Thread::apply`] follows that turn like any other.)

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use async_channel::{Receiver, Sender};

use crate::protocol::{BackgroundTask, Incoming, PermissionAnswer, ResultMessage, session_rules};
use crate::rift::{self, CopyError, Tools};
use crate::session::{Conversation, Session, SessionConfig, SessionEvent};
use crate::store::{Grant, Project, QUIT_FLUSH_TIMEOUT, Store, ThreadRow, now_ms};
use crate::transcript::{Item, Status, Transcript};

/// The provider a thread runs on. Only Claude for now (see `docs/PLAN.md`).
pub const PROVIDER_CLAUDE: &str = "claude";

/// A thread's title before its first message.
pub const NEW_THREAD_TITLE: &str = "New thread";

/// Titles longer than this many characters are cut, with "…" after them.
pub const TITLE_MAX_CHARS: usize = 50;

/// A thread's `claude` is stopped after this long without activity, but never mid-turn, while
/// an approval waits or while a background task runs. The next message starts it again
/// (`--resume`). T3 Code and Codex also use 30 minutes.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What the Continue button on a "Stopped when Newjen quit" notice sends, as a visible message,
/// when claude got the user's last message (see [`Thread::continue_text`]).
pub const CONTINUE_MESSAGE: &str = "Continue where you left off.";

/// True if `line` is what claude prints on stderr (and then exits 1) when `--session-id`
/// names a conversation that already exists: `Error: Session ID <id> is already in use.`
/// Only this exact message for this id counts, so another error that happens to say "already
/// in use" (a busy port, say) is shown as it is.
fn is_session_in_use(line: &str, session_id: &str) -> bool {
    line.contains(&format!("Session ID {session_id} is already in use"))
}

/// True if `result` is claude's "No conversation found" error for `session_id`. A message that
/// names another session id does not count.
fn is_missing_conversation(result: &ResultMessage, session_id: &str) -> bool {
    const NAMED: &str = "No conversation found with session ID:";
    let names_another = |text: &String| {
        text.split_once(NAMED).is_some_and(|(_, rest)| {
            rest.split_whitespace()
                .next()
                .map(|id| id.trim_end_matches('.'))
                != Some(session_id)
        })
    };
    result.is_missing_conversation()
        && !result
            .error_lines()
            .iter()
            .chain(result.result.as_ref())
            .any(names_another)
}

/// One event from one of the thread's `claude` processes. Pass it to [`Thread::apply`].
#[derive(Debug, Clone)]
pub struct ThreadEvent {
    /// Which process sent it (see the module docs).
    generation: u64,
    event: SessionEvent,
}

impl ThreadEvent {
    /// The session event itself.
    pub fn event(&self) -> &SessionEvent {
        &self.event
    }
}

/// One conversation. See the module docs.
pub struct Thread {
    id: String,
    project_id: i64,
    project_dir: PathBuf,
    /// The folder claude works in.
    cwd: PathBuf,
    place: Place,
    title: String,
    provider: String,
    /// Claude's own conversation id (`--session-id` / `--resume`).
    session_id: String,
    /// True once claude has reported the conversation exists, so it must be resumed.
    session_started: bool,
    transcript: Transcript,
    /// The running `claude`, if any. Started on the first message that needs it.
    session: Option<Session>,
    /// The running `claude` has sent `system/init`, so it got past the start-up checks where
    /// the two repairs apply.
    session_initialized: bool,
    /// `claude` processes being stopped in the background (see the module docs).
    stopping: Vec<JoinHandle<()>>,
    /// How to start claude (program, environment); the conversation and grants are filled in
    /// per start (see [`claude_config`]).
    base_config: SessionConfig,
    /// See the module docs. Goes up whenever a process is started or let go.
    generation: u64,
    events_in: Sender<ThreadEvent>,
    events: Receiver<ThreadEvent>,
    grants: Vec<Grant>,
    unread: bool,
    /// Whether this is the thread on screen (see [`Thread::set_active`]).
    active: bool,
    stage: Stage,
    created_at: i64,
    updated_at: i64,
    /// The last time anything happened (an event, a message, an answer), for idle shutdown.
    last_activity: Instant,
    /// When the running turn started, for "Working · 2m". `None` when no turn runs.
    turn_started: Option<Instant>,
    /// A grant was revoked during a turn: stop claude when the turn ends, so the next message
    /// starts one without that rule.
    stop_after_turn: bool,
    recovery: Recovery,
    /// What the store last got for the thread's row, so unchanged rows are not rewritten.
    saved_row: Option<RowState>,
    store: Store,
}

/// Where a thread is in its life. Only a saved thread is written to the store.
#[derive(Debug)]
enum Stage {
    /// Nothing sent yet.
    Draft,
    /// The first message waits while the thread's Rift copy is made.
    Preparing(Preparing),
    /// Its first message was sent.
    Saved,
}

/// Where a thread works.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    ProjectFolder,
    /// A Rift copy of its own, made when the first message is sent. Drafts only.
    NewCopy,
    Copy(rift::Copy),
}

/// Where a draft works, as the user chose (see [`Thread::set_works_in`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorksIn {
    ProjectFolder,
    RiftCopy,
}

/// A first message held while the thread's Rift copy is made (see [`Thread::send`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preparing {
    message: String,
    started_at: Instant,
    last_line: Option<String>,
}

impl Preparing {
    /// The message, sent once the copy is ready.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// When the user sent it, for "Making a Rift copy · 3s".
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// The last line Rift or its hooks printed (see [`Thread::note_copy_line`]).
    pub fn last_line(&self) -> Option<&str> {
        self.last_line.as_deref()
    }
}

/// What [`Thread::send`] did with a message.
#[derive(Debug)]
pub enum Sent {
    /// It went to claude.
    Started,
    /// It is held until the thread's Rift copy is made: run the job off the UI thread and pass
    /// what it returns to [`Thread::finish_copy`].
    MakingCopy(CopyJob),
}

/// Makes a thread's Rift copy ([`rift::create`]). It can take seconds, so the app runs it on a
/// background thread.
#[must_use = "run it, then pass the result to `Thread::finish_copy`"]
pub struct CopyJob {
    project_dir: PathBuf,
    name: String,
}

impl std::fmt::Debug for CopyJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyJob")
            .field("project_dir", &self.project_dir)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl CopyJob {
    /// Makes the copy. Each line Rift and its hooks print goes to `on_line` (pass it on to
    /// [`Thread::note_copy_line`]). An existing destination is never removed.
    pub fn run(
        self,
        tools: &Tools,
        mut on_line: impl FnMut(&str),
    ) -> Result<rift::Copy, CopyError> {
        rift::create(tools, &self.project_dir, &self.name, &mut on_line)
    }
}

/// A first message that could not be sent, back for the composer, and why.
#[derive(Debug)]
pub struct NotSent {
    /// Empty when nothing was held.
    pub message: String,
    pub error: anyhow::Error,
}

/// Removes an archived thread's Rift copy, once its `claude` has stopped, and records that it
/// is gone. It can take seconds, so the app runs it on a background thread.
#[must_use = "run it off the UI thread, or the copy stays until the next launch"]
pub struct Removal {
    thread_id: String,
    copy: rift::Copy,
    stopping: Vec<JoinHandle<()>>,
    store: Store,
}

impl Removal {
    /// The copies of threads archived before (say, Newjen quit before removing them). The app
    /// runs these at startup. Running one that is already gone just records it.
    pub fn pending(store: &Store) -> Result<Vec<Removal>> {
        Ok(store
            .copies_to_remove()?
            .into_iter()
            .map(|(thread_id, copy)| Removal {
                thread_id,
                copy,
                stopping: Vec::new(),
                store: store.clone(),
            })
            .collect())
    }

    pub fn copy(&self) -> &rift::Copy {
        &self.copy
    }

    pub fn run(self, tools: &Tools) -> Result<()> {
        for stopping in self.stopping {
            let _ = stopping.join();
        }
        rift::remove(tools, &self.copy.path, &self.copy.rift_id)?;
        self.store.mark_copy_removed(&self.thread_id, now_ms());
        Ok(())
    }
}

/// The parts of the thread's row that change during a conversation (the unread flag is saved
/// on its own, see [`Thread::set_active`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowState {
    title: String,
    session_id: String,
    session_started: bool,
    status: Status,
}

/// Bookkeeping for the two repairs (see the module docs). Reset by every user message.
#[derive(Debug, Default)]
struct Recovery {
    /// The user's last message, resent once if claude must be started again to receive it.
    /// Dropped when its turn ends.
    message: Option<String>,
    /// The current process printed the "already in use" line. Held back from the transcript
    /// until it is clear whether the repair works.
    in_use_line: Option<String>,
    /// A failed write to a process that has not started its conversation yet. Held back
    /// because it is usually the same "already in use" exit seen from the other side.
    write_failure: Option<String>,
    /// The "already in use" repair has run for this message.
    resumed: bool,
    /// The "No conversation found" repair has run for this message.
    started_fresh: bool,
}

impl Thread {
    /// A new, empty thread in `project`. It is a draft: nothing is saved until its first
    /// message is sent. `base_config` says how to start claude (usually
    /// `SessionConfig::new(&project.path)`).
    pub fn new_draft(store: Store, project: &Project, base_config: SessionConfig) -> Thread {
        let now = now_ms();
        let (events_in, events) = async_channel::unbounded();
        Thread {
            id: new_id(),
            project_id: project.id,
            project_dir: project.path.clone(),
            cwd: project.path.clone(),
            place: Place::ProjectFolder,
            title: NEW_THREAD_TITLE.to_string(),
            provider: PROVIDER_CLAUDE.to_string(),
            session_id: new_id(),
            session_started: false,
            transcript: Transcript::with_cwd(project.path.clone()),
            session: None,
            session_initialized: false,
            stopping: Vec::new(),
            base_config,
            generation: 0,
            events_in,
            events,
            grants: Vec::new(),
            unread: false,
            active: false,
            stage: Stage::Draft,
            created_at: now,
            updated_at: now,
            last_activity: Instant::now(),
            turn_started: None,
            stop_after_turn: false,
            recovery: Recovery::default(),
            saved_row: None,
            store,
        }
    }

    /// A saved thread, with its transcript and grants. What the previous run of Newjen left
    /// unfinished is settled first (`Transcript::normalize_after_relaunch`) and saved. No
    /// `claude` is started.
    pub fn load(
        store: Store,
        project: &Project,
        row: ThreadRow,
        base_config: SessionConfig,
    ) -> Result<Thread> {
        let saved = store.items(&row.id)?;
        // A write that failed in an earlier run can leave a gap in the saved positions (0, 1,
        // 2, 4). Rows load in order, so from the gap on a row's place on screen is not its
        // saved position, and saving "from position N" would replace the wrong rows. Save them
        // again from there, where they are now.
        let first_gap = saved
            .iter()
            .enumerate()
            .position(|(position, item)| item.ix != position);
        let items = saved
            .into_iter()
            .map(|saved| Item::from_saved(saved.schema_version, &saved.json))
            .collect();
        let grants = store.grants(&row.id)?;
        let mut transcript = Transcript::from_saved(row.workdir.clone(), items, row.status);
        transcript.normalize_after_relaunch();
        if let Some(position) = first_gap {
            transcript.mark_changed_from(position);
        }

        let mut thread = Thread::new_draft(store, project, base_config);
        thread.id = row.id;
        thread.cwd = row.workdir;
        thread.place = row.copy.map_or(Place::ProjectFolder, Place::Copy);
        thread.title = row.title;
        thread.provider = row.provider;
        thread.session_id = row.session_id;
        thread.session_started = row.session_started;
        thread.transcript = transcript;
        thread.grants = grants;
        thread.unread = row.unread;
        thread.stage = Stage::Saved;
        thread.created_at = row.created_at;
        thread.updated_at = row.updated_at;
        thread.saved_row = Some(RowState {
            title: thread.title.clone(),
            session_id: thread.session_id.clone(),
            session_started: thread.session_started,
            status: row.status,
        });
        // Keep `updated_at`: settling the old run is not new activity.
        thread.save_with(false);
        Ok(thread)
    }

    /// Every thread of `project` that is not archived, newest first, loaded with
    /// [`Thread::load`].
    pub fn load_all(
        store: &Store,
        project: &Project,
        base_config: &SessionConfig,
    ) -> Result<Vec<Thread>> {
        store
            .threads(project.id)?
            .into_iter()
            .map(|row| Thread::load(store.clone(), project, row, base_config.clone()))
            .collect()
    }

    // -----------------------------------------------------------------------------------------
    // What the app reads
    // -----------------------------------------------------------------------------------------

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// The folder claude works in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Claude's conversation id for this thread.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// True once claude has the conversation, so the next start uses `--resume`.
    pub fn session_started(&self) -> bool {
        self.session_started
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn status(&self) -> Status {
        self.transcript.status()
    }

    /// The "Always allow" rules this thread remembers, oldest first.
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    /// Something happened while the thread was not on screen.
    pub fn is_unread(&self) -> bool {
        self.unread
    }

    /// Nothing sent yet. A thread whose first message waits for its Rift copy is not a draft
    /// any more (see [`Thread::preparing`]). See [`keeps_draft`] for switching away from one.
    pub fn is_draft(&self) -> bool {
        matches!(self.stage, Stage::Draft)
    }

    /// The project folder, or a Rift copy of its own.
    pub fn works_in(&self) -> WorksIn {
        match self.place {
            Place::ProjectFolder => WorksIn::ProjectFolder,
            Place::NewCopy | Place::Copy(_) => WorksIn::RiftCopy,
        }
    }

    /// The thread's Rift copy, once it is made.
    pub fn copy(&self) -> Option<&rift::Copy> {
        match &self.place {
            Place::Copy(copy) => Some(copy),
            Place::ProjectFolder | Place::NewCopy => None,
        }
    }

    /// The first message, while it waits for the thread's Rift copy to be made. The thread is
    /// busy then: nothing else can be sent, and no claude runs for it.
    pub fn preparing(&self) -> Option<&Preparing> {
        match &self.stage {
            Stage::Preparing(preparing) => Some(preparing),
            Stage::Draft | Stage::Saved => None,
        }
    }

    /// True if a new Rift copy must not be made now, because making one snapshots the main
    /// repo and may run `jj new` there: this thread works in the project folder and its agent
    /// is busy (a turn, an approval, a background task, any of which can be mid-edit), or this
    /// thread's own copy is being made. The app refuses with "wait for <thread> to finish".
    pub fn blocks_copy_creation(&self) -> bool {
        match (&self.stage, &self.place) {
            (Stage::Preparing(_), _) => true,
            (_, Place::ProjectFolder) => {
                self.status().is_busy() || !self.background_tasks().is_empty()
            }
            (_, Place::NewCopy | Place::Copy(_)) => false,
        }
    }

    /// True while a `claude` process runs for this thread.
    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// When the thread was created (Unix milliseconds).
    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    /// When the thread last had activity worth showing, e.g. "Done · 5m ago" (Unix
    /// milliseconds).
    pub fn updated_at(&self) -> i64 {
        self.updated_at
    }

    /// When the running turn started, for "Working · 2m". `None` when no turn runs (and after
    /// a relaunch).
    pub fn turn_started_at(&self) -> Option<Instant> {
        self.turn_started.filter(|_| self.status().is_busy())
    }

    /// What claude keeps running in the background now (a dev server, say), even between
    /// turns. Empty when no claude runs: stopping claude stops these too.
    pub fn background_tasks(&self) -> &[BackgroundTask] {
        self.transcript.background_tasks()
    }

    /// The channel this thread's events arrive on, for the whole life of the thread. Pass each
    /// event to [`Thread::apply`]. Read it from one place only: each event is delivered once,
    /// to whichever copy of the receiver asks first.
    pub fn events(&self) -> Receiver<ThreadEvent> {
        self.events.clone()
    }

    /// The `SessionConfig` the next `claude` would be started with (see [`claude_config`]).
    pub fn session_config(&self) -> SessionConfig {
        let rules: Vec<String> = self.grants.iter().map(|grant| grant.rule.clone()).collect();
        let mut config = claude_config(
            &self.base_config,
            &self.session_id,
            self.session_started,
            &rules,
        );
        config.cwd = self.cwd.clone();
        if let Place::Copy(copy) = &self.place {
            config.env.extend(copy.env());
        }
        config
    }

    /// What Continue on the "Stopped when Newjen quit" notice sends: the user's last message
    /// again if claude never started its turn (`Transcript::unconfirmed_message`), otherwise
    /// [`CONTINUE_MESSAGE`].
    pub fn continue_text(&self) -> String {
        self.transcript
            .unconfirmed_message()
            .unwrap_or(CONTINUE_MESSAGE)
            .to_string()
    }

    /// True if Continue sends the user's last message again (see [`Thread::continue_text`]).
    pub fn continue_resends(&self) -> bool {
        self.transcript.unconfirmed_message().is_some()
    }

    // -----------------------------------------------------------------------------------------
    // What the app does
    // -----------------------------------------------------------------------------------------

    /// Chooses where a draft works. Only a draft whose Rift copy is not made yet can change.
    pub fn set_works_in(&mut self, works_in: WorksIn) -> Result<()> {
        if !self.is_draft() || self.copy().is_some() {
            bail!("only a new thread can choose where it works");
        }
        self.place = match works_in {
            WorksIn::ProjectFolder => Place::ProjectFolder,
            WorksIn::RiftCopy => Place::NewCopy,
        };
        Ok(())
    }

    /// Sends a user message, starting `claude` first if none runs. The first message of a
    /// draft also names the thread (see [`title_from_message`]) and saves it.
    ///
    /// The first message of a draft that wants a Rift copy is held instead, and nothing else
    /// happens until the app runs the [`CopyJob`] it gets back and passes the result to
    /// [`Thread::finish_copy`].
    ///
    /// On an error nothing was sent, so the app can keep the text in the composer and show why
    /// (for example claude could not be started).
    pub fn send(&mut self, text: &str) -> Result<Sent> {
        if text.trim().is_empty() {
            bail!("there is nothing to send");
        }
        if self.preparing().is_some() {
            bail!("Newjen is still making this thread's Rift copy");
        }
        if self.status().is_busy() {
            bail!("Claude is still working on the last message");
        }
        if self.place == Place::NewCopy {
            self.title = title_from_message(text);
            self.stage = Stage::Preparing(Preparing {
                message: text.to_string(),
                started_at: Instant::now(),
                last_line: None,
            });
            self.last_activity = Instant::now();
            return Ok(Sent::MakingCopy(CopyJob {
                project_dir: self.project_dir.clone(),
                name: copy_name(&self.id),
            }));
        }
        self.deliver(text)?;
        Ok(Sent::Started)
    }

    /// Records the latest line Rift or its hooks printed while the copy is made.
    pub fn note_copy_line(&mut self, line: &str) {
        if let Stage::Preparing(preparing) = &mut self.stage
            && !line.trim().is_empty()
        {
            preparing.last_line = Some(line.to_string());
        }
    }

    /// Takes what the [`CopyJob`] from [`Thread::send`] returned. With the copy, the thread
    /// moves into it (any `claude` a Try again left is stopped first) and the held message is
    /// sent, as [`Thread::send`] sends it. Otherwise, or if that send fails, the thread is a
    /// draft again and the message comes back for the composer. Only the first call after a
    /// send does anything: later ones fail and send nothing.
    pub fn finish_copy(&mut self, made: Result<rift::Copy, CopyError>) -> Result<(), NotSent> {
        let preparing = match std::mem::replace(&mut self.stage, Stage::Draft) {
            Stage::Preparing(preparing) => preparing,
            stage => {
                self.stage = stage;
                return Err(NotSent {
                    message: String::new(),
                    error: anyhow!("this thread was not waiting for a Rift copy"),
                });
            }
        };
        let message = preparing.message;
        let sent = made.map_err(anyhow::Error::from).and_then(|copy| {
            self.end_session();
            self.cwd = copy.workdir.clone();
            self.transcript.set_cwd(copy.workdir.clone());
            self.place = Place::Copy(copy);
            self.deliver(&message)
        });
        sent.map_err(|error| {
            self.title = NEW_THREAD_TITLE.to_string();
            NotSent { message, error }
        })
    }

    /// The part of [`Thread::send`] that writes the message to claude.
    fn deliver(&mut self, text: &str) -> Result<()> {
        if self.session.is_none() {
            self.spawn()?;
        }
        let session = self.session.as_ref().expect("started above");
        if let Err(error) = session.send_prompt(text) {
            // This claude can no longer receive anything; the next message starts a new one.
            self.end_session();
            self.save_with(false);
            return Err(error);
        }
        if self.is_draft() {
            self.stage = Stage::Saved;
            self.title = title_from_message(text);
        }
        self.transcript.push_user(text);
        self.recovery = Recovery {
            message: Some(text.to_string()),
            ..Recovery::default()
        };
        self.turn_started = Some(Instant::now());
        self.last_activity = Instant::now();
        self.save();
        Ok(())
    }

    /// Answers the approval on screen. `request_id` is the request the button belonged to; a
    /// stale click (that request was already answered) does nothing.
    ///
    /// "Always allow" ([`PermissionAnswer::AllowForSession`]) also saves the rules it makes
    /// claude remember (`protocol::session_rules`), so every later `claude` for this thread
    /// gets them too. When it remembers nothing it is allow once, and nothing is saved.
    pub fn answer(&mut self, request_id: Option<&str>, answer: PermissionAnswer) -> Result<()> {
        let Some(request) = self.transcript.pending_approval().cloned() else {
            return Ok(());
        };
        if request_id.is_some_and(|id| id != request.request_id) {
            return Ok(());
        }
        // Resolve first: a second click or key press now finds nothing to answer.
        self.transcript
            .resolve_approval(&request.request_id, answer);
        self.last_activity = Instant::now();
        let sent = match &self.session {
            Some(session) => session.answer(&request, answer),
            None => Err(anyhow!("the claude session has ended")),
        };
        if sent.is_ok() && answer == PermissionAnswer::AllowForSession {
            let now = now_ms();
            for rule in session_rules(&request) {
                self.add_grant(Grant {
                    rule: rule.cli_rule(),
                    label: rule.label(),
                    created_at: now,
                });
            }
        }
        self.save();
        sent
    }

    /// Stops the turn (⌘.): denies a pending approval and ends the turn, or asks claude to
    /// stop. Does nothing when no turn runs.
    ///
    /// While claude is still starting (no `system/init` yet), it is stopped and the turn ends
    /// here at once. Asking it would not be enough: if it then failed a start-up check, a
    /// repair would start claude again and resend the message, running the very turn the user
    /// stopped.
    pub fn stop(&mut self) -> Result<()> {
        if self.transcript.pending_approval().is_some() {
            return self.answer(None, PermissionAnswer::DenyAndStop);
        }
        if !self.status().is_busy() {
            return Ok(());
        }
        self.last_activity = Instant::now();
        if !self.session_initialized {
            self.end_turn_as_stopped();
            return Ok(());
        }
        match &self.session {
            Some(session) => session.interrupt(),
            None => Ok(()),
        }
    }

    /// "Try again" on an error: stops any running `claude` and starts a fresh one for the
    /// same conversation. Sends nothing. If claude no longer has the conversation, the thread
    /// switches to a new one, so the next message works (see [`Thread::apply`]).
    pub fn restart(&mut self) -> Result<()> {
        if self.session.is_some() {
            // The old process's own exit event will be ignored (its generation is over), so
            // end its turn here as a requested stop would.
            self.transcript.apply(&SessionEvent::Exited {
                code: None,
                requested: true,
            });
        }
        self.end_session();
        self.recovery = Recovery::default();
        self.last_activity = Instant::now();
        let started = self.spawn();
        self.save();
        started
    }

    /// Forgets a grant. claude cannot drop a rule it was given, so the process is stopped, with
    /// any command it runs in the background: now if it is idle, or when the running turn ends.
    /// The next message starts a new one with the remaining rules (`--resume`, so the
    /// conversation goes on).
    pub fn revoke(&mut self, rule: &str) {
        let before = self.grants.len();
        self.grants.retain(|grant| grant.rule != rule);
        if self.grants.len() == before {
            return;
        }
        if self.is_saved() {
            self.store.remove_grant(&self.id, rule);
        }
        if self.session.is_some() {
            if self.status().is_busy() {
                self.stop_after_turn = true;
            } else {
                self.end_session();
                self.save();
            }
        }
    }

    /// Marks this thread as the one on screen (or not). Becoming active clears the unread
    /// flag (see [`becomes_unread`] for when it is set).
    pub fn set_active(&mut self, active: bool) {
        self.active = active;
        if active && self.unread {
            self.set_unread(false);
        }
    }

    /// Stops `claude` if it has been idle for [`IDLE_TIMEOUT`] at `now`: running, no turn, no
    /// approval waiting, no background task. The thread's status stays as it was, and the next
    /// message starts claude again. Returns true if it stopped it. The app calls this every
    /// minute; tests pass a `now` in the future.
    pub fn idle_check(&mut self, now: Instant) -> bool {
        // Also a good moment to write again what a failed write lost, in case no checkpoint
        // comes soon (say the lost write was the turn's last). Between turns only: nothing is
        // saved for each streamed word.
        if !self.status().is_busy() {
            self.save_with(false);
        }
        let idle = self.session.is_some()
            && !self.status().is_busy()
            && self.transcript.pending_approval().is_none()
            && self.background_tasks().is_empty()
            && now.saturating_duration_since(self.last_activity) >= IDLE_TIMEOUT;
        if idle {
            self.end_session();
        }
        idle
    }

    /// Hides the thread from the list and stops its `claude`. Its rows stay in the store. A
    /// thread with a Rift copy hands back the [`Removal`] that deletes it.
    ///
    /// A first message held for a Rift copy is dropped, so a later [`Thread::finish_copy`]
    /// sends nothing; the copy being made is left behind, so the app should not archive a
    /// thread while it is [`Thread::preparing`].
    pub fn archive(&mut self) -> Option<Removal> {
        if self.session.is_some() {
            self.transcript.apply(&SessionEvent::Exited {
                code: None,
                requested: true,
            });
        }
        self.end_session();
        self.save();
        match self.stage {
            Stage::Saved => self.store.archive(&self.id, now_ms()),
            Stage::Preparing(_) => self.stage = Stage::Draft,
            Stage::Draft => {}
        }
        let copy = self.copy()?.clone();
        Some(Removal {
            thread_id: self.id.clone(),
            copy,
            stopping: std::mem::take(&mut self.stopping),
            store: self.store.clone(),
        })
    }

    /// At quit: stops `claude`. If a turn was running, an approval waiting or a background task
    /// running, the thread is first settled exactly as a relaunch would settle it (status
    /// Stopped, the "Stopped when Newjen quit" notice) and saved, waiting at most
    /// [`QUIT_FLUSH_TIMEOUT`] for the disk.
    /// If Newjen is killed instead, the relaunch does the same from the last checkpoint.
    ///
    /// For one thread. To quit several, call [`Thread::begin_quit`] on each and then
    /// [`finish_quit`] once, so their processes stop together rather than one after another.
    pub fn shutdown_for_quit(&mut self) {
        let quitting = self.begin_quit();
        finish_quit(&self.store, [quitting]);
    }

    /// The first half of [`Thread::shutdown_for_quit`]: settles and saves the thread and
    /// starts stopping its `claude`, without waiting for either. Pass what it returns for
    /// every thread to one [`finish_quit`] call.
    pub fn begin_quit(&mut self) -> Quitting {
        if self.status().is_busy() || !self.background_tasks().is_empty() {
            self.transcript.normalize_after_relaunch();
        }
        // Queue the writes before stopping claude, so the disk works while claude stops.
        self.save();
        self.end_session();
        Quitting {
            stopping: std::mem::take(&mut self.stopping),
        }
    }

    /// Handles one event from the thread's channel ([`Thread::events`]): updates the
    /// transcript, repairs a failed start (see the module docs), sets the unread flag and
    /// saves at checkpoints. Events from a process the thread already let go of are ignored.
    pub fn apply(&mut self, event: ThreadEvent) {
        if event.generation != self.generation {
            return;
        }
        let event = event.event;
        self.last_activity = Instant::now();

        match &event {
            // Hold the "already in use" line back until the exit shows whether the repair
            // works; it only becomes part of an error if it doesn't.
            SessionEvent::Stderr(line)
                if !self.session_started && is_session_in_use(line, &self.session_id) =>
            {
                self.recovery.in_use_line = Some(line.clone());
                return;
            }
            SessionEvent::WriteFailed(detail) if !self.session_started => {
                self.recovery.write_failure = Some(detail.clone());
                return;
            }
            SessionEvent::Exited {
                requested: false, ..
            } if self.recovery.in_use_line.is_some() && !self.recovery.resumed => {
                self.recovery.resumed = true;
                self.recovery.in_use_line = None;
                self.recovery.write_failure = None;
                self.session_started = true;
                self.start_again();
                return;
            }
            // claude checks the conversation as it starts, before it reads any message, so this
            // also comes after "Try again" (which sends nothing). Either way the conversation is
            // gone, so switch to a new one now; otherwise every later start would fail the same
            // way.
            SessionEvent::Line(Incoming::Result(result))
                if self.session_started
                    && !self.session_initialized
                    && !self.recovery.started_fresh
                    && is_missing_conversation(result, &self.session_id) =>
            {
                self.recovery.started_fresh = true;
                self.session_id = new_id();
                self.session_started = false;
                match self.recovery.message.as_deref() {
                    Some(message) if message != CONTINUE_MESSAGE => {
                        self.transcript.insert_fresh_start_before_last_message();
                        self.start_again();
                    }
                    // "Continue where you left off." means nothing to a new conversation, and
                    // "Try again" has nothing to resend: the turn ends, and the user's next
                    // message starts the new conversation.
                    _ => {
                        self.end_turn_as_stopped();
                        self.transcript.push_fresh_start();
                        self.save();
                    }
                }
                return;
            }
            SessionEvent::Line(Incoming::System(message)) if message.subtype == "init" => {
                self.session_started = true;
                self.session_initialized = true;
                // claude normally reports the id it was given; if not, its own id is the one
                // to resume.
                if let Some(session_id) = message.session_id.as_ref().filter(|id| !id.is_empty()) {
                    self.session_id = session_id.clone();
                }
            }
            _ => {}
        }

        // Read before the held lines go in: one of them can end the turn, and that turn end
        // must still clear the message a repair would resend.
        let was_busy = self.status().is_busy();
        // Anything held back above is real now (the repair did not apply), so it goes in
        // first, in the order it happened.
        if let Some(line) = self.recovery.in_use_line.take() {
            self.transcript.apply(&SessionEvent::Stderr(line));
        }
        if let Some(detail) = self.recovery.write_failure.take() {
            self.transcript.apply(&SessionEvent::WriteFailed(detail));
        }

        self.transcript.apply(&event);
        let turn_ended = was_busy && !self.status().is_busy();
        // claude started a turn by itself (see `Transcript::apply`).
        if !was_busy && self.status().is_busy() {
            self.turn_started = Some(Instant::now());
        }
        let approval_asked = matches!(event, SessionEvent::Line(Incoming::PermissionRequest(_)));
        if approval_asked && becomes_unread(self.active, false, true) {
            self.set_unread(true);
        }

        match &event {
            // The process is gone (so stopping it returns at once); the next message starts a
            // new one.
            SessionEvent::Exited { .. } => {
                self.session = None;
                self.session_initialized = false;
                self.generation += 1;
            }
            // This process can no longer be talked to; let it go.
            SessionEvent::WriteFailed(_) | SessionEvent::InterruptTimedOut => self.end_session(),
            _ => {}
        }
        if turn_ended {
            self.after_turn_ended();
        }
        if is_checkpoint(&event) {
            self.save();
        }
    }

    // -----------------------------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------------------------

    /// Starts `claude` with [`Thread::session_config`] and forwards its events to the thread's
    /// channel under a new generation.
    fn spawn(&mut self) -> Result<()> {
        // Never two claudes on one conversation: wait until any being stopped are gone
        // (usually they already are).
        for stopping in self.stopping.drain(..) {
            let _ = stopping.join();
        }
        let (session, events) = Session::spawn(self.session_config())?;
        self.generation += 1;
        let generation = self.generation;
        let sink = self.events_in.clone();
        thread::spawn(move || {
            // Ends when the process's channel closes (after `Exited`), or nobody listens.
            while let Ok(event) = events.recv_blocking() {
                if sink
                    .send_blocking(ThreadEvent { generation, event })
                    .is_err()
                {
                    break;
                }
            }
        });
        self.session = Some(session);
        self.session_initialized = false;
        self.recovery.in_use_line = None;
        self.recovery.write_failure = None;
        Ok(())
    }

    /// Lets the current `claude` go: later events from it are ignored, and it is stopped on a
    /// background thread (see the module docs).
    fn end_session(&mut self) {
        self.generation += 1;
        self.stop_after_turn = false;
        self.session_initialized = false;
        // Its exit will be ignored, so the background tasks it takes with it are settled here.
        self.transcript.stop_background_tasks();
        if let Some(session) = self.session.take() {
            self.stopping.retain(|stopping| !stopping.is_finished());
            self.stopping
                .push(thread::spawn(move || session.shutdown()));
        }
    }

    /// One of the two repairs: start `claude` again (with the conversation flags as they are
    /// now) and resend the user's message. If that fails too, the turn ends with an error.
    fn start_again(&mut self) {
        self.end_session();
        let failure = match self.spawn() {
            Ok(()) => match (&self.recovery.message, &self.session) {
                (Some(message), Some(session)) => session.send_prompt(message).err().map(|error| {
                    (
                        "Could not reach Claude",
                        format!(
                            "Newjen could not send the message to the claude process: {error:#}"
                        ),
                    )
                }),
                _ => None,
            },
            Err(error) => Some(("Could not start Claude Code", format!("{error:#}"))),
        };
        if let Some((title, detail)) = failure {
            let was_busy = self.status().is_busy();
            // A claude that could not take the message is no use; the next message starts one.
            self.end_session();
            self.transcript.fail(title, detail);
            if was_busy {
                self.after_turn_ended();
            }
        }
        self.save();
    }

    /// Ends the running turn as stopped and lets `claude` go, as "Try again" does: the
    /// process's own exit will be ignored, so its turn is ended here. Saves.
    fn end_turn_as_stopped(&mut self) {
        let was_busy = self.status().is_busy();
        self.transcript.apply(&SessionEvent::Exited {
            code: None,
            requested: true,
        });
        self.end_session();
        // Whatever a repair was waiting for no longer matters.
        self.recovery = Recovery::default();
        if was_busy && !self.status().is_busy() {
            self.after_turn_ended();
        }
        self.save();
    }

    /// What happens whenever a turn ends, however it ended: the unread rule, the turn's clock,
    /// the message a repair would resend, and a stop that was waiting for the turn to end (see
    /// [`Thread::revoke`]).
    fn after_turn_ended(&mut self) {
        if becomes_unread(self.active, true, false) {
            self.set_unread(true);
        }
        self.turn_started = None;
        self.recovery.message = None;
        if self.stop_after_turn {
            self.end_session();
        }
    }

    fn is_saved(&self) -> bool {
        matches!(self.stage, Stage::Saved)
    }

    fn add_grant(&mut self, grant: Grant) {
        if self.grants.iter().any(|known| known.rule == grant.rule) {
            return;
        }
        if self.is_saved() {
            self.store.add_grant(&self.id, grant.clone());
        }
        self.grants.push(grant);
    }

    fn set_unread(&mut self, unread: bool) {
        if self.unread == unread {
            return;
        }
        self.unread = unread;
        if self.is_saved() {
            self.store.set_unread(&self.id, unread);
        }
    }

    /// Saves what changed: the thread's row and the transcript rows from the first changed one.
    /// Returns true if anything was queued. A draft is never saved.
    fn save(&mut self) -> bool {
        self.save_with(true)
    }

    /// [`Thread::save`]; `touch_time` says whether this counts as activity (`updated_at`).
    fn save_with(&mut self, touch_time: bool) -> bool {
        if !self.is_saved() {
            return false;
        }
        // First, whatever a failed write lost is marked to be written again.
        let mut queued = false;
        if let Some(lost) = self.store.take_unsaved(&self.id) {
            if lost.row {
                self.saved_row = None;
            }
            if let Some(from) = lost.items_from {
                self.transcript.mark_changed_from(from);
            }
            if lost.grants {
                self.store.replace_grants(&self.id, self.grants.clone());
                queued = true;
            }
        }
        let state = RowState {
            title: self.title.clone(),
            session_id: self.session_id.clone(),
            session_started: self.session_started,
            status: self.status(),
        };
        let items_from = self.transcript.dirty_from();
        if items_from.is_none() && self.saved_row.as_ref() == Some(&state) {
            return queued;
        }
        if touch_time {
            self.updated_at = now_ms();
        }
        match items_from {
            // The row and its changed items in one transaction (the row first: on a new
            // thread the items refer to it).
            Some(from) => {
                let items = self.transcript.items()[from..]
                    .iter()
                    .map(Item::to_saved)
                    .collect();
                self.store.save_checkpoint(self.row(), from, items);
                self.transcript.mark_saved();
            }
            None => self.store.save_thread(self.row()),
        }
        self.saved_row = Some(state);
        true
    }

    fn row(&self) -> ThreadRow {
        ThreadRow {
            id: self.id.clone(),
            project_id: self.project_id,
            workdir: self.cwd.clone(),
            title: self.title.clone(),
            provider: self.provider.clone(),
            session_id: self.session_id.clone(),
            session_started: self.session_started,
            status: self.status(),
            unread: self.unread,
            created_at: self.created_at,
            updated_at: self.updated_at,
            archived_at: None,
            copy: self.copy().cloned(),
        }
    }
}

/// What [`Thread::begin_quit`] leaves for [`finish_quit`]: the thread's `claude` processes
/// that are still stopping.
#[must_use = "pass it to `finish_quit`, which waits for the processes and the disk"]
pub struct Quitting {
    stopping: Vec<JoinHandle<()>>,
}

/// The second half of quitting (see [`Thread::begin_quit`]): waits at most
/// [`QUIT_FLUSH_TIMEOUT`] for `store` to write what is queued, and for the `claude` processes
/// to stop. They all stop at the same time, so this takes about as long as the slowest one
/// (about 150 ms at most), however many threads there are.
pub fn finish_quit(store: &Store, quitting: impl IntoIterator<Item = Quitting>) {
    store.flush(QUIT_FLUSH_TIMEOUT);
    for stopping in quitting.into_iter().flat_map(|quitting| quitting.stopping) {
        let _ = stopping.join();
    }
}

/// The config to start `claude` with for a conversation: `--session-id <id>` until the
/// conversation has started, `--resume <id>` after, and one `--allowedTools <rule>` pair per
/// granted rule. Everything else comes from `base`.
pub fn claude_config(
    base: &SessionConfig,
    session_id: &str,
    session_started: bool,
    allowed_rules: &[String],
) -> SessionConfig {
    let mut config = base.clone();
    config.conversation = Some(if session_started {
        Conversation::Resume(session_id.to_string())
    } else {
        Conversation::New(session_id.to_string())
    });
    config.allowed_tools = allowed_rules.to_vec();
    config
}

/// A thread's title from its first message: the first line with text, runs of whitespace made
/// one space, cut after [`TITLE_MAX_CHARS`] characters with "…". [`NEW_THREAD_TITLE`] if there
/// is no text. No model is asked (that would spend the user's usage without them knowing).
pub fn title_from_message(text: &str) -> String {
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let collapsed = first_line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return NEW_THREAD_TITLE.to_string();
    }
    if collapsed.chars().count() <= TITLE_MAX_CHARS {
        return collapsed;
    }
    // Characters, not bytes: cutting a byte string could split a letter or an emoji.
    let cut: String = collapsed.chars().take(TITLE_MAX_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// The unread rule: a thread becomes unread when its turn ends or it asks for an approval
/// while it is not the thread on screen. (Becoming the thread on screen clears it.)
pub fn becomes_unread(active: bool, turn_ended: bool, approval_asked: bool) -> bool {
    !active && (turn_ended || approval_asked)
}

/// The draft rule, for when the user switches away from a draft: keep it if the composer has
/// text in it, drop it if not, so empty threads never pile up.
pub fn keeps_draft(composer_text: &str) -> bool {
    !composer_text.trim().is_empty()
}

/// Events after which the thread is saved. Streamed words (`stream_event`) and stderr lines
/// are not: the reply is saved once its block is finished.
fn is_checkpoint(event: &SessionEvent) -> bool {
    match event {
        SessionEvent::Line(incoming) => matches!(
            incoming,
            Incoming::System(_)
                | Incoming::Assistant(_)
                | Incoming::User(_)
                | Incoming::PermissionRequest(_)
                | Incoming::ControlCancelRequest { .. }
                | Incoming::Result(_)
        ),
        SessionEvent::Stderr(_) => false,
        SessionEvent::WriteFailed(_)
        | SessionEvent::InterruptTimedOut
        | SessionEvent::Exited { .. } => true,
    }
}

/// The name of a thread's Rift copy: `newjen-` and the start of the thread's id.
fn copy_name(thread_id: &str) -> String {
    let short: String = thread_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("newjen-{short}")
}

/// A new random id (a v4 UUID), for threads and Claude conversations.
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by the TurnStart model: a write failure held back during start-up, flushed by an
    /// unrelated stderr line, ended the turn without clearing the message, so the "already in
    /// use" repair resent it after the turn had failed.
    #[test]
    fn a_turn_ended_by_a_held_write_failure_is_never_resent() {
        let home = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(home.path()).unwrap();
        let project = store.open_project(project_dir.path()).unwrap();
        let received = project_dir.path().join("stdin.log");
        let script = project_dir.path().join("claude");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat >> '{}'\n", received.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut config = SessionConfig::new(project_dir.path());
        config.program = script;

        let mut thread = Thread::new_draft(store, &project, config);
        let _ = thread.send("only once").unwrap();
        let delivered = || {
            std::fs::read_to_string(&received)
                .unwrap_or_default()
                .matches("only once")
                .count()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while delivered() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let event = |thread: &Thread, event| ThreadEvent {
            generation: thread.generation,
            event,
        };
        let in_use = format!("Error: Session ID {} is already in use.", thread.session_id);
        for e in [
            SessionEvent::WriteFailed("Broken pipe".to_string()),
            SessionEvent::Stderr("warning: slow start".to_string()),
            SessionEvent::Stderr(in_use),
            SessionEvent::Exited {
                code: Some(1),
                requested: false,
            },
        ] {
            let e = event(&thread, e);
            thread.apply(e);
        }

        assert!(!thread.status().is_busy(), "{:?}", thread.status());
        assert_eq!(thread.recovery.message, None);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(delivered(), 1, "the message reached claude again");
        thread.end_session();
    }

    #[test]
    fn title_is_the_first_line_collapsed_and_cut() {
        assert_eq!(title_from_message("Fix the login bug"), "Fix the login bug");
        assert_eq!(
            title_from_message("\n\n  Fix   the\tlogin  bug  \nmore detail"),
            "Fix the login bug"
        );
        assert_eq!(title_from_message("   \n  "), NEW_THREAD_TITLE);
        let long = "a".repeat(49) + " bcdef";
        let title = title_from_message(&long);
        assert_eq!(title, format!("{}…", "a".repeat(49)));
        // Exactly the limit is not cut.
        let exact = "x".repeat(TITLE_MAX_CHARS);
        assert_eq!(title_from_message(&exact), exact);
        // Emoji and accents are cut by character, never in the middle of one.
        let emoji = "🦀".repeat(60);
        assert_eq!(
            title_from_message(&emoji),
            format!("{}…", "🦀".repeat(TITLE_MAX_CHARS))
        );
    }

    #[test]
    fn unread_only_when_not_on_screen_and_something_happened() {
        assert!(becomes_unread(false, true, false));
        assert!(becomes_unread(false, false, true));
        assert!(!becomes_unread(false, false, false));
        assert!(!becomes_unread(true, true, false));
        assert!(!becomes_unread(true, false, true));
    }

    #[test]
    fn streamed_words_and_stderr_are_not_checkpoints() {
        let delta = Incoming::parse(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,
                "delta":{"type":"text_delta","text":"Hel"}}}"#,
        );
        assert!(!is_checkpoint(&SessionEvent::Line(delta)));
        assert!(!is_checkpoint(&SessionEvent::Stderr("warning".to_string())));
        let finished = Incoming::parse(
            r#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"Hi"}]}}"#,
        );
        assert!(is_checkpoint(&SessionEvent::Line(finished)));
        let result = Incoming::parse(r#"{"type":"result","subtype":"success","is_error":false}"#);
        assert!(is_checkpoint(&SessionEvent::Line(result)));
        assert!(is_checkpoint(&SessionEvent::Exited {
            code: Some(0),
            requested: false
        }));
    }

    #[test]
    fn only_claudes_own_in_use_message_for_this_id_counts() {
        let id = "3f1e1c2a-0000-4000-8000-0000000000ab";
        assert!(is_session_in_use(
            &format!("Error: Session ID {id} is already in use."),
            id
        ));
        assert!(!is_session_in_use(
            "Error: Session ID 11111111-0000-4000-8000-000000000000 is already in use.",
            id
        ));
        assert!(!is_session_in_use(
            "Error: listen EADDRINUSE: address is already in use :::3000",
            id
        ));
    }

    #[test]
    fn only_a_missing_conversation_error_for_this_id_counts() {
        let id = "3f1e1c2a-0000-4000-8000-0000000000ab";
        let result = |is_error: bool, error: &str| ResultMessage {
            is_error,
            errors: Some(vec![serde_json::json!(error)]),
            ..ResultMessage::default()
        };
        let named = |id: &str| format!("No conversation found with session ID: {id}");
        assert!(is_missing_conversation(&result(true, &named(id)), id));
        assert!(is_missing_conversation(
            &result(true, "No conversation found"),
            id
        ));
        assert!(!is_missing_conversation(
            &result(true, &named("11111111-0000-4000-8000-000000000000")),
            id
        ));
        assert!(!is_missing_conversation(&result(false, &named(id)), id));
    }

    #[test]
    fn a_draft_is_kept_only_with_text_in_the_composer() {
        assert!(keeps_draft("half a thought"));
        assert!(!keeps_draft(""));
        assert!(!keeps_draft("  \n\t "));
    }

    #[test]
    fn config_starts_then_resumes_and_passes_each_rule() {
        let base = SessionConfig::new("/tmp/project");
        let rules = vec![
            "Bash(touch one.txt)".to_string(),
            "Bash(echo a,b)".to_string(),
            r#"Bash(echo "hi there")"#.to_string(),
            "Bash(git add:*)".to_string(),
            "mcp__pencil__get_style".to_string(),
        ];
        let start = claude_config(&base, "sid", false, &[]).args();
        let tail: Vec<&str> = start[start.len() - 2..]
            .iter()
            .map(String::as_str)
            .collect();
        assert_eq!(tail, ["--session-id", "sid"]);
        assert!(!start.contains(&"--allowedTools".to_string()));

        let resume = claude_config(&base, "sid", true, &rules).args();
        let at = resume.iter().position(|arg| arg == "--resume").unwrap();
        let tail: Vec<&str> = resume[at..].iter().map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "--resume",
                "sid",
                "--allowedTools",
                "Bash(touch one.txt)",
                "--allowedTools",
                "Bash(echo a,b)",
                "--allowedTools",
                r#"Bash(echo "hi there")"#,
                "--allowedTools",
                "Bash(git add:*)",
                "--allowedTools",
                "mcp__pencil__get_style",
            ]
        );
        assert!(!resume.contains(&"--session-id".to_string()));
    }
}
