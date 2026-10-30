//! The UI-agnostic state of one conversation.
//!
//! A [`Transcript`] is fed every [`SessionEvent`] the session produces (see [`Transcript::apply`])
//! and turns the raw protocol into a simple list of [`Item`]s a UI can render: user prompts,
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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::{
    AssistantMessage, BackgroundTask, ContentBlock, Delta, Incoming, PermissionAnswer,
    PermissionRequest, ResultMessage, StreamEvent, StreamEventMessage, SystemMessage, UserMessage,
};
use crate::session::SessionEvent;

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
    status: Status,
    /// What claude keeps running in the background now, as its last `background_tasks_changed`
    /// said. Empty when no claude runs.
    background_tasks: Vec<BackgroundTask>,
    /// Which tool call started each task (task id → tool_use_id), from `task_started`.
    task_tools: HashMap<String, String>,
    session_id: Option<String>,
    model: Option<String>,
    /// The project directory, used to shorten file paths in tool summaries.
    cwd: Option<PathBuf>,
    /// Approvals waiting for an answer, oldest first.
    pending: VecDeque<PermissionRequest>,
    next_id: u64,
    /// Assistant text items still receiving streamed text, keyed by
    /// (API message id, content block index) → position in `items`.
    open_text: HashMap<(String, u64), usize>,
    /// When each tool call was first seen, to compute how long it took.
    tool_started: HashMap<String, Instant>,
    /// The previous result's `total_cost_usd`, for the fallback per-turn cost.
    previous_total_cost: Option<f64>,
    /// The text of an API-error message (e.g. "Not logged in"), shown in the turn's error.
    api_error_text: Option<String>,
    /// The last few stderr lines, shown if the process exits unexpectedly.
    stderr_tail: VecDeque<String>,
    /// The lowest index of `items` that changed since the last save (see
    /// [`Transcript::dirty_from`]). `None` when everything is saved.
    dirty_from: Option<usize>,
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
            status: Status::Idle,
            background_tasks: Vec::new(),
            task_tools: HashMap::new(),
            session_id: None,
            model: None,
            cwd: None,
            pending: VecDeque::new(),
            next_id: 1,
            open_text: HashMap::new(),
            tool_started: HashMap::new(),
            previous_total_cost: None,
            api_error_text: None,
            stderr_tail: VecDeque::new(),
            dirty_from: None,
        }
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

    /// The model the CLI reported for this session.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The approval to show now: the oldest unanswered one.
    pub fn pending_approval(&self) -> Option<&PermissionRequest> {
        self.pending.front()
    }

    /// What claude keeps running in the background now (a dev server, say), oldest first.
    /// These keep running after the turn that started them, until they finish or claude stops.
    pub fn background_tasks(&self) -> &[BackgroundTask] {
        &self.background_tasks
    }

    /// Newjen stopped the claude running the background tasks, so they stopped with it: each
    /// one's tool card says so ([`BACKGROUND_STOPPED_NOTE`]) and the list is emptied.
    pub fn stop_background_tasks(&mut self) {
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
        self.task_tools.clear();
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
        // Never past the end: saving from the end still clears any stale rows after it.
        self.touch(position.min(self.items.len()));
    }

    /// Adds the user's prompt and marks the conversation as working.
    pub fn push_user(&mut self, text: &str) {
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
        let id = self.new_id();
        self.push(Item::FreshStart { id });
        if self.status == Status::Failed {
            self.status = Status::Stopped;
        }
    }

    /// Adds the fresh-start row just before the user's last message, the one that starts the
    /// new conversation. Its id is a new one, so ids are unique but no longer in row order.
    pub fn insert_fresh_start_before_last_message(&mut self) {
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
        self.task_tools.clear();
        if self.status.is_busy() || background_stopped {
            let id = self.new_id();
            self.push(Item::Stopped { id });
            self.status = Status::Stopped;
        }
    }

    /// Records the user's answer to an approval: removes it from the queue and updates the
    /// tool's state. Call this right after `Session::answer`.
    pub fn resolve_approval(&mut self, request_id: &str, answer: PermissionAnswer) {
        let Some(position) = self
            .pending
            .iter()
            .position(|request| request.request_id == request_id)
        else {
            return; // Already answered or dropped; nothing to do.
        };
        let request = self.pending.remove(position).expect("position is valid");
        let tool_use_id = request.tool_use_id.unwrap_or(request.request_id);
        let new_state = if answer.is_deny() {
            ToolState::Denied
        } else {
            ToolState::Running
        };
        if let Some(Item::Tool { state, .. }) = self.find_tool_mut(&tool_use_id) {
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

    /// Updates the state from one session event. This is the whole state machine.
    pub fn apply(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Line(incoming) => self.apply_incoming(incoming),
            SessionEvent::Stderr(line) => {
                self.stderr_tail.push_back(line.clone());
                while self.stderr_tail.len() > STDERR_LINES_KEPT {
                    self.stderr_tail.pop_front();
                }
            }
            SessionEvent::WriteFailed(detail) => self.apply_write_failed(detail),
            SessionEvent::InterruptTimedOut => self.apply_interrupt_timed_out(),
            SessionEvent::Exited { code, requested } => self.apply_exit(*code, *requested),
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
        self.close_unfinished_tools("Claude could not be reached.");
        self.push_error(
            "Could not reach Claude",
            format!("Newjen could not send the message to the claude process: {detail}"),
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
            "Claude didn't stop, so Newjen ended the session",
            "Claude did not confirm the stop request in time. Try again to continue the \
             conversation."
                .to_string(),
        );
        self.status = Status::Failed;
    }

    fn apply_incoming(&mut self, incoming: &Incoming) {
        if !self.status.is_busy() && starts_turn(incoming) {
            self.status = Status::Working;
        }
        match incoming {
            Incoming::System(message) => self.apply_system(message),
            Incoming::StreamEvent(message) => self.apply_stream_event(message),
            Incoming::Assistant(message) => self.apply_assistant(message),
            Incoming::User(message) => self.apply_user(message),
            Incoming::PermissionRequest(request) => self.apply_permission_request(request),
            Incoming::Result(result) => self.apply_result(result),
            Incoming::ControlCancelRequest { request_id } => self.apply_cancel(request_id),
            // Hooks, rate limits, answers to our own requests and unknown lines change nothing.
            Incoming::ControlRequest(_)
            | Incoming::ControlResponse(_)
            | Incoming::RateLimit(_)
            | Incoming::Unknown(_) => {}
        }
    }

    fn apply_system(&mut self, message: &SystemMessage) {
        if let Some(tasks) = message.background_tasks() {
            self.background_tasks = tasks.to_vec();
            self.mark_background_tools();
            return;
        }
        if message.subtype == "task_started"
            && let (Some(task_id), Some(tool_use_id)) = (&message.task_id, &message.tool_use_id)
        {
            self.task_tools.insert(task_id.clone(), tool_use_id.clone());
            self.mark_background_tools();
            return;
        }
        if message.subtype != "init" {
            return;
        }
        self.confirm_last_message();
        if let Some(session_id) = &message.session_id {
            self.session_id = Some(session_id.clone());
        }
        if let Some(model) = &message.model {
            self.model = Some(model.clone());
        }
        if let Some(cwd) = &message.cwd {
            self.cwd = Some(PathBuf::from(cwd));
        }
    }

    /// Live text: each `text_delta` grows the assistant item for its (message, block). The item
    /// is marked changed, but the thread does not save on these (see `Thread::apply`).
    fn apply_stream_event(&mut self, message: &StreamEventMessage) {
        if message.parent_tool_use_id.is_some() {
            return; // A subagent's stream; slice 1 only shows the main conversation.
        }
        let StreamEvent::ContentBlockDelta {
            index,
            delta: Delta::Text(text),
        } = &message.event
        else {
            return;
        };
        let Some(message_id) = &message.api_message_id else {
            return;
        };
        let key = (message_id.clone(), *index);
        let position = match self.open_text.get(&key) {
            Some(position) => *position,
            None => {
                let position = self.push_assistant(String::new());
                self.open_text.insert(key, position);
                position
            }
        };
        if let Some(Item::Assistant { text: current, .. }) = self.item_mut(position) {
            current.push_str(text);
        }
    }

    /// A finished content block: final text, a tool call, or an API error message.
    fn apply_assistant(&mut self, message: &AssistantMessage) {
        if message.parent_tool_use_id.is_some() {
            return; // A subagent's message; not shown in slice 1.
        }
        if message.is_api_error_message {
            // Shown as the turn's error when the result arrives, not as a chat bubble.
            let text = message
                .message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.api_error_text = Some(text);
            return;
        }

        let message_id = &message.message.id;
        for block in &message.message.content {
            match block {
                ContentBlock::Text { text } => {
                    // The full text replaces whatever the deltas built, so they can never drift.
                    match self.take_open_text(message_id) {
                        Some(position) => {
                            if let Some(Item::Assistant { text: current, .. }) =
                                self.item_mut(position)
                            {
                                *current = text.clone();
                            }
                        }
                        None if !text.is_empty() => {
                            self.push_assistant(text.clone());
                        }
                        None => {}
                    }
                }
                ContentBlock::ToolUse { id, name, input } => self.add_tool(id, name, input),
                // Thinking arrives with empty text in Claude Code; nothing to show.
                _ => {}
            }
        }

        if message.aborted {
            // The user stopped this reply. Close any text still open for it and mark this turn's
            // latest assistant text as interrupted. Only `live` items belong to the current turn
            // (`finish_turn` clears `live` at every result), so if the user stopped Claude before
            // any text arrived, an earlier turn's reply is left alone.
            while self.take_open_text(message_id).is_some() {}
            let latest_live = self
                .items
                .iter()
                .rposition(|item| matches!(item, Item::Assistant { live: true, .. }));
            if let Some(Item::Assistant { interrupted, .. }) =
                latest_live.and_then(|position| self.item_mut(position))
            {
                *interrupted = true;
            }
        }
    }

    /// Removes and returns the most recent open text item for `message_id`.
    ///
    /// This pairs a finished text block with the newest open item for its message. That is
    /// right because the CLI prints one content block per `assistant` line. If one line ever
    /// carried several text blocks while several were open, they would be paired in the wrong
    /// order.
    fn take_open_text(&mut self, message_id: &str) -> Option<usize> {
        let key = self
            .open_text
            .iter()
            .filter(|((id, _), _)| id == message_id)
            .max_by_key(|(_, position)| **position)
            .map(|(key, _)| key.clone())?;
        self.open_text.remove(&key)
    }

    fn apply_user(&mut self, message: &UserMessage) {
        // Subagent lines (with `parent_tool_use_id`) are handled too: this never adds items, it
        // only settles tool cards that already exist. A subagent's approval request carries no
        // parent id, so its card is shown, and its result must settle it here.
        //
        // Only tool results matter. Text blocks here are synthetic notes such as
        // "[Request interrupted by user]" and are never shown as a user bubble.
        for block in message.message.content.blocks() {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = block
            else {
                continue;
            };
            let rejected = message
                .tool_result_meta
                .iter()
                .any(|meta| meta.id == *tool_use_id && meta.was_rejected());
            let took = self
                .tool_started
                .get(tool_use_id)
                .map(|started| started.elapsed());
            // The CLI has settled this call, so any approval still queued for it is stale
            // (for example after stdin closed while it was pending).
            self.pending
                .retain(|request| request.tool_use_id.as_deref() != Some(tool_use_id));
            if self.pending.is_empty() && self.status == Status::Waiting {
                self.status = Status::Working;
            }

            if let Some(Item::Tool {
                state,
                output,
                took: tool_took,
                ..
            }) = self.find_tool_mut(tool_use_id)
            {
                // A denied call stays Denied, even though its result is marked as an error.
                *state = if rejected || *state == ToolState::Denied {
                    ToolState::Denied
                } else if *is_error {
                    ToolState::Failed
                } else {
                    ToolState::Done
                };
                *output = Some(ContentBlock::tool_result_text(content));
                *tool_took = took;
            }
        }
    }

    fn apply_permission_request(&mut self, request: &PermissionRequest) {
        let tool_use_id = request
            .tool_use_id
            .clone()
            .unwrap_or_else(|| request.request_id.clone());
        if self.tool_position(&tool_use_id).is_none() {
            self.add_tool(&tool_use_id, &request.tool_name, &request.input);
        }
        if let Some(Item::Tool { state, .. }) = self.find_tool_mut(&tool_use_id) {
            *state = ToolState::Waiting;
        }
        self.pending.push_back(request.clone());
        self.status = Status::Waiting;
    }

    /// The CLI withdrew an approval request: it no longer waits for the answer, so drop the
    /// request from the queue and close its tool card.
    fn apply_cancel(&mut self, request_id: &str) {
        let Some(position) = self
            .pending
            .iter()
            .position(|request| request.request_id == request_id)
        else {
            return; // Already answered, or not an approval.
        };
        let request = self.pending.remove(position).expect("position is valid");
        let tool_use_id = request.tool_use_id.unwrap_or(request.request_id);
        if let Some(Item::Tool { state, output, .. }) = self.find_tool_mut(&tool_use_id) {
            *state = ToolState::Failed;
            *output = Some("Cancelled by Claude".to_string());
        }
        self.tool_started.remove(&tool_use_id);
        if self.pending.is_empty() && self.status == Status::Waiting {
            self.status = Status::Working;
        }
    }

    fn apply_result(&mut self, result: &ResultMessage) {
        self.finish_turn();

        // Prefer the per-turn usage. Only if it is missing, fall back to the difference of
        // the running totals (wrong on the first turn after --resume, hence the preference).
        let cost_usd = result.turn_cost_usd().or_else(|| {
            let total = result.total_cost_usd?;
            Some(total - self.previous_total_cost.unwrap_or(0.0))
        });
        if result.total_cost_usd.is_some() {
            self.previous_total_cost = result.total_cost_usd;
        }
        let api_error_text = self.api_error_text.take();

        if result.was_stopped() {
            self.close_unfinished_tools("Stopped");
            self.push_turn_end(result.duration_ms, cost_usd, true);
            self.status = Status::Stopped;
        } else if result.is_error {
            if result.is_missing_conversation() {
                // `--resume` named a session the CLI does not have. Forget it, so the next
                // restart starts a fresh session instead of failing the same way forever.
                self.session_id = None;
            }
            self.close_unfinished_tools("The turn ended before this finished.");
            let detail = api_error_text
                .filter(|text| !text.is_empty())
                .or_else(|| result.result.clone().filter(|text| !text.is_empty()))
                .unwrap_or_else(|| {
                    let lines = result.error_lines();
                    if lines.is_empty() {
                        "Claude reported an error but gave no details.".to_string()
                    } else {
                        lines.join("\n")
                    }
                });
            self.push_error("Claude stopped", detail);
            self.status = Status::Failed;
        } else {
            // Normally every tool has its result by now; this is a backstop so no card spins
            // forever after the turn is over.
            self.close_unfinished_tools("The turn ended before this finished.");
            self.push_turn_end(result.duration_ms, cost_usd, false);
            self.status = Status::Done;
        }
        // The user's message arrived while claude ran a turn of its own; it runs next.
        if !result.was_stopped() && result.another_turn_follows() {
            self.status = Status::Working;
            // The init that just ended confirmed the queued message too early.
            self.unconfirm_last_message();
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
        self.close_unfinished_tools("Claude exited before this finished.");
        let mut detail = match code {
            Some(code) => format!("The claude process exited with code {code}."),
            None => "The claude process was killed by a signal.".to_string(),
        };
        if !self.stderr_tail.is_empty() {
            detail.push_str("\n\n");
            detail.push_str(&Vec::from(self.stderr_tail.clone()).join("\n"));
        }
        self.push_error("Claude exited", detail);
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

    fn add_tool(&mut self, tool_use_id: &str, name: &str, input: &Value) {
        if self.tool_position(tool_use_id).is_some() {
            return;
        }
        let id = self.new_id();
        self.tool_started
            .insert(tool_use_id.to_string(), Instant::now());
        self.push(Item::Tool {
            id,
            tool_use_id: tool_use_id.to_string(),
            name: name.to_string(),
            display_name: display_name(name),
            summary: summarize(name, input, self.cwd.as_deref()),
            input: input.clone(),
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
            .filter_map(|task| self.task_tools.get(&task.task_id).map(String::as_str))
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

/// Lines that only come while a turn runs: its `init`, and the main conversation's replies.
/// One between turns means claude started a turn by itself, as it does when a background task
/// finishes: no user message, just `init` and a reply. It is a turn like any other.
///
/// Assumes claude sends `init` only to start a turn, never at spawn or after a result (true in
/// every fixture; checked in `tests/replay.rs`).
fn starts_turn(incoming: &Incoming) -> bool {
    match incoming {
        Incoming::System(message) => message.subtype == "init",
        Incoming::Assistant(message) => message.parent_tool_use_id.is_none(),
        Incoming::StreamEvent(message) => message.parent_tool_use_id.is_none(),
        _ => false,
    }
}

/// A friendlier tool name: MCP tools arrive as `mcp__<server>__<tool>` and become
/// `<server> · <tool>`. Other names are returned unchanged.
pub fn display_name(tool_name: &str) -> String {
    if let Some(rest) = tool_name.strip_prefix("mcp__")
        && let Some((server, tool)) = rest.split_once("__")
    {
        return format!("{server} · {tool}");
    }
    tool_name.to_string()
}

/// A one-line summary of a tool call for its card, e.g. the Bash command or the file it edits.
/// File paths inside `cwd` are shown relative to it.
pub fn summarize(tool_name: &str, input: &Value, cwd: Option<&Path>) -> String {
    let field = |name: &str| input.get(name).and_then(Value::as_str);
    let summary = match tool_name {
        "Bash" => field("command").map(|command| command.lines().next().unwrap_or("").to_string()),
        "Read" | "Edit" | "Write" | "MultiEdit" => {
            field("file_path").map(|path| relative(path, cwd))
        }
        "NotebookEdit" => field("notebook_path")
            .or_else(|| field("file_path"))
            .map(|path| relative(path, cwd)),
        "Glob" | "Grep" => field("pattern").map(str::to_string),
        "WebFetch" => field("url").map(str::to_string),
        "WebSearch" => field("query").map(str::to_string),
        "Task" | "Agent" => field("description").map(str::to_string),
        _ => None,
    };
    summary
        .or_else(|| first_string_field(input))
        .unwrap_or_default()
}

/// The first string value in a JSON object, as a last-resort summary.
fn first_string_field(input: &Value) -> Option<String> {
    let object = input.as_object()?;
    object
        .values()
        .find_map(Value::as_str)
        .map(|text| text.lines().next().unwrap_or("").to_string())
}

/// `path` relative to `cwd` when it is inside it; otherwise unchanged.
fn relative(path: &str, cwd: Option<&Path>) -> String {
    let Some(cwd) = cwd else {
        return path.to_string();
    };
    match Path::new(path).strip_prefix(cwd) {
        Ok(inside) if !inside.as_os_str().is_empty() => inside.display().to_string(),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn display_name_shortens_mcp_tools() {
        assert_eq!(
            display_name("mcp__linear__list_issues"),
            "linear · list_issues"
        );
        assert_eq!(display_name("Bash"), "Bash");
        assert_eq!(display_name("mcp__broken"), "mcp__broken");
    }

    #[test]
    fn summarize_common_tools() {
        let cwd = Path::new("/tmp/project");
        assert_eq!(
            summarize("Bash", &json!({"command": "ls -la\necho hi"}), None),
            "ls -la"
        );
        assert_eq!(
            summarize(
                "Edit",
                &json!({"file_path": "/tmp/project/src/main.rs"}),
                Some(cwd)
            ),
            "src/main.rs"
        );
        assert_eq!(
            summarize("Read", &json!({"file_path": "/etc/hosts"}), Some(cwd)),
            "/etc/hosts"
        );
        assert_eq!(
            summarize("Grep", &json!({"pattern": "fn main"}), None),
            "fn main"
        );
        assert_eq!(
            summarize("WebFetch", &json!({"url": "https://example.com"}), None),
            "https://example.com"
        );
        assert_eq!(
            summarize("WebSearch", &json!({"query": "rust"}), None),
            "rust"
        );
        assert_eq!(
            summarize("Task", &json!({"description": "Explore"}), None),
            "Explore"
        );
        assert_eq!(
            summarize("SomethingElse", &json!({"n": 1, "a": "x"}), None),
            "x"
        );
        assert_eq!(summarize("SomethingElse", &json!({}), None), "");
    }

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
        // A message saved before the flag existed counts as received.
        let old = Item::from_saved(ITEM_SCHEMA_VERSION, r#"{"type":"user","id":1,"text":"a"}"#);
        let mut transcript = Transcript::from_saved("/tmp/p", vec![old], Status::Stopped);
        assert_eq!(transcript.unconfirmed_message(), None);

        transcript.push_user("b");
        assert_eq!(transcript.unconfirmed_message(), Some("b"));
        transcript.mark_saved();
        let init = Incoming::parse(r#"{"type":"system","subtype":"init","session_id":"s"}"#);
        transcript.apply(&SessionEvent::Line(init));
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

    fn line(value: Value) -> SessionEvent {
        SessionEvent::Line(Incoming::from_value(value))
    }

    #[test]
    fn only_changed_rows_are_marked_for_saving() {
        let mut transcript = Transcript::new();
        assert_eq!(transcript.dirty_from(), None);
        transcript.push_user("one");
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "text", "text": "first"}]}})));
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false})));
        assert_eq!(transcript.dirty_from(), Some(0));
        transcript.mark_saved();
        assert_eq!(transcript.dirty_from(), None);

        // The second turn's end must not mark the first turn's reply (it is no longer live).
        transcript.push_user("two");
        assert_eq!(transcript.dirty_from(), Some(3));
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false})));
        assert_eq!(transcript.dirty_from(), Some(3));
        transcript.mark_saved();

        // A streamed word changes the reply being written, so it is marked (but the thread
        // only saves it at the next checkpoint).
        transcript.push_user("three");
        transcript.mark_saved();
        transcript.apply(&line(
            json!({"type": "stream_event", "api_message_id": "m3",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "Hel"}}}),
        ));
        let reply = transcript.items().len() - 1;
        assert_eq!(transcript.dirty_from(), Some(reply));
        transcript.mark_saved();
        transcript.apply(&line(
            json!({"type": "stream_event", "api_message_id": "m3",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "lo"}}}),
        ));
        assert_eq!(transcript.dirty_from(), Some(reply));
    }

    #[test]
    fn a_tool_result_marks_its_card_even_when_it_is_far_up() {
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "ls"}}]}})));
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "text", "text": "while it runs"}]}})));
        transcript.mark_saved();
        transcript.apply(&line(json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}})));
        assert_eq!(transcript.dirty_from(), Some(1));
    }

    /// A transcript as Newjen left it when it quit: a reply streaming, one tool running, one
    /// waiting for an approval.
    fn interrupted_transcript() -> Transcript {
        let mut transcript = Transcript::with_cwd("/tmp/project");
        transcript.push_user("go");
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "sleep 9"}}]}})));
        transcript.apply(&line(json!({"type": "control_request", "request_id": "r2",
            "request": {"subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "t2",
                        "input": {"command": "touch a.txt"}}})));
        transcript.apply(&line(
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
        // The common case: claude was running a tool and streaming a reply, nothing to answer.
        let mut working = Transcript::with_cwd("/tmp/project");
        working.push_user("go");
        working.apply(&line(json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "sleep 9"}}]}})));
        working.apply(&line(
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
        let mut transcript = Transcript::new();
        transcript.push_user("go");
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false})));
        let items = transcript.items().to_vec();
        let mut loaded = Transcript::from_saved("/tmp/p", items.clone(), Status::Done);
        loaded.normalize_after_relaunch();
        assert_eq!(loaded.items(), items.as_slice());
        assert_eq!(loaded.status(), Status::Done);
        assert_eq!(loaded.dirty_from(), None);
    }

    fn tasks_changed(ids: &[&str]) -> SessionEvent {
        let tasks: Vec<Value> = ids
            .iter()
            .map(
                |id| json!({"task_id": id, "task_type": "local_bash", "description": "Dev server"}),
            )
            .collect();
        line(json!({"type": "system", "subtype": "background_tasks_changed", "tasks": tasks}))
    }

    /// A finished turn whose Bash call `t1` left task `b1` running in the background.
    fn turn_with_a_background_task() -> Transcript {
        let mut transcript = Transcript::with_cwd("/tmp/project");
        transcript.push_user("start the server");
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m1",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash",
                         "input": {"command": "npm run dev", "run_in_background": true}}]}})));
        transcript.apply(&tasks_changed(&["b1"]));
        transcript.apply(&line(json!({"type": "system", "subtype": "task_started",
            "task_id": "b1", "tool_use_id": "t1", "is_backgrounded": true})));
        transcript.apply(&line(json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "t1",
             "content": "Command running in background with ID: b1."}]}})));
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m2",
            "content": [{"type": "text", "text": "started"}]}})));
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false})));
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
        transcript.apply(&tasks_changed(&[]));
        assert!(transcript.background_tasks().is_empty());
        assert!(!is_background(&transcript.items()[1]));
        assert_eq!(transcript.dirty_from(), Some(1));
    }

    #[test]
    fn claude_can_start_a_turn_by_itself() {
        let mut transcript = turn_with_a_background_task();
        transcript.apply(&line(
            json!({"type": "system", "subtype": "task_notification",
            "task_id": "b1", "tool_use_id": "t1", "status": "completed"}),
        ));
        transcript.apply(&tasks_changed(&[]));
        assert_eq!(transcript.status(), Status::Done, "not a turn yet");

        transcript.apply(&line(json!({"type": "system", "subtype": "init",
            "session_id": "s"})));
        assert_eq!(transcript.status(), Status::Working);
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m3",
            "content": [{"type": "text", "text": "The server stopped."}]}})));
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false, "duration_ms": 1851})));
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
            json!({"type": "stream_event", "api_message_id": "m4",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": "Also"}}}),
        ));
        assert_eq!(transcript.status(), Status::Working);
    }

    #[test]
    fn lines_outside_a_turn_do_not_start_one() {
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
            transcript.apply(&line(quiet));
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
        transcript.apply(&SessionEvent::Exited {
            code: Some(0),
            requested: true,
        });
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
        crashed.apply(&SessionEvent::Exited {
            code: Some(1),
            requested: false,
        });
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
        let mut transcript = turn_with_a_background_task();
        transcript.push_user("What next?");
        transcript.apply(&tasks_changed(&[]));
        transcript.apply(&line(json!({"type": "system", "subtype": "init",
            "session_id": "s"})));
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m3",
            "content": [{"type": "text", "text": "The server stopped."}]}})));
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false, "queued_turn_count": 1,
            "origin": {"kind": "task-notification"}})));
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

        transcript.apply(&line(json!({"type": "system", "subtype": "init",
            "session_id": "s"})));
        transcript.apply(&line(json!({"type": "assistant", "message": {"id": "m4",
            "content": [{"type": "text", "text": "Here is what is next."}]}})));
        transcript.apply(&line(json!({"type": "result", "subtype": "success",
            "is_error": false, "queued_turn_count": 0})));
        assert_eq!(transcript.status(), Status::Done);
        assert_eq!(transcript.unconfirmed_message(), None);

        // A stop ends the turn even with a message queued; claude starts that one by itself.
        transcript.push_user("And then?");
        transcript.apply(&line(
            json!({"type": "result", "subtype": "error_during_execution",
            "is_error": true, "terminal_reason": "aborted_streaming", "queued_turn_count": 1}),
        ));
        assert_eq!(transcript.status(), Status::Stopped);
    }
}
