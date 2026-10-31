use serde_json::{Value, json};

use super::{CodecOutput, CodexCodec, wire::*};
use crate::agent::*;

#[derive(Debug)]
pub(super) enum CacheScope {
    Once,
    Exact { command: String, cwd: String },
}
#[derive(Debug)]
pub(super) enum Action {
    Command(CacheScope),
    FileChange,
}
#[derive(Debug)]
pub(super) struct PendingApproval {
    pub rpc_id: RpcId,
    pub approval: Approval,
    pub action: Action,
    pub view: ApprovalRequest,
}

impl CodexCodec {
    pub(super) fn server_request(
        &mut self,
        method: &str,
        id: RpcId,
        params: Value,
        out: &mut CodecOutput,
    ) {
        let supported = matches!(
            method,
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
        );
        if !supported {
            out.messages.push(json!({"id":id,"error":{"code":-32601,"message":"Newjen does not support this server request"}}));
            self.fail(
                "Unsupported agent request",
                "Codex requested an action Newjen cannot safely handle.",
                out,
            );
            return;
        }
        let approval: Approval = match serde_json::from_value(params) {
            Ok(a) => a,
            Err(_) => {
                out.messages.push(json!({"id":id,"error":{"code":-32602,"message":"Incomplete approval request"}}));
                self.fail(
                    "Invalid approval request",
                    "Codex supplied an unreadable approval request.",
                    out,
                );
                return;
            }
        };
        if !self.active(&approval.thread_id, &approval.turn_id)
            || self.finalized.contains(&approval.item_id)
            || self.pending.values().any(|p| p.rpc_id == id)
        {
            out.messages.push(json!({"id":id,"error":{"code":-32600,"message":"Approval is not active or its id is already pending"}}));
            return;
        }
        let action = if method == "item/fileChange/requestApproval" {
            Action::FileChange
        } else {
            let scope = match (&approval.kind, &approval.command, &approval.cwd) {
                (CommandKind::Command, Some(command), Some(cwd))
                    if !command.trim().is_empty()
                        && std::path::Path::new(cwd).is_absolute()
                        && approval.network.is_none()
                        && approval
                            .environment
                            .as_deref()
                            .is_none_or(|environment| environment == "local") =>
                {
                    CacheScope::Exact {
                        command: command.clone(),
                        cwd: cwd.clone(),
                    }
                }
                _ => CacheScope::Once,
            };
            Action::Command(scope)
        };
        let tool = self
            .tools
            .get(&approval.item_id)
            .cloned()
            .unwrap_or_else(|| {
                let (name, summary, arguments) = match &action {
                    Action::Command(_) => (
                        "Command",
                        approval
                            .command
                            .clone()
                            .unwrap_or_else(|| "Command details unavailable".into()),
                        json!({"command":approval.command,"cwd":approval.cwd}),
                    ),
                    Action::FileChange => (
                        "File change",
                        "Change preview unavailable".into(),
                        Value::Null,
                    ),
                };
                ToolDescriptor {
                    id: approval.item_id.clone(),
                    name: name.into(),
                    display_name: name.into(),
                    summary,
                    arguments,
                }
            });
        self.tools.insert(tool.id.clone(), tool.clone());
        let (kind, remember, question) = match &action {
            Action::FileChange => (
                ApprovalKind::FileChange {
                    path: file_path(&tool),
                },
                RememberScope::OnceOnly(OnceReason::FileChanges),
                "Allow these file changes once?".into(),
            ),
            Action::Command(scope) => {
                let remember = match scope {
                    CacheScope::Once => RememberScope::OnceOnly(OnceReason::NoSafeScope),
                    CacheScope::Exact { command, cwd } => RememberScope::Rules {
                        labels: vec![format!("{command} (in {cwd}; this session only)")],
                        meaning: RuleMeaning::Exact,
                        shown_in_detail: false,
                    },
                };
                let question = match &approval.cwd {
                    Some(cwd) => format!("Allow this command in {cwd}?"),
                    None => "Allow this command? Working directory unavailable.".into(),
                };
                (
                    ApprovalKind::Command {
                        command: approval.command.clone(),
                        purpose: approval.reason.clone(),
                    },
                    remember,
                    question,
                )
            }
        };
        self.next_approval += 1;
        let key = ApprovalKey {
            namespace: self.namespace,
            serial: self.next_approval,
        };
        let view = ApprovalRequest {
            key,
            view: ApprovalView {
                tool: tool.clone(),
                kind,
                question,
                affected_path: None,
                remember,
            },
        };
        self.pending.insert(
            key,
            PendingApproval {
                rpc_id: id,
                approval,
                action,
                view: view.clone(),
            },
        );
        out.event(AgentEvent::ToolUpsert(tool));
        out.event(AgentEvent::ApprovalRequested(view));
    }

    /// Consumes a live callback exactly once. A stale key has no wire or transcript effect.
    pub fn answer(&mut self, key: ApprovalKey, intent: ApprovalIntent) -> Option<CodecOutput> {
        let pending = self.pending.remove(&key)?;
        if !self.active(&pending.approval.thread_id, &pending.approval.turn_id) {
            return None;
        }
        let mut out = CodecOutput::default();
        let decision = match intent {
            ApprovalIntent::AllowOnce => ApprovalDecision::Accept,
            ApprovalIntent::AllowForSession => match &pending.action {
                Action::Command(CacheScope::Exact { command, cwd }) => {
                    self.grants.push(GrantView {
                        id:GrantId(format!("{}:{}",self.namespace,key.serial)), tool:"Command".into(),
                        shown:format!("{command} (in {cwd})"),
                        note:Some("Exact command; ends when this process stops. Revoking stops the process and clears all its grants.".into()),
                        lifetime:GrantLifetime::Process,
                    });
                    ApprovalDecision::AcceptForSession
                }
                Action::Command(CacheScope::Once) | Action::FileChange => ApprovalDecision::Accept,
            },
            ApprovalIntent::Deny => ApprovalDecision::Decline,
            ApprovalIntent::DenyAndStop => ApprovalDecision::Cancel,
        };
        out.messages
            .push(json!({"id":pending.rpc_id,"result":{"decision":decision}}));
        out.event(AgentEvent::ApprovalAnswered { key, intent });
        if intent == ApprovalIntent::DenyAndStop {
            let stop = self.interrupt();
            out.messages.extend(stop.messages);
            out.effects.extend(stop.effects);
            out.batch.events.extend(stop.batch.events);
        }
        Some(out)
    }
    pub(super) fn update_approvals(&mut self, tool: &ToolDescriptor, out: &mut CodecOutput) {
        for pending in self
            .pending
            .values_mut()
            .filter(|p| p.approval.item_id == tool.id)
        {
            pending.view.view.tool = tool.clone();
            if let Action::FileChange = pending.action {
                pending.view.view.kind = ApprovalKind::FileChange {
                    path: file_path(tool),
                };
            }
            out.event(AgentEvent::ApprovalUpdated(pending.view.clone()));
        }
    }
}
fn file_path(tool: &ToolDescriptor) -> Option<String> {
    tool.arguments
        .as_array()?
        .first()?
        .get("path")?
        .as_str()
        .map(str::to_owned)
}
