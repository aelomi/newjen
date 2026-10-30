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
//! - "No conversation found" (Claude no longer has it): start a new one with a new id, after
//!   a quiet row saying so. The earlier messages stay on screen. This also happens after
//!   "Try again", which has no message to resend: the next message starts the new one.
//!
//! A repair applies only while claude is starting (before its first `system/init`); a Stop
//! pressed in that window ends the turn at once instead (see [`Thread::stop`]).
//!
//! **Saving.** A thread is a *draft* until its first message is sent; drafts are never saved.
//! After that, the row and the changed transcript rows are saved at checkpoints only (a
//! message sent, a reply or tool call finished, an approval asked or answered, the turn's end,
//! an error), never for each streamed word. If a write fails, the store remembers what it
//! lost, and the thread's next save writes that again.
//!
//! **Stopping claude** (idle, revoke, archive, quit) happens on a background thread, because
//! it can take up to about 150 ms and the app must not wait for it. A new `claude` for the
//! thread is only started once the old one is gone.
//!
//! **Subscription safety.** Nothing here starts a turn on its own. A turn starts only from
//! [`Thread::send`]; the two repairs above resend the user's own message once.

use std::path::{Path, PathBuf};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use async_channel::{Receiver, Sender};

use crate::protocol::{Incoming, PermissionAnswer, session_rules};
use crate::session::{Conversation, Session, SessionConfig, SessionEvent};
use crate::store::{Grant, Project, QUIT_FLUSH_TIMEOUT, Store, ThreadRow, now_ms};
use crate::transcript::{Item, Status, Transcript};

/// The provider a thread runs on. Only Claude for now (see `docs/PLAN.md`).
pub const PROVIDER_CLAUDE: &str = "claude";

/// A thread's title before its first message.
pub const NEW_THREAD_TITLE: &str = "New thread";

/// Titles longer than this many characters are cut, with "…" after them.
pub const TITLE_MAX_CHARS: usize = 50;

/// A thread's `claude` is stopped after this long without activity, but never mid-turn or
/// while an approval waits. The next message starts it again (`--resume`). T3 Code and Codex
/// also use 30 minutes.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What the Continue button on a "Stopped when Newjen quit" notice sends, as a visible message.
pub const CONTINUE_MESSAGE: &str = "Continue where you left off.";

/// True if `line` is what claude prints on stderr (and then exits 1) when `--session-id`
/// names a conversation that already exists: `Error: Session ID <id> is already in use.`
/// Only this exact message for this id counts, so another error that happens to say "already
/// in use" (a busy port, say) is shown as it is.
fn is_session_in_use(line: &str, session_id: &str) -> bool {
    line.contains(&format!("Session ID {session_id} is already in use"))
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
    /// The folder claude works in.
    cwd: PathBuf,
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
    /// Not saved yet: no message has been sent.
    draft: bool,
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
            cwd: project.path.clone(),
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
            draft: true,
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
        thread.title = row.title;
        thread.provider = row.provider;
        thread.session_id = row.session_id;
        thread.session_started = row.session_started;
        thread.transcript = transcript;
        thread.grants = grants;
        thread.unread = row.unread;
        thread.draft = false;
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

    /// Not saved yet (no message sent). See [`keeps_draft`] for switching away from one.
    pub fn is_draft(&self) -> bool {
        self.draft
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
        config
    }

    // -----------------------------------------------------------------------------------------
    // What the app does
    // -----------------------------------------------------------------------------------------

    /// Sends a user message, starting `claude` first if none runs. The first message of a
    /// draft also names the thread (see [`title_from_message`]) and saves it.
    ///
    /// On an error nothing was sent and nothing changed, so the app can keep the text in the
    /// composer and show why (for example claude could not be started).
    pub fn send(&mut self, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            bail!("there is nothing to send");
        }
        if self.status().is_busy() {
            bail!("Claude is still working on the last message");
        }
        if self.session.is_none() {
            self.spawn()?;
        }
        let session = self.session.as_ref().expect("started above");
        if let Err(error) = session.send_prompt(text) {
            // This claude can no longer receive anything; the next message starts a new one.
            self.end_session();
            return Err(error);
        }
        if self.draft {
            self.draft = false;
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
        if answer == PermissionAnswer::AllowForSession {
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

    /// Forgets a grant. claude cannot drop a rule it was given, so the process is stopped: now
    /// if it is idle, or when the running turn ends. The next message starts a new one with
    /// the remaining rules (`--resume`, so the conversation goes on).
    pub fn revoke(&mut self, rule: &str) {
        let before = self.grants.len();
        self.grants.retain(|grant| grant.rule != rule);
        if self.grants.len() == before {
            return;
        }
        if !self.draft {
            self.store.remove_grant(&self.id, rule);
        }
        if self.session.is_some() {
            if self.status().is_busy() {
                self.stop_after_turn = true;
            } else {
                self.end_session();
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
    /// approval waiting. The thread's status stays as it was, and the next message starts
    /// claude again. Returns true if it stopped it. The app calls this every minute; tests pass
    /// a `now` in the future.
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
            && now.saturating_duration_since(self.last_activity) >= IDLE_TIMEOUT;
        if idle {
            self.end_session();
        }
        idle
    }

    /// Hides the thread from the list and stops its `claude`. Its rows stay in the store.
    pub fn archive(&mut self) {
        self.end_session();
        if !self.draft {
            self.store.archive(&self.id, now_ms());
        }
    }

    /// At quit: stops `claude`. If a turn was running or an approval waiting, the thread is
    /// first settled exactly as a relaunch would settle it (status Stopped, the "Stopped when
    /// Newjen quit" notice) and saved, waiting at most [`QUIT_FLUSH_TIMEOUT`] for the disk.
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
        if self.status().is_busy() {
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
                if result.is_missing_conversation()
                    && self.session_started
                    && !self.recovery.started_fresh =>
            {
                self.recovery.started_fresh = true;
                self.session_id = new_id();
                self.session_started = false;
                match self.recovery.message.as_deref() {
                    // "Continue where you left off." means nothing to a new conversation, so
                    // it is not resent: the turn ends, and the user's next message starts
                    // the new conversation.
                    Some(CONTINUE_MESSAGE) => {
                        self.end_turn_as_stopped();
                        self.transcript.push_fresh_start();
                        self.save();
                    }
                    Some(_) => {
                        self.transcript.push_fresh_start();
                        self.start_again();
                    }
                    None => {
                        // Nothing to resend: the next message starts the new conversation.
                        self.transcript.push_fresh_start();
                        self.end_session();
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

        // Anything held back above is real now (the repair did not apply), so it goes in
        // first, in the order it happened.
        if let Some(line) = self.recovery.in_use_line.take() {
            self.transcript.apply(&SessionEvent::Stderr(line));
        }
        if let Some(detail) = self.recovery.write_failure.take() {
            self.transcript.apply(&SessionEvent::WriteFailed(detail));
        }

        let was_busy = self.status().is_busy();
        self.transcript.apply(&event);
        let turn_ended = was_busy && !self.status().is_busy();
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
    /// and a stop that was waiting for the turn to end (see [`Thread::revoke`]).
    fn after_turn_ended(&mut self) {
        if becomes_unread(self.active, true, false) {
            self.set_unread(true);
        }
        self.turn_started = None;
        if self.stop_after_turn {
            self.end_session();
        }
    }

    fn add_grant(&mut self, grant: Grant) {
        if self.grants.iter().any(|known| known.rule == grant.rule) {
            return;
        }
        if !self.draft {
            self.store.add_grant(&self.id, grant.clone());
        }
        self.grants.push(grant);
    }

    fn set_unread(&mut self, unread: bool) {
        if self.unread == unread {
            return;
        }
        self.unread = unread;
        if !self.draft {
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
        if self.draft {
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

/// A new random id (a v4 UUID), for threads and Claude conversations.
fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
