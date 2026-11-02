//! Wire types for the Claude Code CLI's `stream-json` protocol, plus builders for the
//! messages Newjen writes to its stdin.
//!
//! The CLI prints one JSON object per line on stdout. [`Incoming::parse`] turns one such line
//! into a Rust value. Parsing is deliberately *lenient*: the CLI adds fields and message types
//! often, so unknown fields are ignored and unknown message types become
//! [`Incoming::Unknown`] instead of an error. Never add `deny_unknown_fields` here.
//!
//! See `docs/PLAN.md` and the fixtures in `crates/core/tests/fixtures/` for real examples.

use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

/// The message Newjen sends back to the model when the user denies a tool call.
/// The CLI requires a message on every deny; without one it reports an "invalid permission
/// result" to the model, which then retries the same tool.
pub const DENY_MESSAGE: &str = "The user denied this tool call in Newjen.";

// ---------------------------------------------------------------------------------------------
// Incoming messages (CLI stdout → Newjen)
// ---------------------------------------------------------------------------------------------

/// One line of CLI stdout, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// `type: "system"`: init, hooks, status, thinking token counts, and more.
    System(SystemMessage),
    /// `type: "assistant"`: one finished content block (text, thinking or tool_use).
    Assistant(AssistantMessage),
    /// `type: "user"`: tool results, plus synthetic notes like "[Request interrupted by user]".
    User(UserMessage),
    /// `type: "stream_event"`: partial-message events (live text deltas).
    StreamEvent(StreamEventMessage),
    /// `type: "control_request"` with subtype `can_use_tool`: the CLI asks for approval.
    PermissionRequest(PermissionRequest),
    /// Any other `type: "control_request"` from the CLI.
    ControlRequest(ControlRequest),
    /// `type: "control_response"`: the CLI's answer to one of our control requests.
    ControlResponse(ControlResponse),
    /// `type: "control_cancel_request"`: the CLI withdraws a control request it sent earlier
    /// (for example an approval it no longer needs). Holds that request's id.
    ControlCancelRequest { request_id: String },
    /// `type: "rate_limit_event"`: usage-limit info, kept raw.
    RateLimit(Value),
    /// `type: "result"`: the end of a turn.
    Result(ResultMessage),
    /// Anything we do not understand (new message types, or a line that is not JSON at all,
    /// which is kept as `Value::String`). Tolerated, never an error.
    Unknown(Value),
}

impl Incoming {
    /// Decodes one stdout line. This never fails: anything unexpected becomes `Unknown`.
    pub fn parse(line: &str) -> Incoming {
        match serde_json::from_str::<Value>(line) {
            Ok(value) => Incoming::from_value(value),
            Err(_) => Incoming::Unknown(Value::String(line.to_string())),
        }
    }

    /// Decodes an already-parsed JSON object. Falls back to `Unknown` when the `type` is new
    /// or the shape does not match what we expect.
    pub fn from_value(value: Value) -> Incoming {
        let message_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let parsed = match message_type {
            "system" => parse_as(&value).map(Incoming::System),
            "assistant" => parse_as(&value).map(Incoming::Assistant),
            "user" => parse_as(&value).map(Incoming::User),
            "stream_event" => parse_as(&value).map(Incoming::StreamEvent),
            "control_request" => parse_control_request(&value),
            "control_response" => parse_as(&value).map(Incoming::ControlResponse),
            // Shape from the Python Agent SDK (`query.py`, `control_cancel_request`).
            "control_cancel_request" => value.get("request_id").and_then(Value::as_str).map(|id| {
                Incoming::ControlCancelRequest {
                    request_id: id.to_string(),
                }
            }),
            "rate_limit_event" => Some(Incoming::RateLimit(value.clone())),
            "result" => parse_as(&value).map(Incoming::Result),
            _ => None,
        };
        parsed.unwrap_or(Incoming::Unknown(value))
    }
}

/// Tries to deserialize `value` into `T`, returning `None` if the shape does not match.
fn parse_as<T: DeserializeOwned>(value: &Value) -> Option<T> {
    T::deserialize(value).ok()
}

/// For `#[serde(deserialize_with = "lenient")]` on fields the state machine relies on.
///
/// `#[serde(default)]` only covers a *missing* field. A field that is present but `null` or of
/// an unexpected type (say `duration_ms: 12.5`) would otherwise fail the whole line, turning it
/// into `Unknown`; for a `result` line that means the turn never ends. With this helper such a
/// field just gets its default value instead.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + Default,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

fn parse_control_request(value: &Value) -> Option<Incoming> {
    let request: ControlRequest = parse_as(value)?;
    if request.subtype() == "can_use_tool"
        && let Some(permission) = PermissionRequest::from_control_request(&request)
    {
        return Some(Incoming::PermissionRequest(permission));
    }
    Some(Incoming::ControlRequest(request))
}

/// `type: "system"`. Only the fields Newjen uses are typed; everything else is ignored.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct SystemMessage {
    /// For example `init`, `hook_started`, `hook_response`, `status`, `thinking_tokens`.
    pub subtype: String,
    pub session_id: Option<String>,
    /// Only on `init`: the model the session uses.
    pub model: Option<String>,
    /// Only on `init`: the working directory as the CLI sees it (on macOS `/tmp` shows up as
    /// `/private/tmp`).
    pub cwd: Option<String>,
    /// Only on `background_tasks_changed`: every background task running now (`[]` when
    /// none). `None` when the field is missing or not a list.
    #[serde(deserialize_with = "lenient")]
    pub tasks: Option<Vec<BackgroundTask>>,
    /// On `task_started`, `task_updated` and `task_notification`: the task it is about.
    #[serde(deserialize_with = "lenient")]
    pub task_id: Option<String>,
    /// On `task_started` and `task_notification`: the tool call that started the task.
    #[serde(deserialize_with = "lenient")]
    pub tool_use_id: Option<String>,
}

impl SystemMessage {
    /// The full list of background tasks, if this is a `background_tasks_changed` line.
    pub fn background_tasks(&self) -> Option<&[BackgroundTask]> {
        match &self.tasks {
            Some(tasks) if self.subtype == "background_tasks_changed" => Some(tasks),
            _ => None,
        }
    }
}

/// One entry of `background_tasks_changed`: a command (or agent) that keeps running after
/// the turn that started it, such as a dev server started with Bash's `run_in_background`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct BackgroundTask {
    #[serde(deserialize_with = "lenient")]
    pub task_id: String,
    /// What the task does, in Claude's words, e.g. "Start the dev server".
    #[serde(deserialize_with = "lenient")]
    pub description: String,
    /// For example `local_bash`.
    #[serde(deserialize_with = "lenient")]
    pub task_type: Option<String>,
}

/// `type: "assistant"`. Each line carries ONE finished content block. Several lines can share
/// the same `message.id` (thinking, then tool_use, then text), so a line is not a chat bubble.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct AssistantMessage {
    pub message: ApiMessage,
    pub session_id: Option<String>,
    /// Set when this is a subagent's message rather than the main conversation's.
    pub parent_tool_use_id: Option<String>,
    /// True on the partial line printed when the user interrupts a reply mid-stream.
    #[serde(deserialize_with = "lenient")]
    pub aborted: bool,
    /// True when the CLI turned an API failure (not logged in, unknown model…) into a fake
    /// assistant message. The text explains the error; it is not a real reply.
    #[serde(deserialize_with = "lenient")]
    pub is_api_error_message: bool,
    /// Machine-readable error code on API-error messages, e.g. `authentication_failed`.
    #[serde(deserialize_with = "lenient")]
    pub error: Option<String>,
}

/// The Anthropic Messages API message inside an `assistant` line.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ApiMessage {
    #[serde(deserialize_with = "lenient")]
    pub id: String,
    pub model: Option<String>,
    pub content: Vec<ContentBlock>,
    /// Token counts of the model call that produced this line. Several lines of one message
    /// repeat the same counts, and the output count can still be partial.
    #[serde(deserialize_with = "lenient")]
    pub usage: Option<ApiUsage>,
}

/// The `usage` object of an `assistant` line (snake_case, unlike a result's `modelUsage`).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ApiUsage {
    #[serde(deserialize_with = "lenient")]
    pub input_tokens: u64,
    #[serde(deserialize_with = "lenient")]
    pub cache_read_input_tokens: u64,
    #[serde(deserialize_with = "lenient")]
    pub cache_creation_input_tokens: u64,
    #[serde(deserialize_with = "lenient")]
    pub output_tokens: u64,
}

impl ApiUsage {
    /// Everything the model saw and wrote in this call: what now sits in the context.
    pub fn context_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.output_tokens)
    }
}

/// One content block of a message. Built from raw JSON by hand (see the `From<Value>` impl) so
/// an unexpected block shape becomes `Unknown` instead of failing the whole line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(from = "Value")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Claude Code currently sends thinking blocks with empty text (only a signature).
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        /// Either a plain string or an array of blocks; see [`ContentBlock::tool_result_text`].
        content: Value,
        is_error: bool,
    },
    Unknown(Value),
}

impl From<Value> for ContentBlock {
    fn from(value: Value) -> Self {
        let text_field = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_string);
        let block = match value.get("type").and_then(Value::as_str) {
            Some("text") => text_field("text").map(|text| ContentBlock::Text { text }),
            Some("thinking") => Some(ContentBlock::Thinking {
                thinking: text_field("thinking").unwrap_or_default(),
            }),
            Some("tool_use") => match (text_field("id"), text_field("name")) {
                (Some(id), Some(name)) => Some(ContentBlock::ToolUse {
                    id,
                    name,
                    input: value.get("input").cloned().unwrap_or(Value::Null),
                }),
                _ => None,
            },
            Some("tool_result") => {
                text_field("tool_use_id").map(|tool_use_id| ContentBlock::ToolResult {
                    tool_use_id,
                    content: value.get("content").cloned().unwrap_or(Value::Null),
                    is_error: value
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            }
            _ => None,
        };
        block.unwrap_or(ContentBlock::Unknown(value))
    }
}

impl ContentBlock {
    /// The readable text of a tool_result's `content`, which the CLI sends either as a string
    /// or as a list of `{type: "text", text}` blocks.
    pub fn tool_result_text(content: &Value) -> String {
        match content {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    }
}

/// `type: "user"`. On stdout these carry tool results (and synthetic notes); they are never
/// something the person typed.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct UserMessage {
    pub message: UserContent,
    pub session_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    /// Extra facts about each tool result, such as whether the user rejected it.
    #[serde(deserialize_with = "lenient")]
    pub tool_result_meta: Vec<ToolResultMeta>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct UserContent {
    pub content: MessageContent,
}

/// A message body is either a plain string or a list of content blocks.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Default for MessageContent {
    fn default() -> Self {
        MessageContent::Blocks(Vec::new())
    }
}

impl MessageContent {
    /// The content blocks, or an empty slice when the body is a plain string.
    pub fn blocks(&self) -> &[ContentBlock] {
        match self {
            MessageContent::Blocks(blocks) => blocks,
            MessageContent::Text(_) => &[],
        }
    }
}

/// One entry of a user line's `tool_result_meta`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ToolResultMeta {
    /// The tool_use id this entry describes.
    pub id: String,
    /// For example `permission-rule` or `user-rejected` when the tool never ran.
    pub non_execution_kind: Option<String>,
    pub permission_decision: Option<PermissionDecision>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct PermissionDecision {
    /// `accept` or `reject`.
    pub decision: String,
    /// For example `user_temporary`, `user_reject` or `config` (auto-approved).
    pub source: Option<String>,
}

impl ToolResultMeta {
    /// True when the user (or Newjen on their behalf) said no to this tool call.
    pub fn was_rejected(&self) -> bool {
        let rejected_by_decision = self
            .permission_decision
            .as_ref()
            .is_some_and(|decision| decision.decision == "reject");
        let rejected_by_user = self.non_execution_kind.as_deref() == Some("user-rejected");
        rejected_by_decision || rejected_by_user
    }
}

/// `type: "stream_event"` (only sent with `--include-partial-messages`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StreamEventMessage {
    pub event: StreamEvent,
    /// The `msg_…` id of the API message these events belong to. Matches the `message.id` of
    /// the later full `assistant` lines.
    #[serde(default)]
    pub api_message_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

/// An Anthropic streaming (SSE) event. Built by hand from JSON so new event types become
/// `Unknown` instead of failing the line.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(from = "Value")]
pub enum StreamEvent {
    MessageStart { message_id: Option<String> },
    ContentBlockStart { index: u64, block: ContentBlock },
    ContentBlockDelta { index: u64, delta: Delta },
    ContentBlockStop { index: u64 },
    MessageDelta,
    MessageStop,
    Unknown(Value),
}

/// The payload of a `content_block_delta` event.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    /// More reply text.
    Text(String),
    /// More of a tool call's input, as a JSON fragment.
    InputJson(String),
    /// Thinking and signature deltas, and anything new.
    Other(Value),
}

impl From<Value> for StreamEvent {
    fn from(value: Value) -> Self {
        let index = value.get("index").and_then(Value::as_u64);
        let event = match value.get("type").and_then(Value::as_str) {
            Some("message_start") => Some(StreamEvent::MessageStart {
                message_id: value
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            }),
            Some("content_block_start") => index.map(|index| StreamEvent::ContentBlockStart {
                index,
                block: ContentBlock::from(value.get("content_block").cloned().unwrap_or_default()),
            }),
            Some("content_block_delta") => index.map(|index| StreamEvent::ContentBlockDelta {
                index,
                delta: Delta::from_value(value.get("delta").cloned().unwrap_or_default()),
            }),
            Some("content_block_stop") => {
                index.map(|index| StreamEvent::ContentBlockStop { index })
            }
            Some("message_delta") => Some(StreamEvent::MessageDelta),
            Some("message_stop") => Some(StreamEvent::MessageStop),
            _ => None,
        };
        event.unwrap_or(StreamEvent::Unknown(value))
    }
}

impl Delta {
    fn from_value(value: Value) -> Delta {
        let field = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_string);
        match value.get("type").and_then(Value::as_str) {
            Some("text_delta") => match field("text") {
                Some(text) => Delta::Text(text),
                None => Delta::Other(value),
            },
            Some("input_json_delta") => match field("partial_json") {
                Some(json) => Delta::InputJson(json),
                None => Delta::Other(value),
            },
            _ => Delta::Other(value),
        }
    }
}

/// `type: "control_request"` from the CLI. `request` is kept raw because its shape depends on
/// the subtype; approvals are decoded into [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    #[serde(default)]
    pub request: Value,
}

impl ControlRequest {
    /// For example `can_use_tool`.
    pub fn subtype(&self) -> &str {
        self.request
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("")
    }
}

/// `type: "control_response"`: the CLI answering one of our control requests (interrupt,
/// initialize). Newjen does not need the contents yet, so `response` stays raw.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlResponse {
    pub response: Value,
}

impl ControlResponse {
    /// The id of the request this answers.
    pub fn request_id(&self) -> Option<&str> {
        self.response.get("request_id").and_then(Value::as_str)
    }
}

/// `type: "result"`: the end of a turn.
///
/// Branch on `is_error`, never on `subtype` alone: API errors (not logged in, unknown model)
/// arrive as `subtype: "success"` with `is_error: true`.
///
/// Every field is `lenient`: losing this line would leave the turn running forever.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ResultMessage {
    #[serde(deserialize_with = "lenient")]
    pub subtype: String,
    #[serde(deserialize_with = "lenient")]
    pub is_error: bool,
    /// The final reply text, or the error text for API errors. `null` after an interrupt.
    #[serde(deserialize_with = "lenient")]
    pub result: Option<String>,
    /// Error strings, e.g. `["No conversation found with session ID: …"]`.
    #[serde(deserialize_with = "lenient")]
    pub errors: Option<Vec<Value>>,
    /// For example `completed`, `api_error`, `aborted_streaming` (user pressed stop) or
    /// `aborted_tools` (user denied and stopped).
    #[serde(deserialize_with = "lenient")]
    pub terminal_reason: Option<String>,
    /// How long this turn took. Per turn, not a running total.
    #[serde(deserialize_with = "lenient")]
    pub duration_ms: Option<u64>,
    /// Cost so far. A RUNNING TOTAL across turns and across `--resume`, not this turn's cost.
    #[serde(deserialize_with = "lenient")]
    pub total_cost_usd: Option<f64>,
    /// Usage of this turn only, per model. Use this for per-turn cost.
    #[serde(deserialize_with = "lenient")]
    pub model_usage_since_last_result: Option<HashMap<String, ModelUsage>>,
    #[serde(deserialize_with = "lenient")]
    pub session_id: Option<String>,
    #[serde(deserialize_with = "lenient")]
    pub num_turns: Option<u64>,
    /// User messages still queued when this result was produced. More than 0 means another
    /// turn follows with no further input: the user sent a message just as claude started a
    /// turn by itself (after a background task finished).
    #[serde(deserialize_with = "lenient")]
    pub queued_turn_count: Option<u64>,
}

/// Per-model usage inside a result.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ModelUsage {
    #[serde(rename = "costUSD", deserialize_with = "lenient")]
    pub cost_usd: Option<f64>,
    #[serde(rename = "inputTokens", deserialize_with = "lenient")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens", deserialize_with = "lenient")]
    pub output_tokens: u64,
    #[serde(rename = "cacheReadInputTokens", deserialize_with = "lenient")]
    pub cache_read_input_tokens: u64,
    #[serde(rename = "cacheCreationInputTokens", deserialize_with = "lenient")]
    pub cache_creation_input_tokens: u64,
    /// Part of `output_tokens`, not on top of it.
    #[serde(rename = "thinkingTokens", deserialize_with = "lenient")]
    pub thinking_tokens: u64,
    #[serde(rename = "contextWindow", deserialize_with = "lenient")]
    pub context_window: Option<u64>,
}

impl ResultMessage {
    /// True when the turn ended because the user stopped it (interrupt, or deny-and-stop).
    /// That is not an error, even though the CLI marks it `is_error`.
    pub fn was_stopped(&self) -> bool {
        self.terminal_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("aborted"))
    }

    /// True when a queued user message will start another turn right after this one (see
    /// `queued_turn_count`).
    pub fn another_turn_follows(&self) -> bool {
        self.queued_turn_count.is_some_and(|count| count > 0)
    }

    /// This turn's cost, if the CLI reported per-turn usage: the sum of `costUSD` over
    /// `model_usage_since_last_result`. `None` when that field is missing.
    pub fn turn_cost_usd(&self) -> Option<f64> {
        let usage = self.model_usage_since_last_result.as_ref()?;
        // `fold` from 0.0 rather than `sum`: an empty `sum` of floats is -0.0, which would
        // print as "$-0.0000".
        Some(
            usage
                .values()
                .filter_map(|model| model.cost_usd)
                .fold(0.0, |total, cost| total + cost),
        )
    }

    /// True when this failed result says the `--resume` session does not exist, e.g.
    /// `"No conversation found with session ID: …"` (seen live in `errors`; see the
    /// `error_bad_resume` fixture).
    pub fn is_missing_conversation(&self) -> bool {
        let says_so = |text: &str| text.contains("No conversation found");
        self.is_error
            && (self.error_lines().iter().any(|line| says_so(line))
                || self.result.as_deref().is_some_and(says_so))
    }

    /// The `errors` list as readable lines.
    pub fn error_lines(&self) -> Vec<String> {
        let errors = self.errors.as_deref().unwrap_or(&[]);
        errors
            .iter()
            .map(|error| match error {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Approvals
// ---------------------------------------------------------------------------------------------

/// The CLI asking whether a tool may run (`control_request` / `can_use_tool`).
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    /// Our reply must echo this id.
    pub request_id: String,
    /// Links the request to the `tool_use` block and its later `tool_result`.
    pub tool_use_id: Option<String>,
    pub tool_name: String,
    /// The CLI's human-readable tool name (falls back to `tool_name`).
    pub display_name: String,
    /// The tool's arguments, e.g. `{"command": "touch file.txt"}` for Bash.
    pub input: Value,
    /// The CLI's one-line description of what the call does, if any.
    pub description: Option<String>,
    /// `permission_suggestions` from the CLI, e.g. an `addRules` entry that would allow this
    /// command from now on. Used for "Allow for this session".
    pub suggestions: Vec<Value>,
    /// The file or folder the call would touch, when the CLI names one (e.g. the file a
    /// `touch` creates). Lets the UI point out paths outside the project.
    pub blocked_path: Option<String>,
}

impl PermissionRequest {
    /// Decodes a `can_use_tool` control request. `None` if the tool name is missing.
    pub fn from_control_request(request: &ControlRequest) -> Option<PermissionRequest> {
        let body = &request.request;
        let text = |name: &str| body.get(name).and_then(Value::as_str).map(str::to_string);
        let tool_name = text("tool_name")?;
        Some(PermissionRequest {
            request_id: request.request_id.clone(),
            tool_use_id: text("tool_use_id"),
            display_name: text("display_name").unwrap_or_else(|| tool_name.clone()),
            tool_name,
            input: body.get("input").cloned().unwrap_or(Value::Null),
            description: text("description"),
            suggestions: body
                .get("permission_suggestions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            blocked_path: text("blocked_path"),
        })
    }
}

/// How the user answered an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAnswer {
    /// Run this one call.
    AllowOnce,
    /// Run it, and auto-approve similar calls for the rest of this session: the rule the CLI
    /// suggests, or this exact Bash command. Nothing is written to the project's settings.
    /// When there is no safe rule to remember (for example Write and Edit), this is the same
    /// as `AllowOnce`; [`session_scope`] tells the UI which case it is, before the click.
    AllowForSession,
    /// Don't run it; Claude is told and carries on.
    Deny,
    /// Don't run it, and end the turn.
    DenyAndStop,
}

impl PermissionAnswer {
    /// True for the two deny answers.
    pub fn is_deny(self) -> bool {
        matches!(self, PermissionAnswer::Deny | PermissionAnswer::DenyAndStop)
    }
}

// ---------------------------------------------------------------------------------------------
// Outgoing messages (Newjen → CLI stdin)
// ---------------------------------------------------------------------------------------------
//
// Each builder returns a JSON value. The session serializes it with a JSON encoder and adds a
// newline; a stdin line that is not valid JSON makes the CLI exit, so never build lines by hand.

/// A prompt typed by the user.
pub fn user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    })
}

/// The reply to an approval request.
pub fn permission_response(request: &PermissionRequest, answer: PermissionAnswer) -> Value {
    let decision = match answer {
        PermissionAnswer::AllowOnce => allow(request),
        PermissionAnswer::AllowForSession => {
            let mut decision = allow(request);
            // An empty list means we found nothing safe to remember: this is then allow-once.
            let permissions = session_permissions(request);
            if !permissions.is_empty() {
                decision["updatedPermissions"] = Value::Array(permissions);
            }
            decision
        }
        PermissionAnswer::Deny => json!({ "behavior": "deny", "message": DENY_MESSAGE }),
        PermissionAnswer::DenyAndStop => {
            json!({ "behavior": "deny", "message": DENY_MESSAGE, "interrupt": true })
        }
    };
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request.request_id,
            "response": decision,
        },
    })
}

pub fn permission_response_with_note(
    request: &PermissionRequest,
    answer: PermissionAnswer,
    note: Option<&str>,
) -> Value {
    let mut response = permission_response(request, answer);
    if answer.is_deny()
        && let Some(note) = note.map(str::trim).filter(|note| !note.is_empty())
    {
        response["response"]["response"]["message"] = json!(note);
    }
    response
}

fn allow(request: &PermissionRequest) -> Value {
    json!({ "behavior": "allow", "updatedInput": request.input })
}

/// What "Always allow" would remember for a request. The UI can show this *before* the click,
/// e.g. "Always allow → remembers `touch a.txt` for this session".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionScope {
    /// The rules that would be remembered until claude exits, as readable text: a rule's
    /// content (a Bash command such as `npm test *`), or `server · tool` for one MCP tool.
    Remembers(Vec<String>),
    /// "Always allow" would act as allow once, for this reason.
    OnceOnly(OnceReason),
}

/// Why "Always allow" falls back to allow once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnceReason {
    /// A file edit (Write, Edit, MultiEdit, NotebookEdit). Every safe-looking rule for edits
    /// turned out too broad or useless (verified live; see `session_permissions`).
    Edit,
    /// The CLI offered no narrow rule, and there is no exact Bash command to name.
    NoSafeRule,
}

/// Tools that change files. "Always allow" never remembers anything for them.
const EDIT_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// What "Always allow" would remember for `request`. Built from the same rules that
/// [`permission_response`] sends, so the label can never promise more than the answer does.
pub fn session_scope(request: &PermissionRequest) -> SessionScope {
    if EDIT_TOOLS.contains(&request.tool_name.as_str()) {
        return SessionScope::OnceOnly(OnceReason::Edit);
    }
    let rules = session_rules(request);
    if rules.is_empty() {
        return SessionScope::OnceOnly(OnceReason::NoSafeRule);
    }
    SessionScope::Remembers(rules.iter().map(SessionRule::label).collect())
}

/// One rule that "Always allow" remembers: one Bash command (`touch a.txt`), or one MCP tool.
/// A thread saves these and passes them back to every new claude with `--allowedTools`, so a
/// resumed conversation keeps them (see `thread.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRule {
    /// The tool the rule is for, e.g. `Bash` or `mcp__linear__list_issues`.
    pub tool_name: String,
    /// What the rule allows, e.g. the Bash command `npm test *`. `None` for a rule that names
    /// one MCP tool and nothing else.
    pub rule_content: Option<String>,
}

impl SessionRule {
    /// The rule in the CLI's own notation, which `--allowedTools` takes: `Bash(touch a.txt)`,
    /// or just the tool name for an MCP tool.
    ///
    /// The content is escaped the way the CLI writes rules itself: `\` becomes `\\`, and `(`
    /// and `)` become `\(` and `\)`. claude un-escapes `--allowedTools` values when it reads
    /// them (checked in claude 2.1.314), so without this a command such as
    /// `sed 's/\(a\)/b/' f` would come back as a different command and no longer match.
    pub fn cli_rule(&self) -> String {
        match &self.rule_content {
            Some(content) => format!("{}({})", self.tool_name, escape_rule_content(content)),
            None => self.tool_name.clone(),
        }
    }

    /// The text the approval dock and the grants list show: the rule's content (a command),
    /// or `server · tool` for an MCP tool.
    pub fn label(&self) -> String {
        match &self.rule_content {
            Some(content) => content.clone(),
            None => crate::providers::claude::display_name(&self.tool_name),
        }
    }
}

/// Escapes a rule's content for the CLI's `Tool(content)` notation, exactly as claude does it
/// itself. The backslash goes first, so the backslashes added for brackets are not doubled.
fn escape_rule_content(content: &str) -> String {
    content
        .replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// The rules answering `request` with [`PermissionAnswer::AllowForSession`] makes the CLI
/// remember. Read from the very `updatedPermissions` that answer sends, so what Newjen saves is
/// always exactly what the CLI was told. Empty when "Always allow" acts as allow once.
pub fn session_rules(request: &PermissionRequest) -> Vec<SessionRule> {
    session_permissions(request)
        .iter()
        .filter_map(|permission| permission.get("rules").and_then(Value::as_array))
        .flatten()
        .filter_map(|rule| {
            let tool_name = rule.get("toolName").and_then(Value::as_str)?.to_string();
            let rule_content = rule
                .get("ruleContent")
                .and_then(Value::as_str)
                .filter(|content| !content.trim().is_empty())
                .map(str::to_string);
            Some(SessionRule {
                tool_name,
                rule_content,
            })
        })
        .collect()
}

/// True if answering `request` with [`PermissionAnswer::AllowForSession`] really makes the
/// CLI remember the approval. False means it is sent as a plain allow-once. A shorthand for
/// checking [`session_scope`] for `Remembers`; the app shows `session_scope` itself.
pub fn allows_for_session(request: &PermissionRequest) -> bool {
    matches!(session_scope(request), SessionScope::Remembers(_))
}

/// What to send as `updatedPermissions` for "Allow for this session". Everything returned has
/// `destination: "session"`, so it lasts until the CLI exits and is never written into a
/// settings file (the CLI's own suggestions usually say `localSettings`). An empty list means
/// "nothing safe to remember": the answer is then a plain allow-once.
///
/// 1. Nothing for file edits (see [`EDIT_TOOLS`]).
/// 2. The CLI's `addRules` suggestions that are narrow (see [`is_narrow_suggestion`]), e.g.
///    Bash `touch a.txt`. Other suggestions are dropped.
/// 3. Else, for Bash, a rule naming the exact command, so only identical calls are approved.
///    Some calls come with no suggestions at all (seen live for `echo $(date) > when.txt`).
///    Skipped if the command contains `*`: in a Bash rule `*` is a wildcard, so the rule would
///    also approve other commands.
/// 4. Else nothing. Verified live with claude 2.1.310, the other options are too broad:
///    - The `setMode` `acceptEdits` suggestion (what Write and Edit offer) also auto-approves
///      file-changing Bash commands such as `rm` and `mkdir` for the rest of the session.
///    - A whole-tool rule such as `{"toolName": "Write"}` also approved a Write outside the
///      project.
///    - An exact-file Write rule did not match a later write to the same file, so it is
///      useless.
fn session_permissions(request: &PermissionRequest) -> Vec<Value> {
    if EDIT_TOOLS.contains(&request.tool_name.as_str()) {
        return Vec::new();
    }
    let rules: Vec<Value> = request
        .suggestions
        .iter()
        .filter(|suggestion| is_narrow_suggestion(suggestion, &request.tool_name))
        .map(|suggestion| {
            let mut rule = suggestion.clone();
            rule["destination"] = Value::String("session".to_string());
            rule
        })
        .collect();
    if !rules.is_empty() {
        return rules;
    }

    if request.tool_name != "Bash" {
        return Vec::new();
    }
    let command = request
        .input
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if command.trim().is_empty() || command.contains('*') {
        return Vec::new();
    }
    vec![json!({
        "type": "addRules",
        "rules": [{ "toolName": "Bash", "ruleContent": command }],
        "behavior": "allow",
        "destination": "session",
    })]
}

/// True for an `addRules` allow suggestion whose every rule is narrow: it is for the tool being
/// asked about (`tool_name`), that tool is not a file edit, and the rule has a `ruleContent`
/// (such as one Bash command) or names exactly one MCP tool (`mcp__server__tool`). A bare
/// rule such as `{"toolName": "WebSearch"}` would allow the whole tool, so a suggestion with
/// one of those is dropped. So is one that sneaks in a rule for another tool (say an `Edit`
/// rule on a Bash request): the dock only shows this call, so it must not approve others.
fn is_narrow_suggestion(suggestion: &Value, tool_name: &str) -> bool {
    let text = |name: &str| suggestion.get(name).and_then(Value::as_str);
    if text("type") != Some("addRules") || text("behavior") != Some("allow") {
        return false;
    }
    let Some(rules) = suggestion.get("rules").and_then(Value::as_array) else {
        return false;
    };
    !rules.is_empty()
        && rules.iter().all(|rule| {
            let tool = rule.get("toolName").and_then(Value::as_str);
            // Edits are never remembered (see `session_permissions`), whatever tool asked.
            if tool != Some(tool_name) || tool.is_some_and(|tool| EDIT_TOOLS.contains(&tool)) {
                return false;
            }
            let has_content = rule
                .get("ruleContent")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.trim().is_empty());
            has_content || bare_mcp_tool(rule).is_some()
        })
}

/// For a rule with no content that names one MCP tool, that tool's name.
fn bare_mcp_tool(rule: &Value) -> Option<&str> {
    let tool = rule.get("toolName").and_then(Value::as_str)?;
    // `mcp__server` alone would allow every tool of that server: require `mcp__server__tool`.
    let (server, name) = tool.strip_prefix("mcp__")?.split_once("__")?;
    (!server.is_empty() && !name.is_empty()).then_some(tool)
}

/// The error reply to a control request Newjen does not handle, so the CLI never waits for an
/// answer that will not come. The shape is the Agent SDK's `ControlErrorResponse`
/// (`{subtype: "error", request_id, error}`, Python SDK `types.py`; `query.py` sends it the
/// same way when a request handler fails).
pub fn control_error_response(request_id: &str, error: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": error,
        },
    })
}

/// Asks the CLI to stop the current turn. `request_id` is any id we choose; the CLI echoes it
/// in its `control_response`.
pub fn interrupt_request(request_id: &str) -> Value {
    json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash_request(suggestions: Vec<Value>) -> PermissionRequest {
        PermissionRequest {
            request_id: "req-1".to_string(),
            tool_use_id: Some("toolu_1".to_string()),
            tool_name: "Bash".to_string(),
            display_name: "Bash".to_string(),
            input: json!({ "command": "touch a.txt" }),
            description: None,
            suggestions,
            blocked_path: None,
        }
    }

    #[test]
    fn assistant_usage_and_result_model_usage_keep_their_own_key_styles() {
        let Incoming::Assistant(assistant) = Incoming::from_value(json!({
            "type": "assistant",
            "message": {"id": "m1", "model": "claude-haiku-5-5", "content": [],
                "usage": {"input_tokens": 2, "cache_creation_input_tokens": 13999,
                    "cache_read_input_tokens": 15381, "output_tokens": 4}}
        })) else {
            panic!("not an assistant line");
        };
        let usage = assistant.message.usage.unwrap();
        assert_eq!(usage.context_tokens(), 2 + 13999 + 15381 + 4);

        let Incoming::Result(result) = Incoming::from_value(json!({
            "type": "result",
            "model_usage_since_last_result": {"claude-haiku-5-5": {
                "inputTokens": 2, "outputTokens": 6, "cacheReadInputTokens": 15859,
                "cacheCreationInputTokens": 13608, "thinkingTokens": 3,
                "costUSD": 0.5, "contextWindow": 1000000}}
        })) else {
            panic!("not a result line");
        };
        let model = &result.model_usage_since_last_result.unwrap()["claude-haiku-5-5"];
        assert_eq!(model.cache_read_input_tokens, 15859);
        assert_eq!(model.cache_creation_input_tokens, 13608);
        assert_eq!(model.thinking_tokens, 3);
        assert_eq!(model.context_window, Some(1_000_000));
    }

    #[test]
    fn malformed_usage_numbers_default_instead_of_losing_the_line() {
        let Incoming::Result(result) = Incoming::from_value(json!({
            "type": "result",
            "model_usage_since_last_result": {"m": {"inputTokens": "many", "contextWindow": "big"}}
        })) else {
            panic!("not a result line");
        };
        let model = &result.model_usage_since_last_result.unwrap()["m"];
        assert_eq!(model.input_tokens, 0);
        assert_eq!(model.context_window, None);
    }

    #[test]
    fn denial_notes_are_encoded_and_empty_notes_keep_the_default() {
        let request = bash_request(vec![]);
        let reply = permission_response_with_note(
            &request,
            PermissionAnswer::DenyAndStop,
            Some("Use a test folder instead.\nKeep my files."),
        );
        assert_eq!(
            reply["response"]["response"]["message"],
            "Use a test folder instead.\nKeep my files."
        );
        assert_eq!(reply["response"]["response"]["interrupt"], true);
        assert_eq!(
            permission_response_with_note(&request, PermissionAnswer::Deny, Some("  ")),
            permission_response(&request, PermissionAnswer::Deny)
        );
        assert_eq!(
            permission_response_with_note(&request, PermissionAnswer::AllowOnce, Some("ignored")),
            permission_response(&request, PermissionAnswer::AllowOnce)
        );
    }

    #[test]
    fn non_json_line_is_unknown_not_an_error() {
        assert_eq!(
            Incoming::parse("this is not json"),
            Incoming::Unknown(Value::String("this is not json".to_string()))
        );
    }

    #[test]
    fn unknown_type_is_kept_raw() {
        let line = r#"{"type":"brand_new_thing","x":1}"#;
        assert!(matches!(Incoming::parse(line), Incoming::Unknown(_)));
    }

    #[test]
    fn unknown_content_block_does_not_break_the_line() {
        let line = r#"{"type":"assistant","message":{"id":"m","content":[{"type":"mystery"},{"type":"text","text":"hi"}]}}"#;
        let Incoming::Assistant(message) = Incoming::parse(line) else {
            panic!("expected an assistant line");
        };
        assert!(matches!(
            message.message.content[0],
            ContentBlock::Unknown(_)
        ));
        assert_eq!(
            message.message.content[1],
            ContentBlock::Text {
                text: "hi".to_string()
            }
        );
    }

    #[test]
    fn odd_field_values_do_not_drop_a_result_or_assistant_line() {
        // `null` or an unexpected type in one field must not turn the line into `Unknown`.
        let line = r#"{"type":"result","subtype":null,"is_error":false,"duration_ms":12.5,
                       "terminal_reason":"completed"}"#;
        let Incoming::Result(result) = Incoming::parse(line) else {
            panic!("expected a result line");
        };
        assert_eq!(result.subtype, "");
        assert_eq!(result.duration_ms, None);
        assert_eq!(result.terminal_reason.as_deref(), Some("completed"));

        let line = r#"{"type":"assistant","aborted":null,"error":{"code":1},
                       "message":{"id":"m","content":[{"type":"text","text":"hi"}]}}"#;
        let Incoming::Assistant(message) = Incoming::parse(line) else {
            panic!("expected an assistant line");
        };
        assert!(!message.aborted);
        assert_eq!(message.error, None);
        assert_eq!(message.message.content.len(), 1);
    }

    #[test]
    fn empty_turn_usage_costs_plus_zero() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,
                       "model_usage_since_last_result":{}}"#;
        let Incoming::Result(result) = Incoming::parse(line) else {
            panic!("expected a result line");
        };
        let cost = result.turn_cost_usd().expect("a cost");
        assert!(cost == 0.0 && cost.is_sign_positive(), "{cost}");
    }

    #[test]
    fn allow_once_echoes_the_input() {
        let reply = permission_response(&bash_request(vec![]), PermissionAnswer::AllowOnce);
        assert_eq!(reply["response"]["request_id"], "req-1");
        assert_eq!(reply["response"]["response"]["behavior"], "allow");
        assert_eq!(
            reply["response"]["response"]["updatedInput"]["command"],
            "touch a.txt"
        );
    }

    #[test]
    fn allow_for_session_rewrites_add_rules_to_session() {
        let suggestions = vec![
            json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "touch a.txt"}],
                   "behavior": "allow", "destination": "localSettings"}),
            json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}),
        ];
        let reply = permission_response(
            &bash_request(suggestions),
            PermissionAnswer::AllowForSession,
        );
        let permissions = reply["response"]["response"]["updatedPermissions"]
            .as_array()
            .expect("updatedPermissions");
        assert_eq!(permissions.len(), 1);
        assert_eq!(permissions[0]["type"], "addRules");
        assert_eq!(permissions[0]["destination"], "session");
    }

    /// The `updatedPermissions` an AllowForSession reply carries (`Null` if none).
    fn session_permissions_sent(request: &PermissionRequest) -> Value {
        permission_response(request, PermissionAnswer::AllowForSession)["response"]["response"]
            ["updatedPermissions"]
            .clone()
    }

    /// True if the AllowForSession reply is exactly the AllowOnce reply.
    fn session_answer_is_allow_once(request: &PermissionRequest) -> bool {
        permission_response(request, PermissionAnswer::AllowForSession)
            == permission_response(request, PermissionAnswer::AllowOnce)
    }

    #[test]
    fn allow_for_session_never_forwards_set_mode() {
        // What the CLI suggests for Write and Edit (recorded live). `acceptEdits` would also
        // auto-approve `rm` and `mkdir` through Bash, so it is never sent.
        let mut request = bash_request(vec![
            json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}),
            json!({"type": "setMode", "mode": "bypassPermissions", "destination": "session"}),
        ]);
        request.tool_name = "Write".to_string();
        request.input = json!({"file_path": "/tmp/project/a.txt", "content": "one"});
        assert!(session_answer_is_allow_once(&request));
        assert!(!allows_for_session(&request));
    }

    #[test]
    fn allow_for_session_without_suggestions_names_the_exact_bash_command() {
        let request = bash_request(vec![]);
        assert_eq!(
            session_permissions_sent(&request),
            json!([{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "touch a.txt"}],
                    "behavior": "allow", "destination": "session"}])
        );
        assert!(allows_for_session(&request));
    }

    #[test]
    fn allow_for_session_never_allows_a_whole_tool() {
        // A whole-tool rule approved a Write outside the project (verified live).
        let mut request = bash_request(vec![]);
        request.tool_name = "WebSearch".to_string();
        request.input = json!({"query": "x"});
        assert!(session_answer_is_allow_once(&request));
        assert!(!allows_for_session(&request));
    }

    #[test]
    fn allow_for_session_for_a_bash_command_with_a_star_is_allow_once() {
        // `*` is a wildcard in a Bash rule (verified live), so this rule would also approve
        // other commands.
        let mut request = bash_request(vec![]);
        request.input = json!({"command": "rm *.txt"});
        assert!(session_answer_is_allow_once(&request));
        assert!(!allows_for_session(&request));
    }

    #[test]
    fn allow_for_session_for_bash_without_a_command_is_allow_once() {
        for input in [json!({}), json!({"command": "  "})] {
            let mut request = bash_request(vec![]);
            request.input = input;
            assert!(session_answer_is_allow_once(&request));
            assert!(!allows_for_session(&request));
        }
    }

    /// A request for `tool_name` with one synthetic suggestion.
    fn request_with(tool_name: &str, suggestion: Value) -> PermissionRequest {
        let mut request = bash_request(vec![suggestion]);
        request.tool_name = tool_name.to_string();
        request.input = json!({"query": "x"});
        request
    }

    #[test]
    fn a_bare_whole_tool_suggestion_is_allow_once() {
        let request = request_with(
            "WebSearch",
            json!({"type": "addRules", "rules": [{"toolName": "WebSearch"}],
                   "behavior": "allow", "destination": "localSettings"}),
        );
        assert!(session_answer_is_allow_once(&request));
        assert_eq!(
            session_scope(&request),
            SessionScope::OnceOnly(OnceReason::NoSafeRule)
        );
    }

    #[test]
    fn a_bare_rule_for_one_mcp_tool_is_forwarded() {
        let request = request_with(
            "mcp__x__y",
            json!({"type": "addRules", "rules": [{"toolName": "mcp__x__y"}],
                   "behavior": "allow", "destination": "localSettings"}),
        );
        assert_eq!(
            session_permissions_sent(&request),
            json!([{"type": "addRules", "rules": [{"toolName": "mcp__x__y"}],
                    "behavior": "allow", "destination": "session"}])
        );
        assert_eq!(
            session_scope(&request),
            SessionScope::Remembers(vec!["x · y".to_string()])
        );
    }

    #[test]
    fn a_suggestion_with_one_bare_non_mcp_rule_is_dropped() {
        let request = request_with(
            "WebFetch",
            json!({"type": "addRules", "rules": [
                       {"toolName": "WebFetch", "ruleContent": "domain:example.com"},
                       {"toolName": "WebSearch"}],
                   "behavior": "allow", "destination": "localSettings"}),
        );
        assert!(session_answer_is_allow_once(&request));
        assert!(!allows_for_session(&request));
    }

    #[test]
    fn scope_names_the_remembered_rule_before_the_click() {
        let request = bash_request(vec![json!({"type": "addRules", "rules": [
            {"toolName": "Bash", "ruleContent": "npm test *"}],
            "behavior": "allow", "destination": "localSettings"})]);
        assert_eq!(
            session_scope(&request),
            SessionScope::Remembers(vec!["npm test *".to_string()])
        );
        assert!(allows_for_session(&request));
    }

    #[test]
    fn a_rule_for_another_tool_or_an_edit_tool_is_never_forwarded() {
        // A Bash request whose suggestion would also allow editing any file: dropped, so
        // "Always allow" falls back to the exact Bash command alone.
        let request = bash_request(vec![json!({"type": "addRules", "rules": [
            {"toolName": "Edit", "ruleContent": "//abs/**"}],
            "behavior": "allow", "destination": "localSettings"})]);
        assert_eq!(
            session_permissions_sent(&request),
            json!([{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "touch a.txt"}],
                    "behavior": "allow", "destination": "session"}])
        );
        // An MCP rule must name the very tool that asked, not a sibling.
        let request = request_with(
            "mcp__x__y",
            json!({"type": "addRules", "rules": [{"toolName": "mcp__x__z"}],
                   "behavior": "allow", "destination": "localSettings"}),
        );
        assert!(session_answer_is_allow_once(&request));
    }

    #[test]
    fn edits_are_once_only_even_with_a_narrow_suggestion() {
        let mut request = request_with(
            "Edit",
            json!({"type": "addRules", "rules": [{"toolName": "Edit", "ruleContent": "a.txt"}],
                   "behavior": "allow", "destination": "localSettings"}),
        );
        request.input = json!({"file_path": "/tmp/project/a.txt"});
        assert_eq!(
            session_scope(&request),
            SessionScope::OnceOnly(OnceReason::Edit)
        );
        assert!(session_answer_is_allow_once(&request));
    }

    #[test]
    fn session_rules_match_what_the_answer_sends() {
        let request = bash_request(vec![json!({"type": "addRules", "rules": [
            {"toolName": "Bash", "ruleContent": "git add:*"}],
            "behavior": "allow", "destination": "localSettings"})]);
        let rules = session_rules(&request);
        assert_eq!(
            rules,
            [SessionRule {
                tool_name: "Bash".to_string(),
                rule_content: Some("git add:*".to_string()),
            }]
        );
        assert_eq!(rules[0].cli_rule(), "Bash(git add:*)");
        assert_eq!(rules[0].label(), "git add:*");

        // No suggestion: the exact command.
        let rules = session_rules(&bash_request(vec![]));
        assert_eq!(rules[0].cli_rule(), "Bash(touch a.txt)");

        // A command with backslashes and brackets: escaped for `--allowedTools`, while the
        // label (and the session rule claude was sent) keep it as it is.
        let mut sed = bash_request(vec![]);
        sed.input = json!({"command": r"sed -i 's/\(a\)/b/' f"});
        let rules = session_rules(&sed);
        assert_eq!(
            rules[0].rule_content.as_deref(),
            Some(r"sed -i 's/\(a\)/b/' f")
        );
        assert_eq!(rules[0].cli_rule(), r"Bash(sed -i 's/\\\(a\\\)/b/' f)");
        assert_eq!(rules[0].label(), r"sed -i 's/\(a\)/b/' f");

        // One MCP tool, by name only.
        let request = request_with(
            "mcp__pencil__get_style",
            json!({"type": "addRules", "rules": [{"toolName": "mcp__pencil__get_style"}],
                   "behavior": "allow", "destination": "localSettings"}),
        );
        let rules = session_rules(&request);
        assert_eq!(rules[0].cli_rule(), "mcp__pencil__get_style");
        assert_eq!(rules[0].label(), "pencil · get_style");

        // Nothing for edits or whole tools, as `session_scope` says.
        let mut edit = bash_request(vec![]);
        edit.tool_name = "Write".to_string();
        assert!(session_rules(&edit).is_empty());
        let mut search = bash_request(vec![]);
        search.tool_name = "WebSearch".to_string();
        assert!(session_rules(&search).is_empty());
    }

    /// How claude 2.1.314 reads an `--allowedTools` value (its `AJ` and `rms` functions): the
    /// content runs from the first `(` to the final `)` that no backslash escapes, then `\(`,
    /// `\)` and `\\` are un-escaped. Returns the tool and the content.
    fn read_like_the_cli(rule: &str) -> (String, Option<String>) {
        let escaped =
            |at: usize| rule[..at].chars().rev().take_while(|c| *c == '\\').count() % 2 == 1;
        let open = rule
            .char_indices()
            .find(|(at, c)| *c == '(' && !escaped(*at));
        let close = rule
            .char_indices()
            .rev()
            .find(|(at, c)| *c == ')' && !escaped(*at));
        match (open, close) {
            (Some((open, _)), Some((close, _))) if close == rule.len() - 1 && close > open => {
                let content = rule[open + 1..close]
                    .replace("\\(", "(")
                    .replace("\\)", ")")
                    .replace("\\\\", "\\");
                (rule[..open].to_string(), Some(content))
            }
            _ => (rule.to_string(), None),
        }
    }

    #[test]
    fn rule_strings_are_escaped_as_the_cli_reads_them() {
        let bash = |content: &str| SessionRule {
            tool_name: "Bash".to_string(),
            rule_content: Some(content.to_string()),
        };
        // (command, what `--allowedTools` gets). The first four were also verified live.
        let cases = [
            ("touch one.txt", "Bash(touch one.txt)"),
            ("echo a,b", "Bash(echo a,b)"),
            (r#"echo "hi there""#, r#"Bash(echo "hi there")"#),
            ("git add:*", "Bash(git add:*)"),
            (r#"echo "it's (here)""#, r#"Bash(echo "it's \(here\)")"#),
            (
                r"sed 's/\(foo\)/bar/' a.txt",
                r"Bash(sed 's/\\\(foo\\\)/bar/' a.txt)",
            ),
            (r"printf 'a\\nb' > f", r"Bash(printf 'a\\\\nb' > f)"),
            (r"echo a\", r"Bash(echo a\\)"),
        ];
        for (command, cli) in cases {
            let rule = bash(command);
            assert_eq!(rule.cli_rule(), cli);
            // claude gets back exactly the command the user approved.
            assert_eq!(
                read_like_the_cli(cli),
                ("Bash".to_string(), Some(command.to_string()))
            );
            // The label stays the command as it is.
            assert_eq!(rule.label(), command);
        }
        let mcp = SessionRule {
            tool_name: "mcp__pencil__get_style".to_string(),
            rule_content: None,
        };
        assert_eq!(mcp.cli_rule(), "mcp__pencil__get_style");
        assert_eq!(
            read_like_the_cli("mcp__pencil__get_style"),
            ("mcp__pencil__get_style".to_string(), None)
        );
    }

    #[test]
    fn missing_conversation_is_recognised_only_on_errors() {
        let missing = |is_error: bool| ResultMessage {
            is_error,
            errors: Some(vec![json!("No conversation found with session ID: abc")]),
            ..ResultMessage::default()
        };
        assert!(missing(true).is_missing_conversation());
        assert!(!missing(false).is_missing_conversation());
        let other = ResultMessage {
            is_error: true,
            result: Some("Not logged in".to_string()),
            ..ResultMessage::default()
        };
        assert!(!other.is_missing_conversation());
    }

    #[test]
    fn a_queued_user_message_means_another_turn_follows() {
        let result = |line: &str| match Incoming::parse(line) {
            Incoming::Result(result) => result,
            other => panic!("not a result: {other:?}"),
        };
        let own_turn = result(
            r#"{"type":"result","subtype":"success","is_error":false,"queued_turn_count":1,
                "origin":{"kind":"task-notification","producer":"session-task"}}"#,
        );
        assert!(own_turn.another_turn_follows());
        for line in [
            r#"{"type":"result","subtype":"success","is_error":false,"queued_turn_count":0}"#,
            r#"{"type":"result","subtype":"success","is_error":false}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"queued_turn_count":"1"}"#,
        ] {
            assert!(!result(line).another_turn_follows(), "{line}");
        }
    }

    fn system(line: &str) -> SystemMessage {
        match Incoming::parse(line) {
            Incoming::System(message) => message,
            other => panic!("not a system line: {other:?}"),
        }
    }

    #[test]
    fn background_tasks_are_read_only_from_their_own_line() {
        let changed = system(
            r#"{"type":"system","subtype":"background_tasks_changed","tasks":[
                {"task_id":"b1","run_id":"r","task_type":"local_bash","description":"Dev server"},
                {"task_id":"b2","description":null}]}"#,
        );
        let tasks = changed.background_tasks().expect("a task list");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].task_id, "b1");
        assert_eq!(tasks[0].description, "Dev server");
        assert_eq!(tasks[0].task_type.as_deref(), Some("local_bash"));
        assert_eq!(tasks[1].description, "");

        let none = system(r#"{"type":"system","subtype":"background_tasks_changed","tasks":[]}"#);
        assert_eq!(none.background_tasks(), Some(&[][..]));
        let broken =
            system(r#"{"type":"system","subtype":"background_tasks_changed","tasks":"b1"}"#);
        assert_eq!(broken.background_tasks(), None);
        let other = system(r#"{"type":"system","subtype":"status","tasks":[]}"#);
        assert_eq!(other.background_tasks(), None);

        let started = system(
            r#"{"type":"system","subtype":"task_started","task_id":"b1","tool_use_id":"t1",
                "is_backgrounded":true}"#,
        );
        assert_eq!(started.task_id.as_deref(), Some("b1"));
        assert_eq!(started.tool_use_id.as_deref(), Some("t1"));
    }

    #[test]
    fn control_error_response_shape() {
        assert_eq!(
            control_error_response("r1", "nope"),
            json!({"type": "control_response", "response": {
                "subtype": "error", "request_id": "r1", "error": "nope"}})
        );
    }

    #[test]
    fn control_cancel_request_is_parsed() {
        assert_eq!(
            Incoming::parse(r#"{"type":"control_cancel_request","request_id":"r9"}"#),
            Incoming::ControlCancelRequest {
                request_id: "r9".to_string()
            }
        );
        // Without an id there is nothing to cancel.
        assert!(matches!(
            Incoming::parse(r#"{"type":"control_cancel_request"}"#),
            Incoming::Unknown(_)
        ));
    }

    #[test]
    fn deny_always_has_a_message() {
        let deny = permission_response(&bash_request(vec![]), PermissionAnswer::Deny);
        assert_eq!(deny["response"]["response"]["message"], DENY_MESSAGE);
        assert!(deny["response"]["response"].get("interrupt").is_none());

        let stop = permission_response(&bash_request(vec![]), PermissionAnswer::DenyAndStop);
        assert_eq!(stop["response"]["response"]["message"], DENY_MESSAGE);
        assert_eq!(stop["response"]["response"]["interrupt"], true);
    }

    #[test]
    fn interrupt_and_user_message_shapes() {
        assert_eq!(
            interrupt_request("int_1"),
            json!({"type": "control_request", "request_id": "int_1",
                   "request": {"subtype": "interrupt"}})
        );
        assert_eq!(
            user_message("hi"),
            json!({"type": "user", "message": {"role": "user", "content": "hi"}})
        );
    }
}
