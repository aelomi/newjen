use serde::Deserialize;
use serde_json::Value;

pub(super) use super::super::rpc::RpcId;

#[derive(Debug, Deserialize)]
pub(super) struct Envelope {
    pub id: Option<RpcId>,
    pub method: Option<String>,
    #[serde(default)]
    pub params: Value,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct Opened {
    pub thread: Thread,
    pub model: String,
    pub cwd: String,
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: String,
    #[serde(rename = "approvalsReviewer")]
    pub approvals_reviewer: String,
    pub sandbox: Sandbox,
    #[serde(rename = "serviceTier")]
    pub service_tier: Option<String>,
}
#[derive(Debug, Deserialize)]
pub(super) struct Thread {
    pub id: String,
}
#[derive(Debug, Deserialize)]
pub(super) struct Sandbox {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, rename = "networkAccess")]
    pub network_access: bool,
}
#[derive(Debug, Clone, Copy, Deserialize)]
pub(super) enum Status {
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "completed")]
    Completed,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "declined")]
    Declined,
    #[serde(rename = "interrupted")]
    Interrupted,
    #[serde(other)]
    Unknown,
}
#[derive(Debug, Deserialize)]
pub(super) struct Turn {
    pub id: String,
    pub status: Status,
    pub error: Option<TurnError>,
    #[serde(rename = "durationMs")]
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Deserialize)]
pub(super) struct TurnNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub turn: Turn,
}
#[derive(Debug, Deserialize)]
pub(super) struct TurnError {
    pub message: String,
    #[serde(rename = "codexErrorInfo")]
    pub info: Option<Value>,
}
impl TurnError {
    pub fn is_limit(&self) -> bool {
        self.info.as_ref().is_some_and(|info| {
            matches!(
                info.as_str(),
                Some("usageLimitExceeded" | "rateLimitExceeded" | "sessionBudgetExceeded")
            ) || info.to_string().contains("\"httpStatusCode\":429")
        })
    }
}
#[derive(Debug, Deserialize)]
pub(super) struct ErrorNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub error: TurnError,
    #[serde(rename = "willRetry")]
    pub will_retry: bool,
}
#[derive(Debug, Deserialize)]
pub(super) struct ItemNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    pub item: Value,
}
#[derive(Debug, Deserialize)]
pub(super) struct Delta {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    pub delta: String,
}
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Approval {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub reason: Option<String>,
    #[serde(rename = "commandActions")]
    pub actions: Option<Value>,
    #[serde(default)]
    pub kind: CommandKind,
    #[serde(rename = "networkApprovalContext")]
    pub network: Option<Value>,
    #[serde(rename = "environmentId")]
    pub environment: Option<String>,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) enum CommandKind {
    #[default]
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "writeStdin")]
    WriteStdin,
    #[serde(other)]
    Unknown,
}
#[derive(Debug, Deserialize)]
pub(super) struct Resolved {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "requestId")]
    pub request_id: RpcId,
}
#[derive(Debug, Deserialize)]
pub(super) struct UsageNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "tokenUsage")]
    pub usage: Usage,
}
#[derive(Debug, Deserialize)]
pub(super) struct Usage {
    pub last: Counts,
    pub total: Counts,
    #[serde(rename = "modelContextWindow")]
    pub context_window: Option<u64>,
}
#[derive(Debug, Deserialize)]
pub(super) struct Counts {
    #[serde(rename = "inputTokens")]
    pub input: u64,
    #[serde(rename = "cachedInputTokens")]
    pub cached_input: u64,
    #[serde(default, rename = "cacheWriteInputTokens")]
    pub cache_write_input: u64,
    #[serde(rename = "outputTokens")]
    pub output: u64,
    #[serde(rename = "reasoningOutputTokens")]
    pub reasoning_output: u64,
    #[serde(rename = "totalTokens")]
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(super) enum ItemKind {
    #[serde(rename = "agentMessage")]
    AgentMessage,
    #[serde(rename = "userMessage")]
    UserMessage,
    #[serde(rename = "hookPrompt")]
    HookPrompt,
    #[serde(rename = "reasoning")]
    Reasoning,
    #[serde(rename = "plan")]
    Plan,
    #[serde(rename = "commandExecution")]
    Command,
    #[serde(rename = "fileChange")]
    FileChange,
    #[serde(rename = "mcpToolCall")]
    McpTool,
    #[serde(rename = "dynamicToolCall")]
    DynamicTool,
    #[serde(rename = "webSearch")]
    WebSearch,
    #[serde(rename = "imageView")]
    ImageView,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(super) enum ApprovalDecision {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "acceptForSession")]
    AcceptForSession,
    #[serde(rename = "decline")]
    Decline,
    #[serde(rename = "cancel")]
    Cancel,
}
