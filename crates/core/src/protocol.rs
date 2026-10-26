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
    /// `type: "rate_limit_event"`: usage-limit info. Kept raw; Newjen does not use it yet.
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
}

/// Per-model usage inside a result.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct ModelUsage {
    #[serde(rename = "costUSD")]
    pub cost_usd: Option<f64>,
}

impl ResultMessage {
    /// True when the turn ended because the user stopped it (interrupt, or deny-and-stop).
    /// That is not an error, even though the CLI marks it `is_error`.
    pub fn was_stopped(&self) -> bool {
        self.terminal_reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("aborted"))
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
        })
    }
}

/// How the user answered an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionAnswer {
    /// Run this one call.
    AllowOnce,
    /// Run it, and auto-approve identical calls for the rest of this session (nothing is
    /// written to the project's settings).
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
            let rules = session_rules(&request.suggestions);
            // Without an `addRules` suggestion there is nothing to remember: allow once.
            if !rules.is_empty() {
                decision["updatedPermissions"] = Value::Array(rules);
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

fn allow(request: &PermissionRequest) -> Value {
    json!({ "behavior": "allow", "updatedInput": request.input })
}

/// Keeps only the `addRules` suggestions and changes their destination to `session`, so the
/// rule lasts until the CLI exits and is never written into the project's settings files.
fn session_rules(suggestions: &[Value]) -> Vec<Value> {
    suggestions
        .iter()
        .filter(|suggestion| suggestion.get("type").and_then(Value::as_str) == Some("addRules"))
        .map(|suggestion| {
            let mut rule = suggestion.clone();
            rule["destination"] = Value::String("session".to_string());
            rule
        })
        .collect()
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
        }
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

    #[test]
    fn allow_for_session_without_rules_is_allow_once() {
        let reply = permission_response(&bash_request(vec![]), PermissionAnswer::AllowForSession);
        assert_eq!(
            reply,
            permission_response(&bash_request(vec![]), PermissionAnswer::AllowOnce)
        );
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
