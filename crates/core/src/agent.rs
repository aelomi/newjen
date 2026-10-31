use crate::protocol::PermissionRequest;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentKey(pub(crate) u64);
#[derive(Debug, Clone)]
pub enum TextUpdate {
    Append(String),
    Replace(String),
}
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub summary: String,
    pub arguments: Value,
}
#[derive(Debug, Clone)]
pub enum ToolOutcome {
    Completed,
    Denied,
    Failed,
}
#[derive(Debug, Clone)]
pub enum TurnOutcome {
    Completed,
    Stopped,
    Failed { title: String, detail: String },
}
#[derive(Debug, Clone)]
pub struct BackgroundTaskView {
    pub task_id: String,
    pub description: String,
    pub tool: Option<String>,
}
#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnActive,
    UserConfirmed,
    ConversationEstablished(String),
    ModelReported(String),
    WorkingDirectoryReported(PathBuf),
    Text {
        key: ContentKey,
        update: TextUpdate,
    },
    ReplyInterrupted,
    ToolUpsert(ToolDescriptor),
    ToolFinished {
        id: String,
        outcome: ToolOutcome,
        output: String,
        closes_approval: bool,
    },
    ApprovalRequested(PermissionRequest),
    ApprovalWithdrawn(String),
    BackgroundTasks(Vec<BackgroundTaskView>),
    TurnFinished {
        outcome: TurnOutcome,
        duration_ms: Option<u64>,
        cost_usd: Option<f64>,
        forget_conversation: bool,
        queued: bool,
    },
    Stderr(String),
    WriteFailed(String),
    InterruptTimedOut,
    Exited {
        code: Option<i32>,
        requested: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpoint {
    Transient,
    Save,
}
#[derive(Debug, Clone)]
pub struct AgentBatch {
    pub events: Vec<AgentEvent>,
    pub checkpoint: Checkpoint,
}
