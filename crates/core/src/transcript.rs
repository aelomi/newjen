//! The UI-agnostic state of one conversation.
//!
//! A [`Transcript`] is fed atomic [`AgentBatch`] values (see [`Transcript::apply`])
//! and turns semantic events into a simple list of [`Item`]s a UI can render: user prompts,
//! assistant replies, tool calls, end-of-turn captions and errors. It also tracks the overall
//! [`Status`] and the queue of approvals waiting for an answer.
//!
//! It never talks to the process itself. The app calls `Session::answer` and then
//! [`Transcript::resolve_approval`] with the same answer.
//!
//! Items are saved to the store (see `store.rs`) as JSON, one row per item. The transcript
//! remembers the lowest index that changed since the last save ([`Transcript::dirty_from`]),
//! so a save rewrites only the rows from there on.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{
    AgentBatch, AgentEvent, BackgroundTaskView, ContentKey, TextUpdate, ToolDescriptor,
    ToolOutcome, TurnOutcome,
};

/// How many stderr lines to keep for the "Claude exited" error.
const STDERR_LINES_KEPT: usize = 20;

/// The version of the JSON shape [`Item`]s are saved in. Saved next to each row, so a later
/// Newjen can tell old rows from new ones. Bump it when a change would make old rows load
/// wrongly (renaming or removing a field or variant); adding a field with a default does not
/// need it.
pub const ITEM_SCHEMA_VERSION: u32 = 1;

/// The notice added when Newjen quit (or crashed) while a thread was working or waiting.
pub const STOPPED_ON_QUIT: &str = "Stopped when Newjen quit";
/// The line under [`STOPPED_ON_QUIT`], next to its Continue button.
pub const STOPPED_ON_QUIT_DETAIL: &str = "Nothing runs until you continue.";
pub const STOPPED_UNSENT_DETAIL: &str =
    "Claude never got your last message. Nothing runs until you send it again.";
/// Why an approval left unanswered at quit is [`ToolState::Expired`]. Shown as
/// "Expired · Newjen quit before you answered".
pub const EXPIRED_NOTE: &str = "Newjen quit before you answered";
/// The note on a tool call that was still running when Newjen quit.
pub const STOPPED_TOOL_NOTE: &str = "Stopped when Newjen quit";
/// The note on a command that ran in the background until Newjen stopped its claude (to drop
/// a revoked rule, or for "Try again").
pub const BACKGROUND_STOPPED_NOTE: &str = "Stopped with Claude";
/// The quiet row where a conversation had to start over because Claude no longer had it.
pub const FRESH_START_NOTE: &str = "Claude no longer has the earlier conversation; it starts \
                                    fresh from here (earlier messages stay visible)";
/// The quiet row shown for a saved row this version of Newjen cannot read.
pub const UNREADABLE_NOTE: &str = "This row was saved by a different version of Newjen";

/// What the conversation is doing right now. Saved with the thread (see [`Status::as_str`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Nothing sent yet.
    Idle,
    /// Claude is working on a turn.
    Working,
    /// Claude is waiting for the user to approve a tool call.
    Waiting,
    /// The last turn finished normally.
    Done,
    /// The user stopped the last turn.
    Stopped,
    /// The last turn failed, or the process exited unexpectedly.
    Failed,
}

impl Status {
    /// The name the store saves, e.g. `"working"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Idle => "idle",
            Status::Working => "working",
            Status::Waiting => "waiting",
            Status::Done => "done",
            Status::Stopped => "stopped",
            Status::Failed => "failed",
        }
    }

    /// Reads a name saved by [`Status::as_str`]. A name this version does not know (saved by
    /// a newer Newjen) reads as `Idle`, the quietest status, rather than failing the thread.
    pub fn from_saved(name: &str) -> Status {
        match name {
            "working" => Status::Working,
            "waiting" => Status::Waiting,
            "done" => Status::Done,
            "stopped" => Status::Stopped,
            "failed" => Status::Failed,
            _ => Status::Idle,
        }
    }

    /// True while a turn is running: Claude is working, or waiting for an approval.
    pub fn is_busy(self) -> bool {
        matches!(self, Status::Working | Status::Waiting)
    }
}

/// The state of one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Running,
    /// Waiting for the user's approval.
    Waiting,
    Done,
    /// The user said no.
    Denied,
    /// The tool ran and reported an error, or the turn ended before it finished.
    Failed,
    /// It was waiting for an approval when Newjen quit, so the question can no longer be
    /// answered (see [`EXPIRED_NOTE`]).
    Expired,
}

/// One row of the conversation. Every item has an `id` that never changes, so a UI can use it
/// as a stable element id.
///
/// Saved as JSON with a `"type"` field naming the variant, e.g.
/// `{"type":"user","id":1,"text":"hi"}` (see [`Item::to_saved`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Item {
    User {
        id: u64,
        text: String,
        /// Sent, but claude has not started its turn yet (no `system/init`), so claude may never
        /// have received it. Saved, so a relaunch knows too (see
        /// [`Transcript::unconfirmed_message`]).
        #[serde(default, skip_serializing_if = "is_false")]
        unconfirmed: bool,
    },
    Assistant {
        id: u64,
        text: String,
        /// True while the turn is still running (text may still grow).
        live: bool,
        /// True when the user stopped the reply part-way.
        interrupted: bool,
    },
    Tool {
        id: u64,
        tool_use_id: String,
        name: String,
        display_name: String,
        /// A one-line description, e.g. the Bash command or the file path.
        summary: String,
        input: Value,
        state: ToolState,
        output: Option<String>,
        took: Option<Duration>,
        /// The command this call started still runs in the background (Bash's
        /// `run_in_background`), after the call itself finished. Saved, so a relaunch can tell
        /// that quitting stopped it (see [`Transcript::normalize_after_relaunch`]).
        #[serde(default, skip_serializing_if = "is_false")]
        background: bool,
    },
    TurnEnd {
        id: u64,
        duration_ms: Option<u64>,
        /// This turn's cost (not the session's running total).
        cost_usd: Option<f64>,
        stopped: bool,
    },
    Error {
        id: u64,
        title: String,
        detail: String,
    },
    /// Newjen quit (or crashed) while the thread was working or waiting, so the turn was
    /// stopped. Shown as [`STOPPED_ON_QUIT`] / [`STOPPED_ON_QUIT_DETAIL`] with a Continue
    /// button. Added by [`Transcript::normalize_after_relaunch`].
    Stopped { id: u64 },
    /// Claude no longer had the earlier conversation, so a new one starts here. The rows above
    /// stay visible but Claude does not remember them. Shown as [`FRESH_START_NOTE`].
    FreshStart { id: u64 },
    /// A saved row this version of Newjen cannot read (written by a newer or older version).
    /// Shown as a quiet [`UNREADABLE_NOTE`] row. Keeps the saved row as it was, so saving the
    /// thread again never destroys it.
    #[serde(skip)]
    Unreadable {
        id: u64,
        schema_version: u32,
        json: String,
    },
}

impl Item {
    /// The item's stable id.
    pub fn id(&self) -> u64 {
        match self {
            Item::User { id, .. }
            | Item::Assistant { id, .. }
            | Item::Tool { id, .. }
            | Item::TurnEnd { id, .. }
            | Item::Error { id, .. }
            | Item::Stopped { id }
            | Item::FreshStart { id }
            | Item::Unreadable { id, .. } => *id,
        }
    }

    /// The item as the store saves it: the schema version and the JSON text.
    pub fn to_saved(&self) -> (u32, String) {
        match self {
            // Written back exactly as it was read.
            Item::Unreadable {
                schema_version,
                json,
                ..
            } => (*schema_version, json.clone()),
            // Cannot fail for these types (every map key is a string). If it ever did, the
            // empty text reads back as an `Unreadable` row instead of losing the thread.
            item => (
                ITEM_SCHEMA_VERSION,
                serde_json::to_string(item).unwrap_or_default(),
            ),
        }
    }

    /// Reads a row saved by [`Item::to_saved`]. Never fails: a row from another schema
    /// version, an unknown variant or broken JSON becomes [`Item::Unreadable`], so one odd row
    /// can never stop a thread from opening. Fields this version does not know are ignored.
    pub fn from_saved(schema_version: u32, json: &str) -> Item {
        let parsed = (schema_version == ITEM_SCHEMA_VERSION)
            .then(|| serde_json::from_str::<Item>(json).ok())
            .flatten();
        parsed.unwrap_or_else(|| Item::Unreadable {
            id: 0,
            schema_version,
            json: json.to_string(),
        })
    }

    fn set_id(&mut self, new_id: u64) {
        match self {
            Item::User { id, .. }
            | Item::Assistant { id, .. }
            | Item::Tool { id, .. }
            | Item::TurnEnd { id, .. }
            | Item::Error { id, .. }
            | Item::Stopped { id }
            | Item::FreshStart { id }
            | Item::Unreadable { id, .. } => *id = new_id,
        }
    }
}

/// For `skip_serializing_if`: a `false` flag is left out of the saved JSON.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// The state of one conversation. See the module docs.
#[derive(Debug, Clone)]
pub struct Transcript {
    items: Vec<Item>,
    presentation: crate::agent::AgentPresentation,
    usage: crate::thread_usage::ThreadUsage,
    rate_limit: Option<crate::agent::RateLimitEvent>,
    status: Status,
    /// The agent's currently reported background tasks.
    background_tasks: Vec<BackgroundTaskView>,
    session_id: Option<String>,
    model: Option<String>,
    /// The project directory, used to shorten file paths in tool summaries.
    cwd: Option<PathBuf>,
    /// Approvals waiting for an answer, oldest first.
    pending: VecDeque<crate::agent::ApprovalRequest>,
    next_id: u64,
    /// Live content key → row position.
    open_text: HashMap<ContentKey, usize>,
    /// When each tool call was first seen, to compute how long it took.
    tool_started: HashMap<String, Instant>,
    /// The last few stderr lines, shown if the process exits unexpectedly.
    stderr_tail: VecDeque<String>,
    /// The lowest index of `items` that changed since the last save (see
    /// [`Transcript::dirty_from`]). `None` when everything is saved.
    dirty_from: Option<usize>,
    /// Goes up with every change, so what is derived from the transcript can be cached.
    revision: u64,
    line: std::cell::RefCell<Option<(u64, Option<String>)>>,
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

impl Transcript {
    pub fn new() -> Self {
        Transcript {
            items: Vec::new(),
            presentation: Default::default(),
            usage: Default::default(),
            rate_limit: None,
            status: Status::Idle,
            background_tasks: Vec::new(),
            session_id: None,
            model: None,
            cwd: None,
            pending: VecDeque::new(),
            next_id: 1,
            open_text: HashMap::new(),
            tool_started: HashMap::new(),
            stderr_tail: VecDeque::new(),
            dirty_from: None,
            revision: 0,
            line: std::cell::RefCell::new(None),
        }
    }

    /// A number that changes whenever the transcript does (items, status, approvals, background
    /// tasks or working directory). Equal values mean nothing that is shown has changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// `compute()`'s result for the current [`Transcript::revision`], computed once per revision.
    pub(crate) fn cached_line(&self, compute: impl FnOnce() -> Option<String>) -> Option<String> {
        if let Some((revision, line)) = &*self.line.borrow()
            && *revision == self.revision
        {
            return line.clone();
        }
        let line = compute();
        *self.line.borrow_mut() = Some((self.revision, line.clone()));
        line
    }

    /// A transcript rebuilt from saved items and status (see `Thread::load`). Item ids are
    /// kept, so new items continue after the highest one; rows that could not be read get new
    /// ids after that. Call [`Transcript::normalize_after_relaunch`] next.
    pub fn from_saved(cwd: impl Into<PathBuf>, items: Vec<Item>, status: Status) -> Self {
        let mut transcript = Self::with_cwd(cwd);
        let highest = items.iter().map(Item::id).max().unwrap_or(0);
        transcript.next_id = highest + 1;
        transcript.items = items;
        let mut seen = std::collections::HashSet::new();
        for ix in 0..transcript.items.len() {
            let id = transcript.items[ix].id();
            // An unreadable row has no id (0), and a damaged file could repeat one; either
            // would confuse a UI that keys elements by id, so give those a new one.
            if id == 0 || !seen.insert(id) {
                let fresh = transcript.new_id();
                transcript.items[ix].set_id(fresh);
            }
        }
        transcript.status = status;
        transcript
    }

    /// Like [`Transcript::new`], but file paths inside `cwd` are shown relative to it right
    /// away (before the CLI reports its own working directory).
    pub fn with_cwd(cwd: impl Into<PathBuf>) -> Self {
        let mut transcript = Self::new();
        transcript.cwd = Some(cwd.into());
        transcript
    }

    /// Shows file paths relative to `cwd` from now on (a thread that moved into its worktree).
    pub fn set_cwd(&mut self, cwd: impl Into<PathBuf>) {
        self.revision += 1;
        self.cwd = Some(cwd.into());
    }

    pub(crate) fn set_presentation(&mut self, presentation: crate::agent::AgentPresentation) {
        self.revision += 1;
        self.presentation = presentation;
    }
    pub fn presentation(&self) -> &crate::agent::AgentPresentation {
        &self.presentation
    }

    pub fn usage(&self) -> &crate::thread_usage::ThreadUsage {
        &self.usage
    }
    pub fn rate_limit(&self) -> Option<&crate::agent::RateLimitEvent> {
        self.rate_limit.as_ref()
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn status(&self) -> Status {
        self.status
    }

    /// The Claude session id, known after the first turn starts. Pass it to `--resume`.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The folder tool summaries are shown relative to, once known.
    pub fn cwd(&self) -> Option<&std::path::Path> {
        self.cwd.as_deref()
    }

    /// The model the CLI reported for this session.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The approval to show now: the oldest unanswered one.
    pub fn pending_approval(&self) -> Option<&crate::agent::ApprovalRequest> {
        self.pending.front()
    }

    /// What claude keeps running in the background now (a dev server, say), oldest first.
    /// These keep running after the turn that started them, until they finish or claude stops.
    pub fn background_tasks(&self) -> &[BackgroundTaskView] {
        &self.background_tasks
    }

    /// Newjen stopped the claude running the background tasks, so they stopped with it: each
    /// one's tool card says so ([`BACKGROUND_STOPPED_NOTE`]) and the list is emptied.
    pub fn stop_background_tasks(&mut self) {
        self.revision += 1;
        for ix in 0..self.items.len() {
            if let Item::Tool {
                state,
                output,
                background,
                ..
            } = &mut self.items[ix]
                && *background
            {
                *background = false;
                *state = ToolState::Failed;
                *output = Some(BACKGROUND_STOPPED_NOTE.to_string());
                self.touch(ix);
            }
        }
        self.clear_background_tasks();
    }

    fn clear_background_tasks(&mut self) {
        self.background_tasks.clear();
        self.mark_background_tools();
    }

    /// The lowest item index that changed since the last [`Transcript::mark_saved`], or `None`
    /// if nothing did. Saving rewrites the rows from here on (see `Store::replace_items`).
    pub fn dirty_from(&self) -> Option<usize> {
        self.dirty_from
    }

    /// Records that every item up to now has been saved.
    pub fn mark_saved(&mut self) {
        self.dirty_from = None;
    }

    /// Records that the items from `position` on must be saved again, for example because the
    /// write that saved them failed (see `Thread::load` and `Store::write_errors`).
    pub fn mark_changed_from(&mut self, position: usize) {
        self.revision += 1;
        // Never past the end: saving from the end still clears any stale rows after it.
        self.touch(position.min(self.items.len()));
    }

    /// Adds the user's prompt and marks the conversation as working.
    pub fn push_user(&mut self, text: &str) {
        self.revision += 1;
        let id = self.new_id();
        self.push(Item::User {
            id,
            text: text.to_string(),
            unconfirmed: true,
        });
        self.status = Status::Working;
    }

    /// The user's last message, if claude never started its turn (see `Item::User`).
    pub fn unconfirmed_message(&self) -> Option<&str> {
        self.items
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::User {
                    text, unconfirmed, ..
                } => Some(unconfirmed.then_some(text.as_str())),
                _ => None,
            })
            .flatten()
    }

    /// A turn started (`system/init`), so claude has the user's last message.
    fn confirm_last_message(&mut self) {
        let Some(position) = self
            .items
            .iter()
            .rposition(|item| matches!(item, Item::User { .. }))
        else {
            return;
        };
        if let Item::User { unconfirmed, .. } = &mut self.items[position]
            && *unconfirmed
        {
            *unconfirmed = false;
            self.touch(position);
        }
    }

    fn unconfirm_last_message(&mut self) {
        let Some(position) = self
            .items
            .iter()
            .rposition(|item| matches!(item, Item::User { .. }))
        else {
            return;
        };
        if let Item::User { unconfirmed, .. } = &mut self.items[position]
            && !*unconfirmed
        {
            *unconfirmed = true;
            self.touch(position);
        }
    }

    /// Adds the quiet row saying the conversation starts over here (see [`Item::FreshStart`]).
    /// A failure before it was the old conversation's, so the thread is Stopped, not Failed.
    pub fn push_fresh_start(&mut self) {
        self.revision += 1;
        let id = self.new_id();
        self.push(Item::FreshStart { id });
        if self.status == Status::Failed {
            self.status = Status::Stopped;
        }
    }

    /// Adds the fresh-start row just before the user's last message, the one that starts the
    /// new conversation. Its id is a new one, so ids are unique but no longer in row order.
    pub fn insert_fresh_start_before_last_message(&mut self) {
        self.revision += 1;
        let Some(position) = self
            .items
            .iter()
            .rposition(|item| matches!(item, Item::User { .. }))
        else {
            return self.push_fresh_start();
        };
        let id = self.new_id();
        self.items.insert(position, Item::FreshStart { id });
        for at in self.open_text.values_mut().filter(|at| **at >= position) {
            *at += 1;
        }
        self.touch(position);
    }

    /// Ends the turn with an error the app found itself (for example claude could not be
    /// started again), the same way a failed turn ends.
    pub fn fail(&mut self, title: &str, detail: String) {
        self.revision += 1;
        self.finish_turn();
        self.close_unfinished_tools("The turn ended before this finished.");
        self.push_error(title, detail);
        self.status = Status::Failed;
    }

    /// Settles what an earlier run of Newjen left unfinished, because nothing of it is still
    /// running: Newjen quit (or crashed) in the middle of it. Applied when a thread is loaded,
    /// and at quit so the saved thread already looks like this.
    ///
    /// - An approval that was waiting expires ([`ToolState::Expired`]).
    /// - A tool call that was running is marked stopped ([`STOPPED_TOOL_NOTE`]).
    /// - A reply that was still streaming is marked interrupted.
    /// - If the thread was working or waiting, or a command still ran in the background, it
    ///   becomes `Stopped` and an [`Item::Stopped`] notice is added, with a Continue button.
    ///   Nothing restarts on its own.
    ///
    /// Running it twice changes nothing the second time.
    pub fn normalize_after_relaunch(&mut self) {
        self.revision += 1;
        let mut background_stopped = !self.background_tasks.is_empty();
        for ix in 0..self.items.len() {
            let changed = match &mut self.items[ix] {
                Item::Tool {
                    state,
                    output,
                    background,
                    ..
                } => {
                    let was_background = std::mem::take(background);
                    background_stopped |= was_background;
                    match state {
                        ToolState::Waiting => {
                            *state = ToolState::Expired;
                            output.get_or_insert_with(|| EXPIRED_NOTE.to_string());
                            true
                        }
                        ToolState::Running => {
                            // Shown like a call the user stopped (see `close_unfinished_tools`).
                            *state = ToolState::Failed;
                            output.get_or_insert_with(|| STOPPED_TOOL_NOTE.to_string());
                            true
                        }
                        _ => was_background,
                    }
                }
                Item::Assistant {
                    live, interrupted, ..
                } if *live => {
                    *live = false;
                    *interrupted = true;
                    true
                }
                _ => false,
            };
            if changed {
                self.touch(ix);
            }
        }
        self.open_text.clear();
        self.pending.clear();
        self.tool_started.clear();
        self.background_tasks.clear();
        if self.status.is_busy() || background_stopped {
            let id = self.new_id();
            self.push(Item::Stopped { id });
            self.status = Status::Stopped;
        }
    }

    /// Records the user's answer to an approval: removes it from the queue and updates the
    /// tool's state. Call this right after `Session::answer`.
    pub fn resolve_approval(
        &mut self,
        key: crate::agent::ApprovalKey,
        answer: crate::agent::ApprovalIntent,
    ) {
        self.revision += 1;
        let Some(position) = self.pending.iter().position(|request| request.key() == key) else {
            return; // Already answered or dropped; nothing to do.
        };
        let request = self.pending.remove(position).expect("position is valid");
        let tool_use_id = request.view().tool.id.clone();
        let new_state = if answer.is_deny() {
            ToolState::Denied
        } else {
            ToolState::Running
        };
        if let Some(Item::Tool { state, .. }) = self.find_tool_mut(&tool_use_id)
            && *state != ToolState::Denied
        {
            *state = new_state;
        }
        // A tool's time should be how long it ran, not how long the user took to answer:
        // restart its clock when allowed, and drop it when denied (it never runs).
        if answer.is_deny() {
            self.tool_started.remove(&tool_use_id);
        } else {
            self.tool_started.insert(tool_use_id, Instant::now());
        }
        if self.pending.is_empty() && self.status == Status::Waiting {
            self.status = Status::Working;
        }
    }

    /// Applies one atomic batch from the agent adapter.
    pub fn apply(&mut self, batch: &AgentBatch) {
        self.revision += 1;
        for event in &batch.events {
            self.apply_event(event);
        }
    }

    /// A prompt or answer could not be written to claude, so the turn can't go on.
    fn apply_write_failed(&mut self, detail: &str) {
        // Already failed (usually a "Claude exited" error for the same crash): one error is
        // enough. If this one comes first, `apply_exit` skips its own error the same way.
        if self.status == Status::Failed {
            return;
        }
        self.finish_turn();
        self.close_unfinished_tools(self.presentation.unreachable_note);
        self.push_error(
            self.presentation.send_failed,
            format!("{}: {detail}", self.presentation.send_detail_prefix),
        );
        self.status = Status::Failed;
    }

    /// claude ignored a stop request, so the session ended the process. Failed (not Stopped),
    /// so the UI offers "Try again", which resumes the conversation in a fresh claude.
    fn apply_interrupt_timed_out(&mut self) {
        if self.status == Status::Failed {
            return; // One error is enough (see `apply_write_failed`).
        }
        self.finish_turn();
        self.close_unfinished_tools("Stopped");
        self.push_error(
            self.presentation.interrupt_title,
            self.presentation.interrupt_detail.to_string(),
        );
        self.status = Status::Failed;
    }

    fn apply_event(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ContextUsed { .. } | AgentEvent::TurnUsage(_) => self.usage.apply(event),
            AgentEvent::RateLimit(limit) => self.rate_limit = Some(limit.clone()),
            AgentEvent::ProviderUsage(_) => {}
            AgentEvent::ToolOutput { id, update } => {
                if let Some(Item::Tool { output, .. }) = self.find_tool_mut(id) {
                    match update {
                        TextUpdate::Append(text) => output.get_or_insert_default().push_str(text),
                        TextUpdate::Replace(text) => *output = Some(text.clone()),
                    }
                }
            }
            AgentEvent::ToolDuration { id, duration_ms } => {
                if let Some(Item::Tool { took, .. }) = self.find_tool_mut(id) {
                    *took = Some(Duration::from_millis(*duration_ms));
                }
            }
            AgentEvent::ApprovalAnswered { key, intent } => self.resolve_approval(*key, *intent),
            AgentEvent::ApprovalUpdated(request) => {
                if let Some(current) = self.pending.iter_mut().find(|r| r.key() == request.key()) {
                    *current = request.clone();
                }
            }
            AgentEvent::TurnActive => {
                if !self.status.is_busy() {
                    self.status = Status::Working;
                }
            }
            AgentEvent::UserConfirmed => self.confirm_last_message(),
            AgentEvent::ConversationEstablished(id) => self.session_id = Some(id.clone()),
            AgentEvent::ModelReported(model) => self.model = Some(model.clone()),
            AgentEvent::WorkingDirectoryReported(cwd) => self.cwd = Some(cwd.clone()),
            AgentEvent::Text { key, update } => match update {
                TextUpdate::Append(text) => {
                    let position = match self.open_text.get(key) {
                        Some(position) => *position,
                        None => {
                            let position = self.push_assistant(String::new());
                            self.open_text.insert(*key, position);
                            position
                        }
                    };
                    if let Some(Item::Assistant { text: current, .. }) = self.item_mut(position) {
                        current.push_str(text);
                    }
                }
                TextUpdate::Replace(text) => {
                    if let Some(position) = self.open_text.remove(key) {
                        if let Some(Item::Assistant { text: current, .. }) = self.item_mut(position)
                        {
                            *current = text.clone();
                        }
                    } else if !text.is_empty() {
                        self.push_assistant(text.clone());
                    }
                }
            },
            AgentEvent::ReplyInterrupted => {
                let latest = self
                    .items
                    .iter()
                    .rposition(|item| matches!(item, Item::Assistant { live: true, .. }));
                if let Some(Item::Assistant { interrupted, .. }) =
                    latest.and_then(|position| self.item_mut(position))
                {
                    *interrupted = true;
                }
            }
            AgentEvent::ToolUpsert(tool) => self.add_tool(tool),
            AgentEvent::ToolFinished {
                id,
                outcome,
                output,
            } => {
                let took = self.tool_started.get(id).map(|started| started.elapsed());
                if let Some(Item::Tool {
                    state,
                    output: current,
                    took: tool_took,
                    ..
                }) = self.find_tool_mut(id)
                {
                    *state = match outcome {
                        ToolOutcome::Denied => ToolState::Denied,
                        _ if *state == ToolState::Denied => ToolState::Denied,
                        ToolOutcome::Failed => ToolState::Failed,
                        ToolOutcome::Completed => ToolState::Done,
                    };
                    *current = Some(output.clone());
                    *tool_took = took;
                }
            }
            AgentEvent::ApprovalResolved(key) => {
                let tool_id = self
                    .pending
                    .iter()
                    .find(|r| r.key() == *key)
                    .map(|r| r.view().tool.id.clone());
                self.pending.retain(|request| request.key() != *key);
                if let Some(id) = tool_id
                    && !self.pending.iter().any(|r| r.view().tool.id == id)
                    && let Some(Item::Tool { state, .. }) = self.find_tool_mut(&id)
                    && *state == ToolState::Waiting
                {
                    *state = ToolState::Running;
                }
                if self.pending.is_empty() && self.status == Status::Waiting {
                    self.status = Status::Working;
                }
            }
            AgentEvent::ApprovalRequested(request) => self.apply_permission_request(request),
            AgentEvent::ApprovalWithdrawn { key, note } => self.apply_cancel(*key, note),
            AgentEvent::BackgroundTasks(tasks) => {
                self.background_tasks = tasks.clone();
                self.mark_background_tools();
            }
            AgentEvent::TurnFinished {
                outcome,
                duration_ms,
                cost_usd,
                forget_conversation,
                queued,
            } => {
                self.finish_turn();
                match outcome {
                    TurnOutcome::Stopped => {
                        self.close_unfinished_tools("Stopped");
                        self.push_turn_end(*duration_ms, *cost_usd, true);
                        self.status = Status::Stopped;
                    }
                    TurnOutcome::Failed { title, detail } => {
                        if *forget_conversation {
                            self.session_id = None;
                        }
                        self.close_unfinished_tools("The turn ended before this finished.");
                        let detail = if let Some(crate::agent::RateLimitEvent::Rejected(limit)) =
                            &self.rate_limit
                        {
                            let resets = limit
                                .windows
                                .iter()
                                .map(|window| match window.resets_at_unix_seconds {
                                    Some(reset) => {
                                        format!("{} resets at Unix time {reset}", window.label)
                                    }
                                    None => format!("{}: reset time unavailable", window.label),
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            format!("{detail}\n{resets}\nNothing resumes automatically.")
                        } else {
                            detail.clone()
                        };
                        self.push_error(title, detail);
                        self.status = Status::Failed;
                    }
                    TurnOutcome::Completed => {
                        self.close_unfinished_tools("The turn ended before this finished.");
                        self.push_turn_end(*duration_ms, *cost_usd, false);
                        self.status = Status::Done;
                    }
                }
                if *queued {
                    self.status = Status::Working;
                    self.unconfirm_last_message();
                }
            }
            AgentEvent::Stderr(line) => {
                self.stderr_tail.push_back(line.clone());
                while self.stderr_tail.len() > STDERR_LINES_KEPT {
                    self.stderr_tail.pop_front();
                }
            }
            AgentEvent::WriteFailed(detail) => self.apply_write_failed(detail),
            AgentEvent::InterruptTimedOut => self.apply_interrupt_timed_out(),
            AgentEvent::Exited { code, requested } => self.apply_exit(*code, *requested),
        }
    }

    fn apply_permission_request(&mut self, request: &crate::agent::ApprovalRequest) {
        let tool_use_id = request.view().tool.id.clone();
        if let Some(Item::Tool { state, .. }) = self.find_tool_mut(&tool_use_id)
            && *state != ToolState::Denied
        {
            *state = ToolState::Waiting;
        }
        self.pending.push_back(request.clone());
        self.status = Status::Waiting;
    }

    /// The CLI withdrew an approval request: it no longer waits for the answer, so drop the
    /// request from the queue and close its tool card.
    fn apply_cancel(&mut self, key: crate::agent::ApprovalKey, note: &str) {
        let Some(position) = self.pending.iter().position(|request| request.key() == key) else {
            return; // Already answered, or not an approval.
        };
        let request = self.pending.remove(position).expect("position is valid");
        let tool_use_id = request.view().tool.id.clone();
        if let Some(Item::Tool { state, output, .. }) = self.find_tool_mut(&tool_use_id) {
            *state = ToolState::Failed;
            *output = Some(note.to_string());
        }
        self.tool_started.remove(&tool_use_id);
        if self.pending.is_empty() && self.status == Status::Waiting {
            self.status = Status::Working;
        }
    }

    /// `requested` is true when we stopped the process ourselves (`Session::shutdown`), so the
    /// exit is not an error.
    fn apply_exit(&mut self, code: Option<i32>, requested: bool) {
        self.finish_turn();
        // Its background tasks were in its process group, so they are gone too.
        if requested {
            self.stop_background_tasks();
            // Stopped mid-turn (for example a restart): the turn will never finish.
            if matches!(self.status, Status::Working | Status::Waiting) {
                self.close_unfinished_tools("Stopped");
                self.status = Status::Stopped;
            }
            return;
        }
        self.clear_background_tasks();
        if self.status == Status::Failed {
            return;
        }
        self.close_unfinished_tools(self.presentation.exited_tool_note);
        let mut detail = match code {
            Some(code) => format!(
                "The {} process exited with code {code}.",
                self.presentation.label.to_lowercase()
            ),
            None => format!(
                "The {} process was killed by a signal.",
                self.presentation.label.to_lowercase()
            ),
        };
        if !self.stderr_tail.is_empty() {
            detail.push_str("\n\n");
            detail.push_str(&Vec::from(self.stderr_tail.clone()).join("\n"));
        }
        self.push_error(self.presentation.exit_title, detail);
        self.status = Status::Failed;
    }

    /// End-of-turn bookkeeping shared by `result` and process exit: text stops being live,
    /// and unanswered approvals are dropped (the CLI no longer waits for them).
    fn finish_turn(&mut self) {
        for ix in 0..self.items.len() {
            // Only items that really change are marked, so a save after a long conversation
            // rewrites this turn's rows, not every reply since the start.
            if let Item::Assistant { live, .. } = &mut self.items[ix]
                && *live
            {
                *live = false;
                self.touch(ix);
            }
        }
        self.open_text.clear();
        self.pending.clear();
    }

    /// Tools still running or waiting when a turn ends will never finish: mark them failed.
    fn close_unfinished_tools(&mut self, note: &str) {
        for ix in 0..self.items.len() {
            if let Item::Tool { state, output, .. } = &mut self.items[ix]
                && matches!(state, ToolState::Running | ToolState::Waiting)
            {
                *state = ToolState::Failed;
                if output.is_none() {
                    *output = Some(note.to_string());
                }
                self.touch(ix);
            }
        }
    }

    fn add_tool(&mut self, tool: &ToolDescriptor) {
        let tool_use_id = &tool.id;
        if let Some(Item::Tool {
            name,
            display_name,
            summary,
            input,
            ..
        }) = self.find_tool_mut(tool_use_id)
        {
            *name = tool.name.clone();
            *display_name = tool.display_name.clone();
            *summary = tool.summary.clone();
            *input = tool.arguments.clone();
            return;
        }
        let id = self.new_id();
        self.tool_started
            .insert(tool_use_id.to_string(), Instant::now());
        self.push(Item::Tool {
            id,
            tool_use_id: tool_use_id.to_string(),
            name: tool.name.clone(),
            display_name: tool.display_name.clone(),
            summary: tool.summary.clone(),
            input: tool.arguments.clone(),
            state: ToolState::Running,
            output: None,
            took: None,
            background: false,
        });
    }

    /// Flags each tool card whose task is in the background list now, and unflags the rest.
    fn mark_background_tools(&mut self) {
        let running: Vec<&str> = self
            .background_tasks
            .iter()
            .filter_map(|task| task.tool.as_deref())
            .collect();
        let changed: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(ix, item)| match item {
                Item::Tool {
                    tool_use_id,
                    background,
                    ..
                } if *background != running.contains(&tool_use_id.as_str()) => Some(ix),
                _ => None,
            })
            .collect();
        for ix in changed {
            if let Some(Item::Tool { background, .. }) = self.item_mut(ix) {
                *background = !*background;
            }
        }
    }

    /// Where the tool card for `wanted_id` is, if there is one.
    fn tool_position(&self, wanted_id: &str) -> Option<usize> {
        self.items.iter().position(
            |item| matches!(item, Item::Tool { tool_use_id, .. } if tool_use_id == wanted_id),
        )
    }

    /// The tool card for `wanted_id`, to change it (so it is marked changed).
    fn find_tool_mut(&mut self, wanted_id: &str) -> Option<&mut Item> {
        let position = self.tool_position(wanted_id)?;
        self.item_mut(position)
    }

    /// Item `position`, to change it: marks it changed for the next save.
    fn item_mut(&mut self, position: usize) -> Option<&mut Item> {
        if position < self.items.len() {
            self.touch(position);
        }
        self.items.get_mut(position)
    }

    /// Adds an item at the end and marks it changed for the next save.
    fn push(&mut self, item: Item) {
        self.items.push(item);
        self.touch(self.items.len() - 1);
    }

    /// Remembers that item `position` changed since the last save.
    fn touch(&mut self, position: usize) {
        self.revision += 1;
        self.dirty_from = Some(self.dirty_from.map_or(position, |from| from.min(position)));
    }

    /// Adds a live assistant item and returns its position.
    fn push_assistant(&mut self, text: String) -> usize {
        let id = self.new_id();
        self.push(Item::Assistant {
            id,
            text,
            live: true,
            interrupted: false,
        });
        self.items.len() - 1
    }

    fn push_turn_end(&mut self, duration_ms: Option<u64>, cost_usd: Option<f64>, stopped: bool) {
        let id = self.new_id();
        self.push(Item::TurnEnd {
            id,
            duration_ms,
            cost_usd,
            stopped,
        });
    }

    fn push_error(&mut self, title: &str, detail: String) {
        let id = self.new_id();
        self.push(Item::Error {
            id,
            title: title.to_string(),
            detail,
        });
    }

    fn new_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Incoming;
    use crate::providers::claude::events::ClaudeEvents;
    use crate::session::SessionEvent;
    use serde_json::json;

    /// One of every kind of item, as a saved thread could hold them.
    fn every_item() -> Vec<Item> {
        vec![
            Item::User {
                id: 1,
                text: "hi \"there\"\n🦀".to_string(),
                unconfirmed: true,
            },
            Item::Assistant {
                id: 2,
                text: "hello".to_string(),
                live: false,
                interrupted: true,
            },
            Item::Tool {
                id: 3,
                tool_use_id: "toolu_1".to_string(),
                name: "Bash".to_string(),
                display_name: "Bash".to_string(),
                summary: "touch a.txt".to_string(),
                input: json!({"command": "touch a.txt", "nested": [1, null, {"x": true}]}),
                state: ToolState::Expired,
                output: Some("out".to_string()),
                took: Some(Duration::from_millis(1234)),
                background: true,
            },
            Item::TurnEnd {
                id: 4,
                duration_ms: Some(963),
                cost_usd: Some(0.0034),
                stopped: false,
            },
            Item::Error {
                id: 5,
                title: "Claude stopped".to_string(),
                detail: "details".to_string(),
            },
            Item::Stopped { id: 6 },
            Item::FreshStart { id: 7 },
        ]
    }
    #[test]
    fn every_item_survives_saving_and_loading() {
        for item in every_item() {
            let (version, json) = item.to_saved();
            assert_eq!(version, ITEM_SCHEMA_VERSION);
            assert_eq!(Item::from_saved(version, &json), item, "{json}");
        }
    }

    #[test]
    fn the_background_flag_is_saved_only_when_set() {
        // A tool row saved before the flag existed still reads, as not in the background.
        let old = r#"{"type":"tool","id":3,"tool_use_id":"t1","name":"Bash",
            "display_name":"Bash","summary":"ls","input":{},"state":"done","output":null,
            "took":null}"#;
        let item = Item::from_saved(ITEM_SCHEMA_VERSION, old);
        assert!(matches!(
            item,
            Item::Tool {
                background: false,
                ..
            }
        ));
        // And an unset flag is not written, so older versions read new rows the same.
        assert!(!item.to_saved().1.contains("background"));
    }

    #[test]
    fn a_message_is_unconfirmed_until_its_turn_starts() {
        let mut codec = ClaudeEvents::default();
        // A message saved before the flag existed counts as received.
        let old = Item::from_saved(ITEM_SCHEMA_VERSION, r#"{"type":"user","id":1,"text":"a"}"#);
        let mut transcript = Transcript::from_saved("/tmp/p", vec![old], Status::Stopped);
        assert_eq!(transcript.unconfirmed_message(), None);

        transcript.push_user("b");
        assert_eq!(transcript.unconfirmed_message(), Some("b"));
        transcript.mark_saved();
        let init = Incoming::parse(r#"{"type":"system","subtype":"init","session_id":"s"}"#);
        transcript.apply(&codec.translate(&SessionEvent::Line(init), None));
        assert_eq!(transcript.unconfirmed_message(), None);
        assert_eq!(
            transcript.dirty_from(),
            Some(1),
            "saved at the init checkpoint"
        );
        assert!(!transcript.items()[1].to_saved().1.contains("unconfirmed"));
    }

    #[test]
    fn rows_that_cannot_be_read_become_placeholders_and_are_kept_as_they_were() {
        let rows = [
            (1, "{not json"),
            (1, r#"{"type":"hologram","id":9}"#),
            (1, r#"{"type":"tool","id":9,"state":"teleporting"}"#),
            (99, r#"{"type":"user","id":9,"text":"from the future"}"#),
        ];
        for (version, json) in rows {
            let item = Item::from_saved(version, json);
            assert!(matches!(item, Item::Unreadable { .. }), "{json}");
            // Saving it again writes back exactly what was read.
            assert_eq!(item.to_saved(), (version, json.to_string()));
        }
        // A field this version does not know is ignored, not an error.
        let item = Item::from_saved(1, r#"{"type":"user","id":3,"text":"hi","mood":"happy"}"#);
        assert_eq!(
            item,
            Item::User {
                id: 3,
                text: "hi".to_string(),
                unconfirmed: false,
            }
        );
    }

    #[test]
    fn loaded_items_keep_their_ids_and_new_ones_come_after() {
        let items = vec![
            Item::User {
                id: 4,
                text: "a".to_string(),
                unconfirmed: false,
            },
            Item::from_saved(1, "broken"),
            // A repeated id (a damaged file) gets a new one.
            Item::Stopped { id: 4 },
        ];
        let mut transcript = Transcript::from_saved("/tmp/p", items, Status::Done);
        let ids: Vec<u64> = transcript.items().iter().map(Item::id).collect();
        assert_eq!(ids, [4, 5, 6]);
        assert_eq!(transcript.dirty_from(), None, "loading is not a change");
        transcript.push_user("b");
        assert_eq!(transcript.items()[3].id(), 7);
        assert_eq!(transcript.dirty_from(), Some(3));
    }

    #[test]
    fn statuses_are_saved_by_name() {
        for status in [
            Status::Idle,
            Status::Working,
            Status::Waiting,
            Status::Done,
            Status::Stopped,
            Status::Failed,
        ] {
            assert_eq!(Status::from_saved(status.as_str()), status);
        }
        assert_eq!(Status::from_saved("napping"), Status::Idle);
    }

    fn line(codec: &mut ClaudeEvents, value: Value) -> AgentBatch {
        codec.translate(&SessionEvent::Line(Incoming::from_value(value)), None)
    }

    #[test]
    fn only_changed_rows_are_marked_for_saving() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = Transcript::new();
        assert_eq!(transcript.dirty_from(), None);
        transcript.push_user("one");
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "text", "text": "first"}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false}),
        ));
        assert_eq!(transcript.dirty_from(), Some(0));
        transcript.mark_saved();
        assert_eq!(transcript.dirty_from(), None);

        // The second turn's end must not mark the first turn's reply (it is no longer live).
        transcript.push_user("two");
        assert_eq!(transcript.dirty_from(), Some(3));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false}),
        ));
        assert_eq!(transcript.dirty_from(), Some(3));
        transcript.mark_saved();

        // A streamed word changes the reply being written, so it is marked (but the thread
        // only saves it at the next checkpoint).
        transcript.push_user("three");
        transcript.mark_saved();
        transcript.apply(&line(
            &mut codec,
            json!({"type": "stream_event", "api_message_id": "m3",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "Hel"}}}),
        ));
        let reply = transcript.items().len() - 1;
        assert_eq!(transcript.dirty_from(), Some(reply));
        transcript.mark_saved();
        transcript.apply(&line(
            &mut codec,
            json!({"type": "stream_event", "api_message_id": "m3",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "lo"}}}),
        ));
        assert_eq!(transcript.dirty_from(), Some(reply));
    }

    #[test]
    fn a_tool_result_marks_its_card_even_when_it_is_far_up() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "ls"}}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "text", "text": "while it runs"}]}}),
        ));
        transcript.mark_saved();
        transcript.apply(&line(
            &mut codec,
            json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}}),
        ));
        assert_eq!(transcript.dirty_from(), Some(1));
    }

    /// A transcript as Newjen left it when it quit: a reply streaming, one tool running, one
    /// waiting for an approval.
    fn interrupted_transcript() -> Transcript {
        let mut codec = ClaudeEvents::default();
        let mut transcript = Transcript::with_cwd("/tmp/project");
        transcript.push_user("go");
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "sleep 9"}}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "control_request", "request_id": "r2",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "t2",
                        "input": {"command": "touch a.txt"}}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "stream_event", "api_message_id": "m2",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "Working on"}}}),
        ));
        assert_eq!(transcript.status(), Status::Waiting);
        transcript
    }

    #[test]
    fn relaunch_expires_approvals_and_stops_what_was_running() {
        let saved = interrupted_transcript();
        let mut transcript =
            Transcript::from_saved("/tmp/project", saved.items().to_vec(), saved.status());
        transcript.normalize_after_relaunch();

        assert_eq!(transcript.status(), Status::Stopped);
        assert!(transcript.pending_approval().is_none());
        let items = transcript.items();
        assert!(matches!(
            &items[1],
            Item::Tool { state: ToolState::Failed, output: Some(note), .. }
                if note == STOPPED_TOOL_NOTE
        ));
        assert!(matches!(
            &items[2],
            Item::Tool { state: ToolState::Expired, output: Some(note), .. }
                if note == EXPIRED_NOTE
        ));
        assert!(matches!(
            &items[3],
            Item::Assistant {
                live: false,
                interrupted: true,
                ..
            }
        ));
        assert!(matches!(items[4], Item::Stopped { .. }));
        assert_eq!(items.len(), 5);
        assert_eq!(transcript.dirty_from(), Some(1));

        // Settling twice (at quit, then again at the next launch) adds nothing.
        transcript.mark_saved();
        transcript.normalize_after_relaunch();
        assert_eq!(transcript.items().len(), 5);
        assert_eq!(transcript.dirty_from(), None);
    }

    #[test]
    fn relaunch_stops_a_thread_that_was_working_without_an_approval() {
        let mut codec = ClaudeEvents::default();
        // The common case: claude was running a tool and streaming a reply, nothing to answer.
        let mut working = Transcript::with_cwd("/tmp/project");
        working.push_user("go");
        working.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "sleep 9"}}]}}),
        ));
        working.apply(&line(
            &mut codec,
            json!({"type": "stream_event", "api_message_id": "m2",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "Still"}}}),
        ));
        assert_eq!(working.status(), Status::Working);
        assert!(working.pending_approval().is_none());

        let mut transcript =
            Transcript::from_saved("/tmp/project", working.items().to_vec(), working.status());
        transcript.normalize_after_relaunch();
        assert_eq!(transcript.status(), Status::Stopped);
        let items = transcript.items();
        assert_eq!(items.len(), 4);
        assert!(matches!(
            &items[1],
            Item::Tool { state: ToolState::Failed, output: Some(note), .. }
                if note == STOPPED_TOOL_NOTE
        ));
        assert!(matches!(
            &items[2],
            Item::Assistant { live: false, interrupted: true, text, .. } if text == "Still"
        ));
        assert!(matches!(items[3], Item::Stopped { .. }));
        assert_eq!(transcript.dirty_from(), Some(1));
    }

    #[test]
    fn rows_can_be_marked_for_saving_again() {
        let mut transcript = Transcript::new();
        transcript.push_user("one");
        transcript.push_user("two");
        transcript.mark_saved();
        transcript.mark_changed_from(1);
        assert_eq!(transcript.dirty_from(), Some(1));
        // An earlier change wins; a later one does not move it.
        transcript.mark_changed_from(5);
        assert_eq!(transcript.dirty_from(), Some(1));
        transcript.mark_saved();
        // Past the end means "from the end": nothing to write, but nothing stale is kept.
        transcript.mark_changed_from(9);
        assert_eq!(transcript.dirty_from(), Some(2));
    }

    #[test]
    fn relaunch_leaves_a_finished_thread_alone() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false}),
        ));
        let items = transcript.items().to_vec();
        let mut loaded = Transcript::from_saved("/tmp/p", items.clone(), Status::Done);
        loaded.normalize_after_relaunch();
        assert_eq!(loaded.items(), items.as_slice());
        assert_eq!(loaded.status(), Status::Done);
        assert_eq!(loaded.dirty_from(), None);
    }

    fn tasks_changed(codec: &mut ClaudeEvents, ids: &[&str]) -> AgentBatch {
        let tasks: Vec<Value> = ids
            .iter()
            .map(
                |id| json!({"task_id": id, "task_type": "local_bash", "description": "Dev server"}),
            )
            .collect();
        line(
            codec,
            json!({"type": "system", "subtype": "background_tasks_changed", "tasks": tasks}),
        )
    }

    /// A finished turn whose Bash call `t1` left task `b1` running in the background.
    fn turn_with_a_background_task() -> Transcript {
        let mut codec = ClaudeEvents::default();
        let mut transcript = Transcript::with_cwd("/tmp/project");
        transcript.push_user("start the server");
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "npm run dev", "run_in_background": true}}]}}),
        ));
        transcript.apply(&tasks_changed(&mut codec, &["b1"]));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "system", "subtype": "task_started",
            "task_id": "b1", "tool_use_id": "t1", "is_backgrounded": true}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1",
             "content": "Command running in background with ID: b1."}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m2",
            "content": [{"type": "text", "text": "started"}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false}),
        ));
        transcript
    }

    fn is_background(item: &Item) -> bool {
        matches!(
            item,
            Item::Tool {
                background: true,
                ..
            }
        )
    }

    #[test]
    fn a_background_task_outlives_its_turn_and_is_flagged_on_its_call() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = turn_with_a_background_task();
        assert_eq!(transcript.status(), Status::Done);
        assert_eq!(transcript.background_tasks().len(), 1);
        assert_eq!(transcript.background_tasks()[0].description, "Dev server");
        assert!(matches!(
            &transcript.items()[1],
            Item::Tool {
                state: ToolState::Done,
                background: true,
                ..
            }
        ));

        // The list is the whole truth: once the task leaves it, its call is no longer flagged.
        transcript.mark_saved();
        transcript.apply(&tasks_changed(&mut codec, &[]));
        assert!(transcript.background_tasks().is_empty());
        assert!(!is_background(&transcript.items()[1]));
        assert_eq!(transcript.dirty_from(), Some(1));
    }

    #[test]
    fn claude_can_start_a_turn_by_itself() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = turn_with_a_background_task();
        transcript.apply(&line(
            &mut codec,
            json!({"type": "system", "subtype": "task_notification",
            "task_id": "b1", "tool_use_id": "t1", "status": "completed"}),
        ));
        transcript.apply(&tasks_changed(&mut codec, &[]));
        assert_eq!(transcript.status(), Status::Done, "not a turn yet");

        transcript.apply(&line(
            &mut codec,
            json!({"type": "system", "subtype": "init",
            "session_id": "s"}),
        ));
        assert_eq!(transcript.status(), Status::Working);
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m3",
            "content": [{"type": "text", "text": "The server stopped."}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false, "duration_ms": 1851}),
        ));
        assert_eq!(transcript.status(), Status::Done);
        let users = transcript
            .items()
            .iter()
            .filter(|item| matches!(item, Item::User { .. }))
            .count();
        assert_eq!(users, 1, "no user message is made up");
        assert!(matches!(
            &transcript.items()[transcript.items().len() - 2..],
            [
                Item::Assistant { text, live: false, .. },
                Item::TurnEnd { duration_ms: Some(1851), stopped: false, .. }
            ] if text == "The server stopped."
        ));

        // A streamed word between turns starts one too (if `init` was missed).
        transcript.apply(&line(
            &mut codec,
            json!({"type": "stream_event", "api_message_id": "m4",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "Also"}}}),
        ));
        assert_eq!(transcript.status(), Status::Working);
    }

    #[test]
    fn lines_outside_a_turn_do_not_start_one() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = turn_with_a_background_task();
        for quiet in [
            json!({"type": "system", "subtype": "status", "status": "requesting"}),
            json!({"type": "system", "subtype": "hook_started", "hook_name": "Stop"}),
            json!({"type": "system", "subtype": "task_updated", "task_id": "b1",
                   "patch": {"status": "completed"}}),
            json!({"type": "rate_limit_event", "rate_limit_info": {}}),
            json!({"type": "assistant", "parent_tool_use_id": "t9",
                   "message": {"id": "m9", "content": [{"type": "text", "text": "sub"}]}}),
        ] {
            transcript.apply(&line(&mut codec, quiet));
            assert_eq!(transcript.status(), Status::Done);
        }
    }

    #[test]
    fn relaunch_stops_a_thread_whose_command_ran_in_the_background() {
        let running = turn_with_a_background_task();
        let mut transcript =
            Transcript::from_saved("/tmp/project", running.items().to_vec(), running.status());
        assert!(
            transcript.background_tasks().is_empty(),
            "no claude runs after a relaunch"
        );
        transcript.normalize_after_relaunch();
        assert_eq!(transcript.status(), Status::Stopped);
        assert!(matches!(
            transcript.items().last(),
            Some(Item::Stopped { .. })
        ));
        assert!(!is_background(&transcript.items()[1]));
        assert!(matches!(
            &transcript.items()[1],
            Item::Tool {
                state: ToolState::Done,
                ..
            }
        ));

        // Settling twice adds nothing.
        transcript.mark_saved();
        let count = transcript.items().len();
        transcript.normalize_after_relaunch();
        assert_eq!(transcript.items().len(), count);
        assert_eq!(transcript.dirty_from(), None);
    }

    #[test]
    fn a_background_task_dies_with_its_claude() {
        // Newjen stopped claude ("Try again"): the card says the command stopped with it.
        let mut transcript = turn_with_a_background_task();
        transcript.mark_saved();
        transcript.apply(&ClaudeEvents::default().translate(
            &SessionEvent::Exited {
                code: Some(0),
                requested: true,
            },
            None,
        ));
        assert!(transcript.background_tasks().is_empty());
        assert!(matches!(
            &transcript.items()[1],
            Item::Tool { state: ToolState::Failed, output: Some(note), background: false, .. }
                if note == BACKGROUND_STOPPED_NOTE
        ));
        assert_eq!(transcript.dirty_from(), Some(1));
        assert_eq!(transcript.status(), Status::Done);
        // So a relaunch after that has nothing to stop.
        let mut loaded = Transcript::from_saved(
            "/tmp/project",
            transcript.items().to_vec(),
            transcript.status(),
        );
        loaded.normalize_after_relaunch();
        assert_eq!(loaded.status(), Status::Done);
        assert_eq!(loaded.items(), transcript.items());

        // claude exited by itself: its "Claude exited" error is what the user sees.
        let mut crashed = turn_with_a_background_task();
        crashed.apply(&ClaudeEvents::default().translate(
            &SessionEvent::Exited {
                code: Some(1),
                requested: false,
            },
            None,
        ));
        assert!(crashed.background_tasks().is_empty());
        assert!(matches!(
            &crashed.items()[1],
            Item::Tool {
                state: ToolState::Done,
                background: false,
                ..
            }
        ));
        assert_eq!(crashed.status(), Status::Failed);
    }

    #[test]
    fn a_message_queued_behind_claudes_own_turn_keeps_the_turn_going() {
        let mut codec = ClaudeEvents::default();
        let mut transcript = turn_with_a_background_task();
        transcript.push_user("What next?");
        transcript.apply(&tasks_changed(&mut codec, &[]));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "system", "subtype": "init",
            "session_id": "s"}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m3",
            "content": [{"type": "text", "text": "The server stopped."}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false, "queued_turn_count": 1,
            "origin": {"kind": "task-notification"}}),
        ));
        assert_eq!(
            transcript.status(),
            Status::Working,
            "the message runs next"
        );
        assert_eq!(
            transcript.unconfirmed_message(),
            Some("What next?"),
            "claude's own init did not start the queued message's turn"
        );
        assert!(matches!(
            transcript.items().last(),
            Some(Item::TurnEnd { .. })
        ));

        transcript.apply(&line(
            &mut codec,
            json!({"type": "system", "subtype": "init",
            "session_id": "s"}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "assistant", "message": {"id": "m4",
            "content": [{"type": "text", "text": "Here is what is next."}]}}),
        ));
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "success",
            "is_error": false, "queued_turn_count": 0}),
        ));
        assert_eq!(transcript.status(), Status::Done);
        assert_eq!(transcript.unconfirmed_message(), None);

        // A stop ends the turn even with a message queued; claude starts that one by itself.
        transcript.push_user("And then?");
        transcript.apply(&line(
            &mut codec,
            json!({"type": "result", "subtype": "error_during_execution",
            "is_error": true, "terminal_reason": "aborted_streaming", "queued_turn_count": 1}),
        ));
        assert_eq!(transcript.status(), Status::Stopped);
    }
}
