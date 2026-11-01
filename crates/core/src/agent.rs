use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentKey(pub(crate) u64);
#[derive(Debug, Clone, PartialEq)]
pub enum TextUpdate {
    Append(String),
    Replace(String),
}
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDescriptor {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub summary: String,
    pub arguments: Value,
}
#[derive(Debug, Clone, PartialEq)]
pub enum ToolOutcome {
    Completed,
    Denied,
    Failed,
}
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    Completed,
    Stopped,
    Failed { title: String, detail: String },
}
#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundTaskView {
    pub task_id: String,
    pub description: String,
    pub tool: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TurnActive,
    ToolOutput {
        id: String,
        update: TextUpdate,
    },
    ToolDuration {
        id: String,
        duration_ms: u64,
    },
    ApprovalUpdated(ApprovalRequest),
    ApprovalAnswered {
        key: ApprovalKey,
        intent: ApprovalIntent,
    },
    UsageUpdated(Usage),
    RateLimit(RateLimitEvent),
    ProviderUsage(crate::usage::ProviderUsage),
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
    },
    ApprovalRequested(ApprovalRequest),
    ApprovalResolved(ApprovalKey),
    ApprovalWithdrawn {
        key: ApprovalKey,
        note: String,
    },
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
#[derive(Debug, Clone, PartialEq)]
pub struct AgentBatch {
    pub events: Vec<AgentEvent>,
    pub checkpoint: Checkpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApprovalKey {
    pub(crate) namespace: u64,
    pub(crate) serial: u64,
}
impl std::fmt::Display for ApprovalKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.namespace, self.serial)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalIntent {
    AllowOnce,
    AllowForSession,
    Deny,
    DenyAndStop,
}
impl ApprovalIntent {
    pub fn is_deny(self) -> bool {
        matches!(self, Self::Deny | Self::DenyAndStop)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnceReason {
    FileChanges,
    NoSafeScope,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleMeaning {
    Exact,
    Arguments,
    Pattern,
    OneTool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RememberScope {
    OnceOnly(OnceReason),
    Rules {
        labels: Vec<String>,
        meaning: RuleMeaning,
        shown_in_detail: bool,
    },
}
impl RememberScope {
    pub fn can_remember(&self) -> bool {
        matches!(self, Self::Rules { .. })
    }
}
/// A short warning on an approval, only ever set when the request itself shows it. Variants run
/// from most to least severe; a request carries every one it shows, most severe first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApprovalRisk {
    DeletesFiles,
    RewritesHistory,
    OutsideProject,
    UsesNetwork,
}
impl ApprovalRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::DeletesFiles => "Deletes files",
            Self::RewritesHistory => "Rewrites history",
            Self::OutsideProject => "Outside the project",
            Self::UsesNetwork => "Uses the network",
        }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum ApprovalKind {
    Command {
        command: Option<String>,
        purpose: Option<String>,
    },
    FileChange {
        path: Option<String>,
    },
    OtherTool,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalView {
    pub tool: ToolDescriptor,
    pub kind: ApprovalKind,
    pub question: String,
    pub affected_path: Option<String>,
    pub remember: RememberScope,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub(crate) key: ApprovalKey,
    pub(crate) view: ApprovalView,
    pub(crate) headline: String,
    pub(crate) risks: Vec<ApprovalRisk>,
    /// The headline is the model's own description of the command, not Newjen's wording.
    pub(crate) model_headline: bool,
}
impl ApprovalRequest {
    pub fn key(&self) -> ApprovalKey {
        self.key
    }
    pub fn view(&self) -> &ApprovalView {
        &self.view
    }
    /// One plain sentence for the card ("Delete 3 files"). `view().question` stays the fixed
    /// question for accessibility text.
    pub fn headline(&self) -> &str {
        &self.headline
    }
    /// The most severe risk the request shows.
    pub fn risk(&self) -> Option<ApprovalRisk> {
        self.risks.first().copied()
    }
    /// Every risk the request shows, most severe first.
    pub fn risks(&self) -> &[ApprovalRisk] {
        &self.risks
    }
    /// Whether the headline was written by the model (Claude's own description of a command),
    /// so the card can say the command below is what actually runs.
    pub fn headline_by_model(&self) -> bool {
        self.model_headline
    }
}
#[derive(Debug)]
pub struct ApprovalReply {
    pub(crate) message: Value,
    pub(crate) grants: Vec<crate::store::Grant>,
}
#[derive(Debug)]
pub struct AnswerReceipt {
    pub(crate) grants: Vec<crate::store::Grant>,
    /// A denial note the provider could not carry with its decision.
    pub(crate) note_kept: Option<String>,
}
/// What became of the note given with a denial (see `Thread::answer_with_note`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteDelivery {
    /// The provider got the note with the denial, or there was nothing to deliver.
    Delivered,
    /// The provider's denial carries no text, so the note comes back for the composer.
    Kept(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantId(pub(crate) String);
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantLifetime {
    Thread,
    Process,
}
#[derive(Debug, Clone, PartialEq)]
pub struct GrantView {
    pub id: GrantId,
    pub tool: String,
    pub shown: String,
    pub note: Option<String>,
    pub lifetime: GrantLifetime,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDisposition {
    Ordinary,
    Expired { note: String },
    Stopped { note: String },
}

#[derive(Debug, Clone, Copy)]
pub struct AgentPresentation {
    pub label: &'static str,
    pub executable_label: &'static str,
    pub working_placeholder: &'static str,
    pub reply_placeholder: &'static str,
    pub empty_title: &'static str,
    pub grants_detail: &'static str,
    pub send_failed: &'static str,
    pub answer_failed: &'static str,
    pub stop_failed: &'static str,
    pub start_failed: &'static str,
    pub installation_hint: &'static str,
    pub unreachable_note: &'static str,
    pub send_detail_prefix: &'static str,
    pub interrupt_title: &'static str,
    pub interrupt_detail: &'static str,
    pub exit_title: &'static str,
    pub exited_tool_note: &'static str,
}
impl AgentPresentation {
    pub fn empty_caption(&self, project: &str) -> String {
        format!(
            "{} runs in {project}. You approve each tool call before it runs.",
            self.executable_label
        )
    }
    pub fn start_failure(
        &self,
        error: &anyhow::Error,
        program: &std::path::Path,
    ) -> (String, String) {
        use std::io;
        let program = program.display();
        let kind = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<io::Error>())
            .map(io::Error::kind);
        let detail = match kind {
            Some(io::ErrorKind::NotFound) => format!(
                "Newjen could not find `{program}`. {}",
                self.installation_hint
            ),
            Some(io::ErrorKind::PermissionDenied) => format!(
                "`{program}` is not allowed to run ({error:#}). Check that it is an executable file, then try again."
            ),
            _ => {
                format!("{error:#}.\n\nCheck that `{program}` runs in a terminal, then try again.")
            }
        };
        (self.start_failed.to_string(), detail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EffortSelection {
    CliDefault,
    Chosen(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Speed {
    Normal,
    Fast,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentConfig {
    pub provider: crate::providers::Provider,
    pub model: crate::providers::ModelSelection,
    pub effort: EffortSelection,
    pub speed: Speed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCounts {
    pub input: u64,
    pub cached_input: u64,
    pub cache_write_input: u64,
    pub output: u64,
    pub reasoning_output: u64,
    pub total: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub last: TokenCounts,
    pub total: TokenCounts,
    pub context_window: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LimitWindow {
    pub label: String,
    pub used_percent: Option<f64>,
    pub resets_at_unix_seconds: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LimitSnapshot {
    pub bucket: String,
    pub windows: Vec<LimitWindow>,
    pub explanation: String,
}
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RateLimitEvent {
    Updated(LimitSnapshot),
    Warning(LimitSnapshot),
    Rejected(LimitSnapshot),
}

impl AgentConfig {
    pub fn cli_default(provider: crate::providers::Provider) -> Self {
        Self {
            provider,
            model: crate::providers::ModelSelection::CliDefault,
            effort: EffortSelection::CliDefault,
            speed: Speed::Normal,
        }
    }
    pub fn validate(&self, status: &crate::providers::ProviderStatus) -> anyhow::Result<()> {
        use crate::providers::{Availability, ModelSelection};
        anyhow::ensure!(
            self.provider == status.provider,
            "The catalog belongs to another provider"
        );
        let Availability::Ready { catalog, .. } = &status.availability else {
            anyhow::bail!(
                "The selected provider is unavailable; check its installation and sign-in"
            );
        };
        match &self.model {
            ModelSelection::CliDefault => anyhow::ensure!(
                self.effort == EffortSelection::CliDefault && self.speed == Speed::Normal,
                "Choose a model before overriding effort or speed"
            ),
            ModelSelection::Model { id } => {
                let model = catalog
                    .models
                    .iter()
                    .find(|model| &model.id == id)
                    .ok_or_else(|| anyhow::anyhow!("The model is not in the provider catalog"))?;
                if let EffortSelection::Chosen(effort) = &self.effort {
                    anyhow::ensure!(model.efforts.contains(effort), "Unsupported effort");
                }
                anyhow::ensure!(
                    self.speed == Speed::Normal || model.fast.is_some(),
                    "This model does not offer fast mode"
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationState {
    Uncreated,
    Reserved(String),
    Established(String),
}
impl ConversationState {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Uncreated => None,
            Self::Reserved(id) | Self::Established(id) => Some(id),
        }
    }
    pub(crate) fn stored_state(&self) -> &'static str {
        match self {
            Self::Uncreated => "uncreated",
            Self::Reserved(_) => "reserved",
            Self::Established(_) => "established",
        }
    }
    pub(crate) fn parse(state: &str, id: Option<String>) -> anyhow::Result<Self> {
        match (state, id) {
            ("uncreated", None) => Ok(Self::Uncreated),
            ("reserved", Some(id)) if !id.trim().is_empty() => Ok(Self::Reserved(id)),
            ("established", Some(id)) if !id.trim().is_empty() => Ok(Self::Established(id)),
            _ => anyhow::bail!("Invalid saved conversation state"),
        }
    }
}
