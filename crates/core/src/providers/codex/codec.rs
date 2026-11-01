use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{approvals::PendingApproval, wire::*};
use crate::agent::*;
use crate::providers::detect::{CodexAuth, Initialize, ModelPage};
use crate::providers::rpc;
use crate::providers::{Account, ModelCatalog, ModelOption, ModelSelection, Provider};

static NEXT_NAMESPACE: AtomicU64 = AtomicU64::new(1 << 63);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexRuntime {
    Initializing,
    Opening,
    Ready { thread_id: String },
    StartingTurn { thread_id: String },
    Running { thread_id: String, turn_id: String },
    Stopping { thread_id: String, turn_id: String },
    Blocked,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEffect {
    /// Terminate the process group; no further work may run in this generation.
    Shutdown,
    /// Bound turn completion even after the interrupt RPC is acknowledged.
    ArmStopDeadline,
    /// RPC acknowledgement only; retain the completion deadline.
    StopAcknowledged,
    /// Clear the stop deadline.
    TurnEnded,
}

#[derive(Debug)]
pub struct CodecOutput {
    pub batch: AgentBatch,
    pub messages: Vec<Value>,
    pub effects: Vec<ControlEffect>,
}
impl Default for CodecOutput {
    fn default() -> Self {
        Self {
            batch: AgentBatch {
                events: Vec::new(),
                checkpoint: Checkpoint::Transient,
            },
            messages: Vec::new(),
            effects: Vec::new(),
        }
    }
}
impl CodecOutput {
    pub(super) fn event(&mut self, event: AgentEvent) {
        self.batch.events.push(event);
        self.batch.checkpoint = Checkpoint::Save;
    }
}

#[derive(Debug)]
pub(super) enum PendingRpc {
    Initialize,
    Account,
    Models,
    Limits,
    Open,
    StartTurn,
    Interrupt,
}

pub struct CodexCodec {
    pub(super) state: CodexRuntime,
    config: AgentConfig,
    pub(super) cwd: PathBuf,
    resume: Option<String>,
    instructions: Option<String>,
    next_rpc: i64,
    rpc: HashMap<RpcId, PendingRpc>,
    pub(super) namespace: u64,
    pub(super) next_approval: u64,
    pub(super) pending: HashMap<ApprovalKey, PendingApproval>,
    pub(super) grants: Vec<GrantView>,
    pub(super) tools: HashMap<String, ToolDescriptor>,
    pub(super) outputs: HashMap<String, String>,
    pub(super) finalized: std::collections::HashSet<String>,
    pub(super) content: HashMap<String, ContentKey>,
    next_content: u64,
    catalog: ModelCatalog,
    cursors: Vec<String>,
    account: Option<Account>,
    pub(super) limits: HashMap<String, Value>,
    pub(super) ordinary_usage_allowed: Option<bool>,
    tier: String,
}
impl CodexCodec {
    /// Constructing a codec authorizes initialization only, never a model turn.
    pub fn new(
        config: AgentConfig,
        cwd: PathBuf,
        resume: Option<String>,
    ) -> Result<(Self, CodecOutput)> {
        ensure!(
            config.provider == Provider::Codex,
            "The config belongs to another provider"
        );
        ensure!(
            cwd.is_absolute(),
            "A thread needs an absolute working directory"
        );
        ensure!(
            resume.as_ref().is_none_or(|id| !id.trim().is_empty()),
            "Empty conversation id"
        );
        let mut codec = Self {
            state: CodexRuntime::Initializing,
            config,
            cwd,
            resume,
            instructions: None,
            next_rpc: 0,
            rpc: HashMap::new(),
            namespace: NEXT_NAMESPACE.fetch_add(1, Ordering::Relaxed),
            next_approval: 0,
            pending: HashMap::new(),
            grants: Vec::new(),
            tools: HashMap::new(),
            outputs: HashMap::new(),
            finalized: Default::default(),
            content: HashMap::new(),
            next_content: 0,
            catalog: ModelCatalog {
                source: crate::providers::CatalogSource::AccountModels,
                models: Vec::new(),
            },
            cursors: Vec::new(),
            account: None,
            limits: HashMap::new(),
            ordinary_usage_allowed: None,
            tier: "default".into(),
        };
        let mut out = CodecOutput::default();
        let id = codec.reserve(PendingRpc::Initialize);
        out.messages.push(rpc::initialize(&id));
        Ok((codec, out))
    }
    /// Newjen's own instructions for the thread, sent as `developerInstructions` when it is
    /// started or resumed. Set them before the initialize response arrives.
    pub fn set_instructions(&mut self, instructions: Option<String>) {
        self.instructions = instructions.filter(|text| !text.trim().is_empty());
    }
    pub fn runtime(&self) -> &CodexRuntime {
        &self.state
    }
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }
    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }
    pub fn grants(&self) -> &[GrantView] {
        &self.grants
    }

    fn reserve(&mut self, pending: PendingRpc) -> RpcId {
        self.next_rpc += 1;
        let id = RpcId::Integer(self.next_rpc);
        self.rpc.insert(id.clone(), pending);
        id
    }
    pub(super) fn request(
        &mut self,
        pending: PendingRpc,
        method: &str,
        params: Value,
        out: &mut CodecOutput,
    ) {
        let id = self.reserve(pending);
        out.messages.push(rpc::request(&id, method, params));
    }
    pub fn stdout(&mut self, line: &str) -> CodecOutput {
        let mut out = CodecOutput::default();
        if let Err(error) = self.receive(line, &mut out) {
            self.fail("Codex protocol error", &format!("{error:#}"), &mut out);
        }
        out
    }
    fn receive(&mut self, line: &str, out: &mut CodecOutput) -> Result<()> {
        ensure!(line.len() <= 4 * 1024 * 1024, "Oversized app-server line");
        let envelope: Envelope = serde_json::from_str(line).context("Invalid app-server JSON")?;
        match (envelope.method, envelope.id) {
            (Some(method), Some(id)) => self.server_request(&method, id, envelope.params, out),
            (Some(method), None) => {
                if !matches!(self.state, CodexRuntime::Blocked | CodexRuntime::Closed) {
                    self.notification(&method, envelope.params, out)?;
                }
            }
            (None, Some(id)) => {
                if let Some(pending) = self.rpc.remove(&id) {
                    if let Some(error) = envelope.error {
                        if matches!(pending, PendingRpc::Limits) {
                            self.open(out)?;
                        } else {
                            self.fail("Codex request failed", &error.to_string(), out);
                        }
                    } else {
                        self.response(
                            pending,
                            envelope.result.context("Response missing result")?,
                            out,
                        )?;
                    }
                }
            }
            (None, None) => bail!("App-server message has neither method nor id"),
        }
        Ok(())
    }
    fn response(&mut self, pending: PendingRpc, value: Value, out: &mut CodecOutput) -> Result<()> {
        match pending {
            PendingRpc::Initialize => {
                let _: Initialize = serde_json::from_value(value)?;
                ensure!(
                    self.state == CodexRuntime::Initializing,
                    "Unexpected initialize response"
                );
                out.messages.push(rpc::initialized());
                self.request(
                    PendingRpc::Account,
                    "account/read",
                    json!({"refreshToken":false}),
                    out,
                );
            }
            PendingRpc::Account => {
                let auth: CodexAuth = serde_json::from_value(value)?;
                ensure!(
                    auth.account.is_some() || !auth.requires_openai_auth,
                    "Run codex login in your terminal, then retry"
                );
                self.account = auth.account.map(Account::from);
                self.request(PendingRpc::Models, "model/list", json!({}), out);
            }
            PendingRpc::Models => {
                let page: ModelPage = serde_json::from_value(value)?;
                self.catalog.models.extend(
                    page.data
                        .into_iter()
                        .filter(|m| !m.hidden)
                        .map(ModelOption::from),
                );
                if let Some(cursor) = page.next_cursor {
                    ensure!(
                        !self.cursors.contains(&cursor) && self.cursors.len() < 100,
                        "Invalid model pagination"
                    );
                    self.cursors.push(cursor.clone());
                    self.request(
                        PendingRpc::Models,
                        "model/list",
                        json!({"cursor":cursor}),
                        out,
                    );
                } else {
                    self.validate_config()?;
                    self.request(
                        PendingRpc::Limits,
                        "account/rateLimits/read",
                        json!({}),
                        out,
                    );
                }
            }
            PendingRpc::Limits => {
                self.rate_limits(&value, out)?;
                if !matches!(self.state, CodexRuntime::Blocked) {
                    self.open(out)?;
                }
            }
            PendingRpc::Open => {
                let opened: Opened = serde_json::from_value(value)?;
                ensure!(
                    self.state == CodexRuntime::Opening,
                    "Unexpected thread open response"
                );
                ensure!(
                    opened.approval_policy == "untrusted"
                        && opened.approvals_reviewer == "user"
                        && opened.sandbox.kind == "readOnly"
                        && !opened.sandbox.network_access,
                    "Codex did not honor the requested approval and sandbox policy"
                );
                ensure!(
                    !opened.thread.id.is_empty(),
                    "Missing server conversation id"
                );
                ensure!(
                    self.resume
                        .as_ref()
                        .is_none_or(|id| id == &opened.thread.id),
                    "Resumed a different conversation"
                );
                ensure!(
                    std::path::Path::new(&opened.cwd) == self.cwd,
                    "Codex changed the thread's working directory"
                );
                self.validate_effective_model(&opened.model)?;
                ensure!(
                    opened.service_tier.as_deref() == Some(self.tier.as_str()),
                    "Codex did not honor the requested speed tier"
                );
                self.state = CodexRuntime::Ready {
                    thread_id: opened.thread.id.clone(),
                };
                out.event(AgentEvent::ConversationEstablished(opened.thread.id));
                out.event(AgentEvent::ModelReported(opened.model));
                out.event(AgentEvent::WorkingDirectoryReported(self.cwd.clone()));
            }
            PendingRpc::StartTurn => {
                let turn: Turn =
                    serde_json::from_value(value.get("turn").context("Missing turn")?.clone())?;
                let thread_id = match &self.state {
                    CodexRuntime::StartingTurn { thread_id } => thread_id.clone(),
                    CodexRuntime::Running { turn_id, .. }
                    | CodexRuntime::Stopping { turn_id, .. }
                        if turn_id == &turn.id =>
                    {
                        return Ok(());
                    }
                    CodexRuntime::Ready { .. } => return Ok(()), // Completion may precede the RPC reply.
                    _ => bail!("Unexpected turn/start response"),
                };
                self.started(&thread_id, turn, out)?;
            }
            PendingRpc::Interrupt => out.effects.push(ControlEffect::StopAcknowledged),
        }
        Ok(())
    }
    fn validate_config(&mut self) -> Result<()> {
        match &self.config.model {
            ModelSelection::CliDefault => {
                ensure!(
                    self.config.effort == EffortSelection::CliDefault
                        && self.config.speed == Speed::Normal,
                    "Choose a catalog model before overriding effort or speed"
                );
            }
            ModelSelection::Model { id } => {
                let model = self
                    .catalog
                    .models
                    .iter()
                    .find(|m| &m.id == id)
                    .context("Model is not advertised by Codex")?;
                if let EffortSelection::Chosen(effort) = &self.config.effort {
                    ensure!(
                        !effort.is_empty() && model.efforts.contains(effort),
                        "Unsupported reasoning effort"
                    );
                }
                self.tier = match self.config.speed {
                    Speed::Normal => "default".into(),
                    Speed::Fast => model
                        .fast
                        .as_ref()
                        .filter(|tier| model.service_tiers.contains(tier))
                        .context("This model does not advertise a fast service tier")?
                        .id
                        .clone(),
                };
            }
        }
        Ok(())
    }
    fn validate_effective_model(&self, model: &str) -> Result<()> {
        if let ModelSelection::Model { id } = &self.config.model {
            ensure!(id == model, "Codex selected a different model");
        }
        Ok(())
    }
    fn open(&mut self, out: &mut CodecOutput) -> Result<()> {
        let mut params = json!({"cwd":self.cwd,"approvalPolicy":"untrusted","sandbox":"read-only","approvalsReviewer":"user","serviceTier":self.tier});
        if let ModelSelection::Model { id } = &self.config.model {
            params["model"] = json!(id);
        }
        if let Some(instructions) = &self.instructions {
            params["developerInstructions"] = json!(instructions);
        }
        let method = match &self.resume {
            Some(id) => {
                params["threadId"] = json!(id);
                "thread/resume"
            }
            None => "thread/start",
        };
        self.state = CodexRuntime::Opening;
        self.request(PendingRpc::Open, method, params, out);
        Ok(())
    }
    /// The only operation that emits turn/start. Busy/blocked codecs reject the prompt.
    pub fn prompt(&mut self, text: &str) -> Result<CodecOutput> {
        let CodexRuntime::Ready { thread_id } = &self.state else {
            bail!("Codex is not ready for a user prompt");
        };
        ensure!(!text.trim().is_empty(), "Empty prompt");
        let thread_id = thread_id.clone();
        let mut params = json!({"threadId":thread_id,"input":[{"type":"text","text":text,"text_elements":[]}],"serviceTierForTurn":self.tier});
        if let ModelSelection::Model { id } = &self.config.model {
            params["model"] = json!(id);
        }
        if let EffortSelection::Chosen(effort) = &self.config.effort {
            params["effort"] = json!(effort);
        }
        self.tools.clear();
        self.outputs.clear();
        self.content.clear();
        self.finalized.clear();
        self.state = CodexRuntime::StartingTurn { thread_id };
        let mut out = CodecOutput::default();
        self.request(PendingRpc::StartTurn, "turn/start", params, &mut out);
        Ok(out)
    }
    pub(super) fn active(&self, thread: &str, turn: &str) -> bool {
        match &self.state {
            CodexRuntime::Running { thread_id, turn_id }
            | CodexRuntime::Stopping { thread_id, turn_id } => {
                thread == thread_id && turn == turn_id
            }
            _ => false,
        }
    }
    fn started(&mut self, thread: &str, turn: Turn, out: &mut CodecOutput) -> Result<()> {
        if self.active(thread, &turn.id) {
            return Ok(());
        }
        ensure!(
            matches!(&self.state, CodexRuntime::StartingTurn { thread_id } if thread_id == thread),
            "Unsolicited Codex turn"
        );
        ensure!(
            !turn.id.is_empty() && matches!(turn.status, Status::InProgress),
            "Invalid started turn"
        );
        self.state = CodexRuntime::Running {
            thread_id: thread.into(),
            turn_id: turn.id,
        };
        out.event(AgentEvent::TurnActive);
        out.event(AgentEvent::UserConfirmed);
        Ok(())
    }
    fn notification(&mut self, method: &str, value: Value, out: &mut CodecOutput) -> Result<()> {
        match method {
            "turn/started" => {
                let n: TurnNotification = serde_json::from_value(value)?;
                self.started(&n.thread_id, n.turn, out)?;
            }
            "turn/completed" => {
                let n: TurnNotification = serde_json::from_value(value)?;
                if !self.active(&n.thread_id, &n.turn.id) {
                    return Ok(());
                }
                let outcome = match n.turn.status {
                    Status::Completed => TurnOutcome::Completed,
                    Status::Interrupted => {
                        out.event(AgentEvent::ReplyInterrupted);
                        TurnOutcome::Stopped
                    }
                    Status::Failed => {
                        let error = n.turn.error.unwrap_or(TurnError {
                            message: "Codex turn failed".into(),
                            info: None,
                        });
                        self.provider_error(error, false, out);
                        return Ok(());
                    }
                    Status::InProgress | Status::Declined | Status::Unknown => {
                        bail!("Nonterminal status in turn/completed")
                    }
                };
                self.settle_pending(out);
                out.event(finished(outcome, n.turn.duration_ms));
                out.effects.push(ControlEffect::TurnEnded);
                self.state = CodexRuntime::Ready {
                    thread_id: n.thread_id,
                };
            }
            "item/started" | "item/completed" => {
                let n: ItemNotification = serde_json::from_value(value)?;
                if self.active(&n.thread_id, &n.turn_id) {
                    self.item(n.item, method == "item/completed", out)?;
                }
            }
            "item/agentMessage/delta"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta" => {
                let n: Delta = serde_json::from_value(value)?;
                if n.delta.is_empty()
                    || !self.active(&n.thread_id, &n.turn_id)
                    || self.finalized.contains(&n.item_id)
                {
                    return Ok(());
                }
                if method == "item/agentMessage/delta" {
                    let key = self.content_key(&n.item_id);
                    out.event(AgentEvent::Text {
                        key,
                        update: TextUpdate::Append(n.delta),
                    });
                } else {
                    self.outputs
                        .entry(n.item_id.clone())
                        .or_default()
                        .push_str(&n.delta);
                    out.event(AgentEvent::ToolOutput {
                        id: n.item_id,
                        update: TextUpdate::Append(n.delta),
                    });
                }
                out.batch.checkpoint = Checkpoint::Transient;
            }
            "item/fileChange/patchUpdated" => self.patch_updated(value, out)?,
            "serverRequest/resolved" => {
                let n: Resolved = serde_json::from_value(value)?;
                if let Some(key) = self
                    .pending
                    .iter()
                    .find(|(_, p)| p.rpc_id == n.request_id && p.approval.thread_id == n.thread_id)
                    .map(|(k, _)| *k)
                {
                    self.pending.remove(&key);
                    out.event(AgentEvent::ApprovalResolved(key));
                }
            }
            "thread/tokenUsage/updated" => {
                let n: UsageNotification = serde_json::from_value(value)?;
                if self.active(&n.thread_id, &n.turn_id) {
                    out.event(AgentEvent::UsageUpdated(crate::agent::Usage {
                        last: n.usage.last.into(),
                        total: n.usage.total.into(),
                        context_window: n.usage.context_window,
                    }));
                }
            }
            "account/rateLimits/updated" => self.rate_limits(&value, out)?,
            "error" => {
                let n: ErrorNotification = serde_json::from_value(value)?;
                if self.active(&n.thread_id, &n.turn_id) {
                    self.provider_error(n.error, n.will_retry, out);
                }
            }
            "thread/settings/updated" => {
                let settings = value
                    .get("threadSettings")
                    .context("Missing thread settings")?;
                ensure!(
                    settings["approvalPolicy"] == "untrusted"
                        && settings["approvalsReviewer"] == "user"
                        && settings["sandboxPolicy"]["type"] == "readOnly"
                        && settings["sandboxPolicy"]["networkAccess"].as_bool() != Some(true),
                    "Codex changed the approval or sandbox policy"
                );
                ensure!(
                    settings["cwd"]
                        .as_str()
                        .is_some_and(|cwd| std::path::Path::new(cwd) == self.cwd),
                    "Codex changed the working directory"
                );
                self.validate_effective_model(
                    settings["model"]
                        .as_str()
                        .context("Missing effective model")?,
                )?;
                ensure!(
                    settings["serviceTier"].as_str() == Some(self.tier.as_str()),
                    "Codex changed the speed tier"
                );
            }
            "thread/closed" => self.fail(
                "Codex thread closed",
                "The server closed the conversation.",
                out,
            ),
            "model/rerouted" | "thread/model/rerouted" => self.fail(
                "Model changed",
                "Codex rerouted the selected model. Choose a model and retry manually.",
                out,
            ),
            _ => (),
        }
        Ok(())
    }
    pub(super) fn content_key(&mut self, id: &str) -> ContentKey {
        *self.content.entry(id.into()).or_insert_with(|| {
            self.next_content += 1;
            ContentKey(self.next_content)
        })
    }
    /// Terminates the process session, expiring every process-scoped grant and callback.
    pub fn shutdown(&mut self) -> CodecOutput {
        let mut out = CodecOutput::default();
        let active = matches!(
            self.state,
            CodexRuntime::StartingTurn { .. }
                | CodexRuntime::Running { .. }
                | CodexRuntime::Stopping { .. }
        );
        self.close(&mut out);
        if active {
            out.event(finished(TurnOutcome::Stopped, None));
        }
        out
    }

    pub fn interrupt(&mut self) -> CodecOutput {
        let mut out = CodecOutput::default();
        match self.state.clone() {
            CodexRuntime::Running { thread_id, turn_id } => {
                self.state = CodexRuntime::Stopping {
                    thread_id: thread_id.clone(),
                    turn_id: turn_id.clone(),
                };
                self.request(
                    PendingRpc::Interrupt,
                    "turn/interrupt",
                    json!({"threadId":thread_id,"turnId":turn_id}),
                    &mut out,
                );
                out.effects.push(ControlEffect::ArmStopDeadline);
            }
            CodexRuntime::Stopping { .. } | CodexRuntime::Closed => (),
            CodexRuntime::Blocked => {
                self.rpc.clear();
                self.grants.clear();
                out.effects.push(ControlEffect::Shutdown);
            }
            CodexRuntime::Initializing
            | CodexRuntime::Opening
            | CodexRuntime::StartingTurn { .. }
            | CodexRuntime::Ready { .. } => {
                self.close(&mut out);
                out.event(finished(TurnOutcome::Stopped, None));
            }
        }
        out
    }
    /// Call on an expired stop deadline or transport failure; the caller must terminate stdio.
    pub fn transport_failed(&mut self, detail: &str) -> CodecOutput {
        let mut out = CodecOutput::default();
        if !matches!(self.state, CodexRuntime::Blocked | CodexRuntime::Closed) {
            self.fail("Codex connection stopped", detail, &mut out);
        }
        self.rpc.clear();
        self.pending.clear();
        self.grants.clear();
        out
    }
    pub(super) fn settle_pending(&mut self, out: &mut CodecOutput) {
        for (key, _) in self.pending.drain() {
            out.event(AgentEvent::ApprovalResolved(key));
        }
    }
    fn close(&mut self, out: &mut CodecOutput) {
        self.settle_pending(out);
        self.rpc.clear();
        self.grants.clear();
        self.state = CodexRuntime::Closed;
        out.effects.push(ControlEffect::Shutdown);
    }
    pub(super) fn fail(&mut self, title: &str, detail: &str, out: &mut CodecOutput) {
        if matches!(self.state, CodexRuntime::Blocked | CodexRuntime::Closed) {
            return;
        }
        self.settle_pending(out);
        self.rpc.clear();
        self.grants.clear();
        self.state = CodexRuntime::Blocked;
        out.event(finished(
            TurnOutcome::Failed {
                title: title.into(),
                detail: detail.into(),
            },
            None,
        ));
        out.effects.push(ControlEffect::Shutdown);
        out.effects.push(ControlEffect::TurnEnded);
    }
    fn provider_error(&mut self, error: TurnError, will_retry: bool, out: &mut CodecOutput) {
        if error.is_limit() {
            out.event(AgentEvent::RateLimit(RateLimitEvent::Rejected(
                self.limit_snapshot("Usage limit reached"),
            )));
            self.fail("Usage limit reached", &error.message, out);
        } else {
            let title = if will_retry {
                "Codex retry stopped"
            } else {
                "Codex turn failed"
            };
            self.fail(title, &error.message, out);
        }
    }
}
pub(super) fn finished(outcome: TurnOutcome, duration_ms: Option<u64>) -> AgentEvent {
    AgentEvent::TurnFinished {
        outcome,
        duration_ms,
        cost_usd: None,
        forget_conversation: false,
        queued: false,
    }
}
impl From<Counts> for TokenCounts {
    fn from(c: Counts) -> Self {
        Self {
            input: c.input,
            cached_input: c.cached_input,
            cache_write_input: c.cache_write_input,
            output: c.output,
            reasoning_output: c.reasoning_output,
            total: c.total,
        }
    }
}
