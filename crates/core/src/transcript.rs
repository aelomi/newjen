//! The UI-agnostic state of one conversation.
//!
//! A [`Transcript`] is fed every [`SessionEvent`] the session produces (see [`Transcript::apply`])
//! and turns the raw protocol into a simple list of [`Item`]s a UI can render: user prompts,
//! assistant replies, tool calls, end-of-turn captions and errors. It also tracks the overall
//! [`Status`] and the queue of approvals waiting for an answer.
//!
//! It never talks to the process itself. The app calls `Session::answer` and then
//! [`Transcript::resolve_approval`] with the same answer.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::protocol::{
    AssistantMessage, ContentBlock, Delta, Incoming, PermissionAnswer, PermissionRequest,
    ResultMessage, StreamEvent, StreamEventMessage, SystemMessage, UserMessage,
};
use crate::session::SessionEvent;

/// How many stderr lines to keep for the "Claude exited" error.
const STDERR_LINES_KEPT: usize = 20;

/// What the conversation is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// The state of one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    /// Waiting for the user's approval.
    Waiting,
    Done,
    /// The user said no.
    Denied,
    /// The tool ran and reported an error, or the turn ended before it finished.
    Failed,
}

/// One row of the conversation. Every item has an `id` that never changes, so a UI can use it
/// as a stable element id.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    User {
        id: u64,
        text: String,
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
}

impl Item {
    /// The item's stable id.
    pub fn id(&self) -> u64 {
        match self {
            Item::User { id, .. }
            | Item::Assistant { id, .. }
            | Item::Tool { id, .. }
            | Item::TurnEnd { id, .. }
            | Item::Error { id, .. } => *id,
        }
    }
}

/// The state of one conversation. See the module docs.
#[derive(Debug, Clone)]
pub struct Transcript {
    items: Vec<Item>,
    status: Status,
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
        }
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

    /// Adds the user's prompt and marks the conversation as working.
    pub fn push_user(&mut self, text: &str) {
        let id = self.new_id();
        self.items.push(Item::User {
            id,
            text: text.to_string(),
        });
        self.status = Status::Working;
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

    fn apply_incoming(&mut self, incoming: &Incoming) {
        match incoming {
            Incoming::System(message) => self.apply_system(message),
            Incoming::StreamEvent(message) => self.apply_stream_event(message),
            Incoming::Assistant(message) => self.apply_assistant(message),
            Incoming::User(message) => self.apply_user(message),
            Incoming::PermissionRequest(request) => self.apply_permission_request(request),
            Incoming::Result(result) => self.apply_result(result),
            // Hooks, rate limits, answers to our own requests and unknown lines change nothing.
            Incoming::ControlRequest(_)
            | Incoming::ControlResponse(_)
            | Incoming::RateLimit(_)
            | Incoming::Unknown(_) => {}
        }
    }

    fn apply_system(&mut self, message: &SystemMessage) {
        if message.subtype != "init" {
            return;
        }
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

    /// Live text: each `text_delta` grows the assistant item for its (message, block).
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
        if let Some(Item::Assistant { text: current, .. }) = self.items.get_mut(position) {
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
                                self.items.get_mut(position)
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
            if let Some(Item::Assistant { interrupted, .. }) = self
                .items
                .iter_mut()
                .rev()
                .find(|item| matches!(item, Item::Assistant { live: true, .. }))
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
        if self.find_tool_mut(&tool_use_id).is_none() {
            self.add_tool(&tool_use_id, &request.tool_name, &request.input);
        }
        if let Some(Item::Tool { state, .. }) = self.find_tool_mut(&tool_use_id) {
            *state = ToolState::Waiting;
        }
        self.pending.push_back(request.clone());
        self.status = Status::Waiting;
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
            if is_missing_conversation(result) {
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
    }

    /// `requested` is true when we stopped the process ourselves (`Session::shutdown`), so the
    /// exit is not an error.
    fn apply_exit(&mut self, code: Option<i32>, requested: bool) {
        self.finish_turn();
        if requested {
            // Stopped mid-turn (for example a restart): the turn will never finish.
            if matches!(self.status, Status::Working | Status::Waiting) {
                self.close_unfinished_tools("Stopped");
                self.status = Status::Stopped;
            }
            return;
        }
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
        for item in &mut self.items {
            if let Item::Assistant { live, .. } = item {
                *live = false;
            }
        }
        self.open_text.clear();
        self.pending.clear();
    }

    /// Tools still running or waiting when a turn ends will never finish: mark them failed.
    fn close_unfinished_tools(&mut self, note: &str) {
        for item in &mut self.items {
            if let Item::Tool { state, output, .. } = item
                && matches!(state, ToolState::Running | ToolState::Waiting)
            {
                *state = ToolState::Failed;
                if output.is_none() {
                    *output = Some(note.to_string());
                }
            }
        }
    }

    fn add_tool(&mut self, tool_use_id: &str, name: &str, input: &Value) {
        if self.find_tool_mut(tool_use_id).is_some() {
            return;
        }
        let id = self.new_id();
        self.tool_started
            .insert(tool_use_id.to_string(), Instant::now());
        self.items.push(Item::Tool {
            id,
            tool_use_id: tool_use_id.to_string(),
            name: name.to_string(),
            display_name: display_name(name),
            summary: summarize(name, input, self.cwd.as_deref()),
            input: input.clone(),
            state: ToolState::Running,
            output: None,
            took: None,
        });
    }

    fn find_tool_mut(&mut self, wanted_id: &str) -> Option<&mut Item> {
        self.items
            .iter_mut()
            .find(|item| matches!(item, Item::Tool { tool_use_id, .. } if tool_use_id == wanted_id))
    }

    /// Adds a live assistant item and returns its position.
    fn push_assistant(&mut self, text: String) -> usize {
        let id = self.new_id();
        self.items.push(Item::Assistant {
            id,
            text,
            live: true,
            interrupted: false,
        });
        self.items.len() - 1
    }

    fn push_turn_end(&mut self, duration_ms: Option<u64>, cost_usd: Option<f64>, stopped: bool) {
        let id = self.new_id();
        self.items.push(Item::TurnEnd {
            id,
            duration_ms,
            cost_usd,
            stopped,
        });
    }

    fn push_error(&mut self, title: &str, detail: String) {
        let id = self.new_id();
        self.items.push(Item::Error {
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

/// True when a failed result says the `--resume` session does not exist, e.g.
/// `"No conversation found with session ID: …"` (seen live in `errors`).
fn is_missing_conversation(result: &ResultMessage) -> bool {
    let says_so = |text: &str| text.contains("No conversation found");
    result.error_lines().iter().any(|line| says_so(line))
        || result.result.as_deref().is_some_and(says_so)
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
}
