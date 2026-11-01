use std::fmt::Write as _;

use serde_json::{Value, json};

use super::{CodecOutput, CodexCodec, wire::*};
use crate::agent::*;
use crate::approval_text;
use crate::edits::{EditProposal, FileChange};

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
        let (headline, risks) = summary(&action, &approval, &tool, &self.cwd);
        let view = ApprovalRequest {
            key,
            headline,
            risks,
            model_headline: false,
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
                (pending.view.headline, pending.view.risks) =
                    summary(&pending.action, &pending.approval, tool, &self.cwd);
            }
            out.event(AgentEvent::ApprovalUpdated(pending.view.clone()));
        }
    }
}
fn summary(
    action: &Action,
    approval: &Approval,
    tool: &ToolDescriptor,
    project: &std::path::Path,
) -> (String, Vec<ApprovalRisk>) {
    match action {
        Action::FileChange => file_change_summary(tool, project),
        Action::Command(_) => {
            let mut risks = approval
                .command
                .as_deref()
                .map_or_else(Vec::new, |command| {
                    approval_text::command_risks(
                        command,
                        Some(project),
                        approval.cwd.as_deref().map(std::path::Path::new),
                    )
                });
            if approval.network.is_some() && !risks.contains(&ApprovalRisk::UsesNetwork) {
                risks.push(ApprovalRisk::UsesNetwork);
                risks.sort();
            }
            let headline = if matches!(approval.kind, CommandKind::WriteStdin) {
                "Send input to a running command".to_string()
            } else {
                approval
                    .actions
                    .as_ref()
                    .and_then(action_headline)
                    .unwrap_or_else(|| approval_text::run_headline(approval.command.as_deref()))
            };
            (headline, risks)
        }
    }
}
/// A sentence from Codex's parsed `commandActions`, only when every action is the same known
/// kind (read, search or list); anything else, including `unknown`, gets no sentence.
fn action_headline(actions: &Value) -> Option<String> {
    let actions = actions.as_array().filter(|actions| !actions.is_empty())?;
    let kind = actions[0]["type"].as_str()?;
    if actions
        .iter()
        .any(|action| action["type"].as_str() != Some(kind))
    {
        return None;
    }
    let text = |action: &Value, field: &str| {
        action[field]
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    match kind {
        "read" => {
            let mut names: Vec<String> = actions
                .iter()
                .filter_map(|action| {
                    text(action, "name").or_else(|| {
                        text(action, "path").and_then(|path| approval_text::file_name(&path))
                    })
                })
                .collect();
            names.sort();
            names.dedup();
            match names.as_slice() {
                [] => Some("Read files".to_string()),
                [name] => Some(format!("Read {name}")),
                names => Some(format!("Read {} files", names.len())),
            }
        }
        "search" => match actions.as_slice() {
            [only] => Some(match text(only, "query") {
                Some(query) => format!("Search for \"{query}\""),
                None => "Search files".to_string(),
            }),
            _ => Some("Search files".to_string()),
        },
        "listFiles" | "list_files" => match actions.as_slice() {
            [only] => Some(match text(only, "path").filter(|path| path != ".") {
                Some(path) => format!(
                    "List files in {}",
                    approval_text::file_name(&path).unwrap_or(path)
                ),
                None => "List files".to_string(),
            }),
            _ => Some("List files".to_string()),
        },
        _ => None,
    }
}
fn file_change_summary(
    tool: &ToolDescriptor,
    project: &std::path::Path,
) -> (String, Vec<ApprovalRisk>) {
    let Some(changes) = tool.arguments.as_array().filter(|c| !c.is_empty()) else {
        return ("Edit files".to_string(), Vec::new());
    };
    let kinds: Vec<&str> = changes
        .iter()
        .map(|change| change["kind"]["type"].as_str().unwrap_or("update"))
        .collect();
    let verb = |kind: &str| match kind {
        "add" => "Create",
        "delete" => "Delete",
        _ => "Edit",
    };
    let headline = if let [change] = changes.as_slice() {
        let file = change["path"]
            .as_str()
            .and_then(approval_text::file_name)
            .unwrap_or_else(|| "a file".to_string());
        format!("{} {file}", verb(kinds[0]))
    } else if kinds.iter().all(|kind| *kind == kinds[0]) {
        format!("{} {} files", verb(kinds[0]), changes.len())
    } else {
        format!("Change {} files", changes.len())
    };
    let mut risks = Vec::new();
    if kinds.contains(&"delete") {
        risks.push(ApprovalRisk::DeletesFiles);
    }
    if changes.iter().any(|change| {
        change["path"].as_str().is_some_and(|path| {
            path.starts_with('/') && approval_text::is_outside(project, Some(project), path, None)
        })
    }) {
        risks.push(ApprovalRisk::OutsideProject);
    }
    (headline, risks)
}
fn file_path(tool: &ToolDescriptor) -> Option<String> {
    tool.arguments
        .as_array()?
        .first()?
        .get("path")?
        .as_str()
        .map(str::to_owned)
}

/// The edit a file-change approval proposes, for the preview on the approval card. Codex's
/// `changes` carry one diff per file: hunks for an update, the whole content for an added or
/// deleted file. They become one git-style patch; without them the preview is unavailable.
pub(crate) fn edit_proposal(tool: &ToolDescriptor, cwd: &std::path::Path) -> EditProposal {
    let Some(changes) = tool.arguments.as_array().filter(|c| !c.is_empty()) else {
        return EditProposal {
            path: cwd.to_path_buf(),
            change: FileChange::Unavailable,
        };
    };
    let mut patch = String::new();
    let mut first_path = None;
    for change in changes {
        let (Some(path), Some(diff)) = (change["path"].as_str(), change["diff"].as_str()) else {
            return EditProposal {
                path: cwd.to_path_buf(),
                change: FileChange::Unavailable,
            };
        };
        first_path.get_or_insert_with(|| std::path::PathBuf::from(path));
        let quoted = |prefix: &str| {
            serde_json::to_string(&format!("{prefix}/{}", path.trim_start_matches('/')))
                .expect("string")
        };
        let (old, new) = (quoted("a"), quoted("b"));
        let _ = writeln!(patch, "diff --git {old} {new}");
        let whole = |prefix: char, text: &str| {
            let mut body = String::new();
            for line in text.split_inclusive('\n') {
                body.push(prefix);
                body.push_str(line.strip_suffix('\n').unwrap_or(line));
                body.push('\n');
            }
            if !text.is_empty() && !text.ends_with('\n') {
                body.push_str("\\ No newline at end of file\n");
            }
            (text.lines().count(), body)
        };
        match change["kind"]["type"].as_str() {
            Some("add") => {
                let (lines, body) = whole('+', diff);
                let _ = write!(
                    patch,
                    "--- /dev/null\n+++ {new}\n@@ -0,0 +1,{lines} @@\n{body}"
                );
            }
            Some("delete") => {
                let (lines, body) = whole('-', diff);
                let _ = write!(
                    patch,
                    "--- {old}\n+++ /dev/null\n@@ -1,{lines} +0,0 @@\n{body}"
                );
            }
            _ => {
                // An update may end with "\n\nMoved to: <path>", which is not part of the diff.
                let hunks = diff
                    .split_once("\n\nMoved to: ")
                    .map_or(diff, |(hunks, _)| hunks);
                if !hunks.starts_with("--- ") && !hunks.starts_with("diff --git ") {
                    let _ = writeln!(patch, "--- {old}\n+++ {new}");
                }
                patch.push_str(hunks);
                if !hunks.ends_with('\n') {
                    patch.push('\n');
                }
            }
        }
    }
    EditProposal {
        path: first_path.expect("non-empty changes"),
        change: FileChange::Patch(patch),
    }
}
