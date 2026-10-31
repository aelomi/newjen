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
//! **Saving.** A thread is a *draft* until its first message is sent; drafts have no thread row.
//! A pending copy's message and destination are durable before its job runs.
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

use crate::agent::NoteDelivery;
use crate::landing::{self, LandingMode};
#[cfg(test)]
use crate::protocol::{Incoming, ResultMessage};
#[cfg(test)]
use crate::providers::claude::controller::{is_missing_conversation, is_session_in_use};
use crate::rift::{self, CopyError, Tools};
use crate::session::{Session, SessionConfig, SessionEvent};
use crate::store::{
    Grant, PendingCopy, PendingIntent, PendingState, Project, QUIT_FLUSH_TIMEOUT, Store, ThreadRow,
    now_ms,
};
use crate::transcript::{Item, Status, Transcript};

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
    landing_mode: LandingMode,
    review_base: Option<String>,
    landing_resolved: bool,
    snapshot: SnapshotState,
    title: String,
    provider: String,
    /// Claude's own conversation id (`--session-id` / `--resume`).
    session_id: String,
    /// True once claude has reported the conversation exists, so it must be resumed.
    session_started: bool,
    transcript: Transcript,
    controller: crate::session::Controller,
    limit: Option<crate::agent::RateLimitEvent>,
    /// The running `claude`, if any. Started on the first message that needs it.
    session: Option<Session>,
    /// The running `claude` has sent `system/init`, so it got past the start-up checks where
    /// the two repairs apply.
    /// `claude` processes being stopped in the background (see the module docs).
    stopping: Vec<JoinHandle<()>>,
    /// How to start claude (program, environment); the conversation and grants are filled in
    /// per start (see [`Thread::session_config`]).
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
    copy_job: Option<rift::CopyControl>,
    created_at: i64,
    updated_at: i64,
    /// The last time anything happened (an event, a message, an answer), for idle shutdown.
    last_activity: Instant,
    /// When the running turn started, for "Working · 2m". `None` when no turn runs.
    turn_started: Option<Instant>,
    /// A grant was revoked during a turn: stop claude when the turn ends, so the next message
    /// starts one without that rule.
    stop_after_turn: bool,
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
    /// A message waits while the project folder's base is recorded.
    PreparingProject(PreparingProject),
    /// Its first message was sent.
    Saved,
}

#[derive(Debug, Default)]
enum SnapshotState {
    #[default]
    Clean,
    Pending,
    Running,
    Failed(String),
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
    path: PathBuf,
    started_at: Instant,
    last_line: Option<String>,
    main_repo: MainRepoStep,
    cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainRepoStep {
    Snapshotting,
    Ready,
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

    /// The last line Rift or its hooks printed (see [`Thread::note_copy_progress`]).
    pub fn last_line(&self) -> Option<&str> {
        self.last_line.as_deref()
    }

    /// Where the copy is being made.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The user cancelled ([`Thread::stop`]); the message comes back once Rift has stopped.
    pub fn cancelled(&self) -> bool {
        self.cancelled
    }
}

/// A message held while the project folder's base is recorded for landing (see
/// [`Thread::send`] and [`ProjectJob`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparingProject {
    message: String,
    started_at: Instant,
    /// The thread was saved before: a later message, not a draft's first one.
    was_saved: bool,
}

/// What the app shows while a thread's first message waits (see [`Thread::preparing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting<'a> {
    /// The thread's Rift copy is being made.
    Copy(&'a Preparing),
    /// The project folder's base is being recorded.
    Project(&'a PreparingProject),
}

impl Waiting<'_> {
    /// The held message.
    pub fn message(&self) -> &str {
        match self {
            Waiting::Copy(preparing) => &preparing.message,
            Waiting::Project(preparing) => &preparing.message,
        }
    }

    /// When the user sent it, for "Making a Rift copy · 3s".
    pub fn started_at(&self) -> Instant {
        match self {
            Waiting::Copy(preparing) => preparing.started_at,
            Waiting::Project(preparing) => preparing.started_at,
        }
    }

    /// The last line Rift or its hooks printed; nothing for project preparation.
    pub fn last_line(&self) -> Option<&str> {
        match self {
            Waiting::Copy(preparing) => preparing.last_line(),
            Waiting::Project(_) => None,
        }
    }

    /// Where the copy is being made; nothing for project preparation.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Waiting::Copy(preparing) => Some(preparing.path()),
            Waiting::Project(_) => None,
        }
    }

    /// The user cancelled the copy (see [`Preparing::cancelled`]). Project preparation is
    /// quick and cannot be cancelled.
    pub fn cancelled(&self) -> bool {
        match self {
            Waiting::Copy(preparing) => preparing.cancelled(),
            Waiting::Project(_) => false,
        }
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
    PreparingProject(ProjectJob),
}

#[derive(Debug)]
#[must_use = "run it off the UI thread, then call Thread::finish_project"]
pub struct ProjectJob {
    cwd: PathBuf,
    thread_id: String,
    stopping: Vec<JoinHandle<()>>,
}

impl ProjectJob {
    pub fn run(self, tools: &Tools) -> Result<String> {
        for stopping in self.stopping {
            let _ = stopping.join();
        }
        landing::prepare_project(tools, &self.cwd, &self.thread_id)
    }
}

#[derive(Debug)]
#[must_use = "run it off the UI thread, then call Thread::finish_snapshot"]
pub struct SnapshotJob {
    cwd: PathBuf,
    title: String,
    thread_id: String,
    mode: LandingMode,
    stopping: Vec<JoinHandle<()>>,
}

impl SnapshotJob {
    pub fn run(self, tools: &Tools) -> Result<()> {
        for stopping in self.stopping {
            let _ = stopping.join();
        }
        landing::snapshot(tools, &self.cwd, self.mode, &self.title, &self.thread_id)
    }
}

/// Makes a thread's Rift copy ([`rift::create`]). It can take seconds, so the app runs it on a
/// background thread.
#[must_use = "run it, then pass the result to `Thread::finish_copy`"]
pub struct CopyJob {
    project_dir: PathBuf,
    name: String,
    control: rift::CopyControl,
    thread_id: String,
    store: Store,
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
    /// Makes the copy, reporting progress to [`Thread::note_copy_progress`]. An existing
    /// destination is never removed.
    pub fn run(
        self,
        tools: &Tools,
        on_progress: impl FnMut(rift::Progress),
    ) -> Result<rift::Copy, CopyError> {
        rift::create_controlled(
            tools,
            &self.project_dir,
            &self.name,
            &self.control,
            |id| {
                self.store
                    .save_pending_id(&self.thread_id, id)
                    .map_err(|error| CopyError::Failed {
                        command: "save Rift id".to_string(),
                        detail: error.to_string(),
                        output: Vec::new(),
                    })
            },
            on_progress,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyRecovery {
    Draft,
    Kept(PathBuf),
    Saved,
}

/// A first message that could not be sent, back for the composer, and why.
#[derive(Debug)]
pub struct NotSent {
    /// Empty when nothing was held.
    pub message: String,
    pub error: anyhow::Error,
    pub copy_to_remove: Option<Box<rift::Copy>>,
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
        rift::remove(
            tools,
            &self.copy.source,
            &self.copy.path,
            &self.copy.rift_id,
        )?;
        if self.copy.base.is_some() {
            landing::cleanup_remote(tools, &self.copy.source, &self.thread_id)?;
        }
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
    review_base: Option<String>,
    landing_resolved: bool,
    snapshot_needed: bool,
}

impl Thread {
    /// A new, empty thread in `project`. It is a draft: nothing is saved until its first
    /// message is sent. `base_config` says how to start claude (usually
    /// `SessionConfig::new(&project.path)`).
    pub fn new_draft(store: Store, project: &Project, base_config: SessionConfig) -> Thread {
        let now = now_ms();
        let (events_in, events) = async_channel::unbounded();
        let mut transcript = Transcript::with_cwd(project.path.clone());
        transcript.set_presentation(crate::session::presentation(base_config.agent.provider));
        Thread {
            id: new_id(),
            project_id: project.id,
            project_dir: project.path.clone(),
            cwd: project.path.clone(),
            place: Place::ProjectFolder,
            landing_mode: project.landing_mode,
            review_base: None,
            landing_resolved: false,
            snapshot: SnapshotState::Clean,
            title: NEW_THREAD_TITLE.to_string(),
            provider: base_config.agent.provider.id().to_string(),
            session_id: crate::session::reserved_id(base_config.agent.provider),
            session_started: false,
            transcript,
            limit: None,
            controller: crate::session::Controller::new(base_config.agent.provider),
            session: None,
            stopping: Vec::new(),
            base_config,
            generation: 0,
            events_in,
            events,
            grants: Vec::new(),
            unread: false,
            active: false,
            stage: Stage::Draft,
            copy_job: None,
            created_at: now,
            updated_at: now,
            last_activity: Instant::now(),
            turn_started: None,
            stop_after_turn: false,
            saved_row: None,
            store,
        }
    }

    /// Creates a draft from a usage-free detection result. The selection is immutable.
    pub fn draft_with_agent(
        store: Store,
        project: &Project,
        agent: crate::agent::AgentConfig,
        detected: &crate::providers::ProviderStatus,
    ) -> Result<Self> {
        agent.validate(detected)?;
        let crate::providers::Availability::Ready { binary, .. } = &detected.availability else {
            unreachable!("validated above")
        };
        let mut config = SessionConfig::new(&project.path);
        config.program = binary.clone();
        config.agent = agent;
        Ok(Self::new_draft(store, project, config))
    }
    /// Changes an unsent draft's selection. Sending (including a held copy job) freezes it.
    pub fn set_agent_config(
        &mut self,
        agent: crate::agent::AgentConfig,
        detected: &crate::providers::ProviderStatus,
    ) -> Result<()> {
        if !self.is_draft() || self.copy().is_some() {
            bail!("Only an unsent draft can change its agent settings");
        }
        agent.validate(detected)?;
        let crate::providers::Availability::Ready { binary, .. } = &detected.availability else {
            unreachable!("validated above")
        };
        self.end_session();
        self.provider = agent.provider.id().into();
        self.session_id = crate::session::reserved_id(agent.provider);
        self.session_started = false;
        self.controller = crate::session::Controller::new(agent.provider);
        self.transcript
            .set_presentation(crate::session::presentation(agent.provider));
        self.base_config.program = binary.clone();
        self.base_config.extra_args.clear();
        self.base_config.agent = agent;
        Ok(())
    }

    pub fn agent_config(&self) -> Option<&crate::agent::AgentConfig> {
        self.unavailable_reason()
            .is_none()
            .then_some(&self.base_config.agent)
    }
    pub fn unavailable_reason(&self) -> Option<String> {
        crate::providers::Provider::from_id(&self.provider)
            .is_none()
            .then(|| format!("Provider '{}' is unavailable", self.provider))
    }
    pub fn conversation(&self) -> crate::agent::ConversationState {
        if self.session_id.is_empty() {
            crate::agent::ConversationState::Uncreated
        } else if self.session_started {
            crate::agent::ConversationState::Established(self.session_id.clone())
        } else {
            crate::agent::ConversationState::Reserved(self.session_id.clone())
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
        if let Some(provider) = crate::providers::Provider::from_id(&row.provider) {
            crate::session::validate_conversation(provider, &row.conversation)?;
        }
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
        thread.base_config.restore_saved(&row);
        thread.limit = thread.store.thread_limit(&row.id)?;
        thread.id = row.id;
        thread.cwd = row.workdir;
        thread.place = row.copy.map_or(Place::ProjectFolder, Place::Copy);
        thread.landing_mode = row.landing_mode;
        thread.review_base = row
            .review_base
            .or_else(|| thread.copy().and_then(|copy| copy.base.clone()));
        thread.landing_resolved = row.landing_resolved;
        thread.snapshot = if row.snapshot_needed
            || (row.status.is_busy()
                && thread.review_base.is_some()
                && thread.landing_mode == LandingMode::HarnessManaged)
        {
            SnapshotState::Pending
        } else {
            SnapshotState::Clean
        };
        thread.title = row.title;
        thread.provider = row.provider;
        thread.session_id = row.conversation.id().unwrap_or("").to_owned();
        thread.session_started = matches!(
            row.conversation,
            crate::agent::ConversationState::Established(_)
        );
        thread.controller = crate::session::Controller::new(thread.base_config.agent.provider);
        transcript.set_presentation(crate::session::saved_presentation(&thread.provider));
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
            review_base: thread.review_base.clone(),
            landing_resolved: thread.landing_resolved,
            snapshot_needed: thread.work_pending(),
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

    pub fn recovered_draft(
        store: Store,
        project: &Project,
        base_config: SessionConfig,
        pending: &PendingCopy,
    ) -> Thread {
        let mut thread = Thread::new_draft(store, project, base_config);
        thread.id = pending.thread_id.clone();
        thread.created_at = pending.created_at;
        thread.place = Place::NewCopy;
        thread
    }

    pub fn recover_copy_cleanup(store: &Store, project: &Project, tools: &Tools) -> Result<()> {
        let mut errors = Vec::new();
        for cleanup in store.copy_cleanup(project.id)? {
            let result = check_pending_destination(
                project,
                cleanup.project_id,
                &cleanup.thread_id,
                &cleanup.path,
            )
            .and_then(|()| {
                rift::remove_pending(
                    tools,
                    &project.path,
                    &cleanup.path,
                    cleanup.rift_id.as_deref(),
                )
                .map_err(anyhow::Error::from)
            });
            match result {
                Ok(()) => store.delete_copy_cleanup(&cleanup.path),
                Err(error) => errors.push(error.to_string()),
            }
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        Ok(())
    }

    pub fn recover_pending_copy(
        store: &Store,
        project: &Project,
        pending: &PendingCopy,
        tools: &Tools,
    ) -> Result<CopyRecovery> {
        check_pending_destination(
            project,
            pending.project_id,
            &pending.thread_id,
            &pending.path,
        )?;
        if pending.state == PendingState::Sent {
            let owner = store.copy_owner(&pending.path)?;
            store.delete_pending_copy(&pending.thread_id);
            if owner.as_deref() != Some(pending.thread_id.as_str()) {
                return Ok(CopyRecovery::Kept(pending.path.clone()));
            }
            return Ok(CopyRecovery::Saved);
        }
        if pending.state == PendingState::MakingCopy {
            rift::remove_pending(
                tools,
                &project.path,
                &pending.path,
                pending.rift_id.as_deref(),
            )?;
            let mut recovered = pending.clone();
            recovered.state = PendingState::Draft;
            store.save_pending_copy(recovered)?;
        }
        if pending.intent == PendingIntent::Discard {
            store.delete_pending_copy(&pending.thread_id);
        }
        Ok(CopyRecovery::Draft)
    }

    // -----------------------------------------------------------------------------------------
    // What the app reads
    // -----------------------------------------------------------------------------------------

    fn remember_agent(&self) {
        if let Some(config) = self.agent_config() {
            self.store.remember_agent_choice(self.project_id, config);
        }
    }
    pub fn limit_event(&self) -> Option<&crate::agent::RateLimitEvent> {
        self.limit.as_ref()
    }
    pub fn paused_until(&self, now: i64) -> Option<i64> {
        crate::picker::paused_until(self.limit.as_ref(), now)
    }
    pub fn refresh_limits(&mut self, status: &crate::providers::ProviderStatus) {
        if self
            .agent_config()
            .is_some_and(|a| a.provider == status.provider)
            && status.clears_limit()
        {
            self.limit = status.limit_event();
            if self.is_saved()
                && let Some(report) = &self.limit
            {
                self.store.save_thread_limit(&self.id, report);
            }
        }
    }
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

    /// The message that waits while the thread's Rift copy is made or the project folder's
    /// base is recorded. The thread is busy then: nothing else can be sent, and no claude
    /// runs for it.
    pub fn preparing(&self) -> Option<Waiting<'_>> {
        match &self.stage {
            Stage::Preparing(preparing) => Some(Waiting::Copy(preparing)),
            Stage::PreparingProject(preparing) => Some(Waiting::Project(preparing)),
            Stage::Draft | Stage::Saved => None,
        }
    }

    /// True if a new Rift copy must not be made now, because making one snapshots the main
    /// repo and may run `jj new` there: this thread works in the project folder and its agent
    /// is busy (a turn, an approval, a background task, any of which can be mid-edit), or this
    /// thread's own copy is being made. The app refuses with "wait for <thread> to finish".
    pub fn blocks_copy_creation(&self) -> bool {
        match (&self.stage, &self.place) {
            (Stage::Preparing(_) | Stage::PreparingProject(_), _) => true,
            (_, Place::ProjectFolder) => {
                self.status().is_busy()
                    || self.work_pending()
                    || !self.background_tasks().is_empty()
            }
            (_, Place::NewCopy | Place::Copy(_)) => false,
        }
    }

    pub fn blocks_project_send(&self) -> bool {
        matches!(&self.stage, Stage::Preparing(preparing) if preparing.main_repo == MainRepoStep::Snapshotting)
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

    pub fn presentation(&self) -> &crate::agent::AgentPresentation {
        self.transcript.presentation()
    }

    pub fn grant_views(&self) -> Vec<crate::agent::GrantView> {
        self.controller.grants(&self.grants, self.session.as_ref())
    }

    pub fn failure_summary(&self) -> Option<String> {
        self.transcript
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::Error { title, detail, .. } => {
                    Some(self.controller.failure_reason(title, detail))
                }
                _ => None,
            })
    }

    /// What claude keeps running in the background now (a dev server, say), even between
    /// turns. Empty when no claude runs: stopping claude stops these too.
    pub fn background_tasks(&self) -> &[crate::agent::BackgroundTaskView] {
        self.transcript.background_tasks()
    }

    /// The channel this thread's events arrive on, for the whole life of the thread. Pass each
    /// event to [`Thread::apply`]. Read it from one place only: each event is delivered once,
    /// to whichever copy of the receiver asks first.
    pub fn events(&self) -> Receiver<ThreadEvent> {
        self.events.clone()
    }

    /// The configuration used for the next provider process.
    pub fn session_config(&self) -> SessionConfig {
        let rules: Vec<String> = self.grants.iter().map(|grant| grant.rule.clone()).collect();
        let mut config = self.base_config.clone();
        config.bind(&self.session_id, self.session_started, &rules);
        config.cwd = self.cwd.clone();
        if self.landing_mode == LandingMode::HarnessManaged {
            config.instructions = Some("Newjen manages version control for this thread. Do not commit, describe, rebase, merge, create bookmarks or branches, or otherwise write version-control metadata. Make file changes only. Newjen snapshots the folder after each turn.".to_string());
        }
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

    /// The file edit the pending approval proposes, for its preview. `None` when nothing is
    /// pending or it is not a file edit.
    pub fn pending_edit(&self) -> Option<crate::edits::EditProposal> {
        let request = self.transcript.pending_approval()?;
        self.controller.edit_proposal(request.view(), &self.cwd)
    }

    pub fn landing_mode(&self) -> LandingMode {
        self.landing_mode
    }

    pub fn set_landing_mode(&mut self, mode: LandingMode) -> Result<()> {
        if !self.is_draft() {
            bail!("only a new thread can choose its landing mode");
        }
        self.landing_mode = mode;
        Ok(())
    }

    pub fn review_base(&self) -> Option<&str> {
        self.review_base.as_deref()
    }

    pub fn landing_finished(&self) -> bool {
        self.landing_resolved
    }

    pub fn owns_project_work(&self) -> bool {
        self.place == Place::ProjectFolder
            && !self.landing_resolved
            && (self.review_base.is_some()
                || matches!(self.stage, Stage::PreparingProject(_))
                || self.status().is_busy()
                || !self.background_tasks().is_empty())
    }

    pub fn work_pending(&self) -> bool {
        !matches!(self.snapshot, SnapshotState::Clean)
    }

    pub fn snapshot_error(&self) -> Option<&str> {
        match &self.snapshot {
            SnapshotState::Failed(error) => Some(error),
            _ => None,
        }
    }

    pub fn take_snapshot_job(&mut self) -> Option<SnapshotJob> {
        if !matches!(self.snapshot, SnapshotState::Pending)
            || self.status().is_busy()
            || !self.background_tasks().is_empty()
        {
            return None;
        }
        self.end_session();
        self.snapshot = SnapshotState::Running;
        Some(SnapshotJob {
            cwd: self.cwd.clone(),
            title: self.title.clone(),
            thread_id: self.id.clone(),
            mode: self.landing_mode,
            stopping: std::mem::take(&mut self.stopping),
        })
    }

    pub fn finish_snapshot(&mut self, result: Result<()>) {
        if !matches!(self.snapshot, SnapshotState::Running) {
            return;
        }
        self.snapshot = match result {
            Ok(()) => SnapshotState::Clean,
            Err(error) => SnapshotState::Failed(error.to_string()),
        };
        self.save_with(false);
    }

    pub fn retry_snapshot(&mut self) {
        if matches!(self.snapshot, SnapshotState::Failed(_)) {
            self.snapshot = SnapshotState::Pending;
        }
    }

    pub fn finish_landing(&mut self) {
        self.end_session();
        self.landing_resolved = true;
        self.save();
    }

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
        let now = crate::store::now_ms() / 1000;
        if let Some(reset) = self.paused_until(now) {
            bail!(
                "{} usage limit reached; sending is paused until {}. Newjen won't retry or resume on its own",
                self.presentation().label,
                crate::picker::reset_time(reset, now)
            );
        }

        if text.trim().is_empty() {
            bail!("there is nothing to send");
        }
        if self.preparing().is_some() {
            bail!("Newjen is still making this thread's Rift copy");
        }
        if self.landing_resolved {
            bail!("this thread’s work was already landed or discarded");
        }
        if self.work_pending() {
            bail!("Newjen is still snapshotting this thread’s work");
        }
        if self.status().is_busy() {
            bail!(
                "{} is still working on the last message",
                self.presentation().label
            );
        }
        if self.place == Place::NewCopy {
            let name = copy_name(&self.id);
            let path = rift::destination(&self.project_dir, &name)?;
            rift::check_destination(&path)?;
            self.store.save_pending_copy(PendingCopy {
                thread_id: self.id.clone(),
                project_id: self.project_id,
                path: path.clone(),
                intent: PendingIntent::Restore(text.to_string()),
                state: PendingState::MakingCopy,
                rift_id: None,
                created_at: self.created_at,
            })?;
            let control = rift::CopyControl::default();
            self.copy_job = Some(control.clone());
            self.title = title_from_message(text);
            self.stage = Stage::Preparing(Preparing {
                message: text.to_string(),
                path,
                started_at: Instant::now(),
                last_line: None,
                main_repo: MainRepoStep::Snapshotting,
                cancelled: false,
            });
            self.last_activity = Instant::now();
            self.remember_agent();
            return Ok(Sent::MakingCopy(CopyJob {
                project_dir: self.project_dir.clone(),
                name,
                control,
                thread_id: self.id.clone(),
                store: self.store.clone(),
            }));
        }
        if self.review_base.is_none() && landing::available(&self.cwd) {
            self.end_session();
            let was_saved = self.is_saved();
            if !was_saved {
                self.title = title_from_message(text);
            }
            self.stage = Stage::PreparingProject(PreparingProject {
                message: text.to_string(),
                started_at: Instant::now(),
                was_saved,
            });
            return Ok(Sent::PreparingProject(ProjectJob {
                cwd: self.cwd.clone(),
                thread_id: self.id.clone(),
                stopping: std::mem::take(&mut self.stopping),
            }));
        }
        self.deliver(text)?;
        Ok(Sent::Started)
    }

    /// Takes what the [`ProjectJob`] from [`Thread::send`] returned. With the base recorded,
    /// the held message is sent, as [`Thread::send`] sends it. Otherwise, or if that send
    /// fails, the thread is as it was and the message comes back for the composer.
    pub fn finish_project(&mut self, prepared: Result<String>) -> Result<(), NotSent> {
        let preparing = match std::mem::replace(&mut self.stage, Stage::Draft) {
            Stage::PreparingProject(preparing) => preparing,
            stage => {
                self.stage = stage;
                return Err(NotSent {
                    message: String::new(),
                    error: anyhow!("this thread was not waiting for project preparation"),
                    copy_to_remove: None,
                });
            }
        };
        let message = preparing.message;
        self.stage = if preparing.was_saved {
            Stage::Saved
        } else {
            Stage::Draft
        };
        let sent = prepared.and_then(|base| {
            self.end_session();
            self.review_base = Some(base);
            self.deliver(&message)
        });
        sent.map_err(|error| {
            if !preparing.was_saved {
                self.title = NEW_THREAD_TITLE.to_string();
            }
            NotSent {
                message,
                error,
                copy_to_remove: None,
            }
        })
    }

    /// [`Thread::stop`] while the copy is made: Rift and its hooks are stopped as quitting
    /// stops them, and the half-made copy is removed. The held message is never sent; it comes
    /// back through [`Thread::finish_copy`] with [`CopyError::Cancelled`].
    fn cancel_copy(&mut self) {
        let Stage::Preparing(preparing) = &mut self.stage else {
            return;
        };
        if preparing.cancelled {
            return;
        }
        preparing.cancelled = true;
        if let Some(control) = &self.copy_job
            && let Some(stopping) = control.cancel()
        {
            self.stopping.push(stopping);
        }
    }

    pub fn note_copy_progress(&mut self, progress: rift::Progress) {
        if let Stage::Preparing(preparing) = &mut self.stage {
            match progress {
                rift::Progress::MainRepoReady => preparing.main_repo = MainRepoStep::Ready,
                rift::Progress::Line(line) if !line.trim().is_empty() => {
                    preparing.last_line = Some(line)
                }
                rift::Progress::Line(_) => {}
            }
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
                    copy_to_remove: made
                        .ok()
                        .filter(|copy| self.copy() != Some(copy))
                        .map(Box::new),
                });
            }
        };
        let message = preparing.message;
        let (made, copy_to_remove) = match made {
            // Rift finished just as the user cancelled; the copy goes the way a half-made one
            // does, without being used.
            Ok(copy) if preparing.cancelled => (Err(CopyError::Cancelled), Some(Box::new(copy))),
            made => (made, None),
        };
        if made.is_err() {
            let path = preparing.path;
            if copy_to_remove.is_none()
                && (matches!(&made, Err(CopyError::Exists { .. })) || rift::pending_is_gone(&path))
            {
                self.store.delete_pending_copy(&self.id);
            }
        }
        self.copy_job = None;
        let sent = made.map_err(anyhow::Error::from).and_then(|copy| {
            self.end_session();
            self.review_base = copy.base.clone();
            self.cwd = copy.workdir.clone();
            self.transcript.set_cwd(copy.workdir.clone());
            self.place = Place::Copy(copy);
            self.deliver(&message)
        });
        sent.map_err(|error| {
            self.title = NEW_THREAD_TITLE.to_string();
            NotSent {
                message,
                error,
                copy_to_remove,
            }
        })
    }

    /// The part of [`Thread::send`] that writes the message to claude.
    fn deliver(&mut self, text: &str) -> Result<()> {
        self.save_draft(text)?;
        if self.session.is_none() {
            self.spawn()?;
        }
        if self.is_draft()
            && let Some(copy) = self.copy()
        {
            self.store.save_pending_copy(PendingCopy {
                thread_id: self.id.clone(),
                project_id: self.project_id,
                path: copy.path.clone(),
                rift_id: Some(copy.rift_id.clone()),
                intent: PendingIntent::Restore(text.to_string()),
                state: PendingState::Sent,
                created_at: self.created_at,
            })?;
        }
        let session = self.session.as_ref().expect("started above");
        if let Err(error) = session.send_prompt(text) {
            // This claude can no longer receive anything; the next message starts a new one.
            self.end_session();
            self.save_with(false);
            return Err(error);
        }
        if self.is_draft() {
            self.remember_agent();
            self.stage = Stage::Saved;
            self.title = title_from_message(text);
        }
        self.transcript.push_user(text);
        self.controller.begin_message(text);
        self.turn_started = Some(Instant::now());
        self.last_activity = Instant::now();
        self.save();
        Ok(())
    }

    /// Answers the approval on screen. `key` is the request the button belonged to; a
    /// stale click (that request was already answered) does nothing.
    ///
    /// "Always allow" ([`crate::agent::ApprovalIntent::AllowForSession`]) also saves the rules it makes
    /// claude remember (`protocol::session_rules`), so every later `claude` for this thread
    /// gets them too. When it remembers nothing it is allow once, and nothing is saved.
    pub fn answer(
        &mut self,
        key: Option<crate::agent::ApprovalKey>,
        answer: crate::agent::ApprovalIntent,
    ) -> Result<()> {
        self.answer_with_note(key, answer, None).map(|_| ())
    }

    /// [`Thread::answer`] with the user's note for a denial: claude gets it as the denial's
    /// message, so the model knows why; Codex's decision carries no text, so the note comes
    /// back as [`NoteDelivery::Kept`] for the composer. An allow ignores the note.
    pub fn answer_with_note(
        &mut self,
        key: Option<crate::agent::ApprovalKey>,
        answer: crate::agent::ApprovalIntent,
        note: Option<&str>,
    ) -> Result<NoteDelivery> {
        let Some(request) = self.transcript.pending_approval().cloned() else {
            return Ok(NoteDelivery::Delivered);
        };
        if key.is_some_and(|key| key != request.key()) {
            return Ok(NoteDelivery::Delivered);
        }
        // Resolve first: a second click or key press now finds nothing to answer.
        self.transcript.resolve_approval(request.key(), answer);
        self.last_activity = Instant::now();
        let Some(sent) = self
            .controller
            .answer(request.key(), answer, note, self.session.as_ref())
        else {
            return Ok(NoteDelivery::Delivered);
        };
        let sent = sent.map(|receipt| {
            let now = now_ms();
            for mut grant in receipt.grants {
                grant.created_at = now;
                self.add_grant(grant);
            }
            match receipt.note_kept {
                Some(note) => NoteDelivery::Kept(note),
                None => NoteDelivery::Delivered,
            }
        });
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
        if self.preparing().is_some() {
            self.cancel_copy();
            return Ok(());
        }
        if self.transcript.pending_approval().is_some() {
            return self.answer(None, crate::agent::ApprovalIntent::DenyAndStop);
        }
        if !self.status().is_busy() {
            return Ok(());
        }
        self.last_activity = Instant::now();
        if !self.controller.initialized {
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
            self.transcript.apply(&self.controller.translate(
                &SessionEvent::Exited {
                    code: None,
                    requested: true,
                },
                Some(&self.cwd),
            ));
        }
        self.end_session();
        self.controller.reset();
        self.last_activity = Instant::now();
        let started = self.spawn();
        self.save();
        started
    }

    /// Forgets a grant. claude cannot drop a rule it was given, so the process is stopped, with
    /// any command it runs in the background: now if it is idle, or when the running turn ends.
    /// The next message starts a new one with the remaining rules (`--resume`, so the
    /// conversation goes on).
    pub fn revoke(&mut self, id: &crate::agent::GrantId) {
        if self
            .grant_views()
            .iter()
            .any(|grant| &grant.id == id && grant.lifetime == crate::agent::GrantLifetime::Process)
        {
            self.end_turn_as_stopped();
            return;
        }
        let rule = id.0.as_str();
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
    /// sends nothing and returns its copy for removal.
    pub fn archive(&mut self) -> Option<Removal> {
        if self.session.is_some() {
            self.transcript.apply(&self.controller.translate(
                &SessionEvent::Exited {
                    code: None,
                    requested: true,
                },
                Some(&self.cwd),
            ));
        }
        self.end_session();
        self.save();
        match self.stage {
            Stage::Saved => self.store.archive(&self.id, now_ms()),
            Stage::PreparingProject(ref preparing) if preparing.was_saved => {
                self.store.archive(&self.id, now_ms());
                self.stage = Stage::Saved;
            }
            Stage::Preparing(_) => {
                self.store.discard_pending_copy(&self.id);
                self.stage = Stage::Draft;
            }
            Stage::PreparingProject(_) => self.stage = Stage::Draft,
            Stage::Draft => self.store.discard_pending_copy(&self.id),
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

    pub fn save_draft(&self, message: &str) -> Result<()> {
        if !self.is_draft() {
            return Ok(());
        }
        if let Some(mut pending) = self
            .store
            .pending_copies(self.project_id)?
            .into_iter()
            .find(|pending| pending.thread_id == self.id)
        {
            pending.intent = if message.trim().is_empty() {
                PendingIntent::Discard
            } else {
                PendingIntent::Restore(message.to_string())
            };
            self.store.save_pending_copy(pending)?;
        }
        Ok(())
    }

    /// The first half of [`Thread::shutdown_for_quit`]: settles and saves the thread and
    /// starts stopping its `claude`, without waiting for either. Pass what it returns for
    /// every thread to one [`finish_quit`] call.
    pub fn begin_quit(&mut self) -> Quitting {
        if let Some(control) = self.copy_job.take()
            && let Some(stopping) = control.cancel()
        {
            self.stopping.push(stopping);
        }
        if self.preparing().is_some() {
            self.stage = Stage::Draft;
        }
        if self.status().is_busy() || !self.background_tasks().is_empty() {
            if self.landing_mode == LandingMode::HarnessManaged && self.review_base.is_some() {
                self.snapshot = SnapshotState::Pending;
            }
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

        let was_busy = self.status().is_busy();
        let (batch, surfaced_write_failure) = match self.controller.apply(
            &event,
            self.generation,
            &self.cwd,
            &mut self.session_id,
            &mut self.session_started,
        ) {
            crate::session::Update::Held => return,
            crate::session::Update::Restart => {
                self.start_again();
                return;
            }
            crate::session::Update::FreshRestart => {
                self.transcript.insert_fresh_start_before_last_message();
                self.start_again();
                return;
            }
            crate::session::Update::FreshStopped => {
                self.end_turn_as_stopped();
                self.transcript.push_fresh_start();
                self.save();
                return;
            }
            crate::session::Update::Apply(batch) => (batch, false),
            crate::session::Update::SurfacedWriteFailure(batch) => (batch, true),
        };
        let approval_asked = batch
            .events
            .iter()
            .any(|event| matches!(event, crate::agent::AgentEvent::ApprovalRequested(_)));
        let limit_rejected = batch.events.iter().any(|event| {
            matches!(
                event,
                crate::agent::AgentEvent::RateLimit(crate::agent::RateLimitEvent::Rejected(_))
            )
        });
        for event in &batch.events {
            if let crate::agent::AgentEvent::RateLimit(report) = event {
                self.limit = Some(report.clone());
            }
        }
        self.transcript.apply(&batch);
        let turn_ended = was_busy && !self.status().is_busy();
        // claude started a turn by itself (see `Transcript::apply`).
        if !was_busy && self.status().is_busy() {
            self.turn_started = Some(Instant::now());
        }
        if approval_asked && becomes_unread(self.active, false, true) {
            self.set_unread(true);
        }

        match &event {
            // The process is gone (so stopping it returns at once); the next message starts a
            // new one.
            SessionEvent::Exited { .. } => {
                self.session = None;
                self.controller.initialized = false;
                self.generation += 1;
            }
            // This process can no longer be talked to; let it go.
            SessionEvent::WriteFailed(_) | SessionEvent::InterruptTimedOut => self.end_session(),
            _ if surfaced_write_failure => self.end_session(),
            _ => {}
        }
        if limit_rejected {
            self.end_session();
        }
        if turn_ended {
            self.after_turn_ended();
        }
        if batch.checkpoint == crate::agent::Checkpoint::Save {
            self.save();
            if let Some(report) = &self.limit {
                self.store.save_thread_limit(&self.id, report);
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------------------------

    /// Starts `claude` with [`Thread::session_config`] and forwards its events to the thread's
    /// channel under a new generation.
    fn spawn(&mut self) -> Result<()> {
        if let Some(reason) = self.unavailable_reason() {
            bail!("{reason}");
        }
        crate::session::validate_conversation(
            self.base_config.agent.provider,
            &self.conversation(),
        )?;
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
        self.controller.spawned();
        Ok(())
    }

    /// Lets the current `claude` go: later events from it are ignored, and it is stopped on a
    /// background thread (see the module docs).
    fn end_session(&mut self) {
        self.generation += 1;
        self.stop_after_turn = false;
        self.controller.initialized = false;
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
            Ok(()) => match (self.controller.message(), &self.session) {
                (Some(message), Some(session)) => session.send_prompt(message).err().map(|error| {
                    (
                        self.presentation().send_failed,
                        format!("{}: {error:#}", self.presentation().send_detail_prefix),
                    )
                }),
                _ => None,
            },
            Err(error) => Some((self.presentation().start_failed, format!("{error:#}"))),
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
        self.transcript.apply(&self.controller.translate(
            &SessionEvent::Exited {
                code: None,
                requested: true,
            },
            Some(&self.cwd),
        ));
        self.end_session();
        // Whatever a repair was waiting for no longer matters.
        self.controller.reset();
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
        self.controller.end_turn();
        if self.landing_mode == LandingMode::HarnessManaged && self.review_base.is_some() {
            self.snapshot = SnapshotState::Pending;
        }
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
            review_base: self.review_base.clone(),
            landing_resolved: self.landing_resolved,
            snapshot_needed: self.work_pending(),
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
            conversation: self.conversation(),
            model: self.base_config.agent.model.clone(),
            effort: self.base_config.agent.effort.clone(),
            speed: self.base_config.agent.speed,
            status: self.status(),
            unread: self.unread,
            created_at: self.created_at,
            updated_at: self.updated_at,
            archived_at: None,
            copy: self.copy().cloned(),
            landing_mode: self.landing_mode,
            review_base: self.review_base.clone(),
            landing_resolved: self.landing_resolved,
            snapshot_needed: self.work_pending(),
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
#[cfg(test)]
fn claude_config(
    base: &SessionConfig,
    session_id: &str,
    session_started: bool,
    allowed_rules: &[String],
) -> SessionConfig {
    let mut config = base.clone();
    config.bind(session_id, session_started, allowed_rules);
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

fn check_pending_destination(
    project: &Project,
    project_id: i64,
    thread_id: &str,
    path: &Path,
) -> Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let prefix = format!("{}-", copy_prefix(thread_id));
    let suffix = name.strip_prefix(&prefix).unwrap_or("");
    if project_id != project.id
        || path != rift::destination(&project.path, name)?
        || suffix.len() != 6
        || !suffix
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    {
        bail!("the pending Rift copy is outside its destination");
    }
    Ok(())
}

/// The prefix shared by a thread's copy attempts.
fn copy_prefix(thread_id: &str) -> String {
    let short: String = thread_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    format!("newjen-{short}")
}

fn copy_name(thread_id: &str) -> String {
    let suffix: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(6)
        .collect();
    format!("{}-{suffix}", copy_prefix(thread_id))
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
    fn checkpoint(event: &SessionEvent) -> bool {
        crate::providers::claude::events::ClaudeEvents::default()
            .translate(event, None)
            .checkpoint
            == crate::agent::Checkpoint::Save
    }

    #[test]
    fn a_turn_ended_by_a_held_write_failure_is_never_resent() {
        let home = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(home.path()).unwrap();
        let project = store.open_project(project_dir.path()).unwrap();
        let received = project_dir.path().join("stdin.log");
        let spawns = project_dir.path().join("spawns.log");
        let script = project_dir.path().join("claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho start >> '{}'\ncat >> '{}'\n",
                spawns.display(),
                received.display()
            ),
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
        assert_eq!(delivered(), 1);
        let generation = thread.generation;
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
            thread.apply(ThreadEvent {
                generation,
                event: e,
            });
        }

        assert!(!thread.status().is_busy(), "{:?}", thread.status());
        assert_eq!(thread.controller.message(), None);
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(delivered(), 1, "the message reached claude again");
        assert_eq!(std::fs::read_to_string(spawns).unwrap().lines().count(), 1);
        assert!(!thread.has_session());
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
        assert!(!checkpoint(&SessionEvent::Line(delta)));
        assert!(!checkpoint(&SessionEvent::Stderr("warning".to_string())));
        let finished = Incoming::parse(
            r#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"Hi"}]}}"#,
        );
        assert!(checkpoint(&SessionEvent::Line(finished)));
        let result = Incoming::parse(r#"{"type":"result","subtype":"success","is_error":false}"#);
        assert!(checkpoint(&SessionEvent::Line(result)));
        assert!(checkpoint(&SessionEvent::Exited {
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
