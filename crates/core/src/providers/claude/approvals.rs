use crate::agent::{
    ApprovalIntent, ApprovalKey, ApprovalKind, ApprovalReply, ApprovalRequest, ApprovalView,
    OnceReason, RememberScope, RuleMeaning,
};
use crate::protocol::{self, PermissionAnswer, PermissionRequest, SessionScope};
use crate::store::Grant;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_NAMESPACE: AtomicU64 = AtomicU64::new(1);
#[derive(Debug)]
struct Pending {
    key: ApprovalKey,
    request: PermissionRequest,
    remembered: serde_json::Value,
    grants: Vec<Grant>,
}
#[derive(Debug)]
pub(super) struct Approvals {
    namespace: u64,
    serial: u64,
    pending: VecDeque<Pending>,
}
impl Default for Approvals {
    fn default() -> Self {
        Self {
            namespace: NEXT_NAMESPACE.fetch_add(1, Ordering::Relaxed),
            serial: 0,
            pending: VecDeque::new(),
        }
    }
}
impl Approvals {
    pub(super) fn request(
        &mut self,
        request: &PermissionRequest,
        cwd: Option<&Path>,
    ) -> ApprovalRequest {
        self.serial += 1;
        let key = ApprovalKey {
            namespace: self.namespace,
            serial: self.serial,
        };
        let remembered = protocol::permission_response(request, PermissionAnswer::AllowForSession);
        let grants = protocol::session_rules(request)
            .iter()
            .map(|rule| Grant {
                rule: rule.cli_rule(),
                label: rule.label(),
                created_at: 0,
            })
            .collect();
        self.pending.push_back(Pending {
            key,
            request: request.clone(),
            remembered,
            grants,
        });
        ApprovalRequest {
            key,
            view: view(request, cwd),
        }
    }
    pub(super) fn cancel(&mut self, id: &str) -> Option<ApprovalKey> {
        let position = self
            .pending
            .iter()
            .position(|pending| pending.request.request_id == id)?;
        Some(self.pending.remove(position)?.key)
    }
    pub(super) fn settle_tool(&mut self, id: &str) -> Vec<ApprovalKey> {
        let closed = self
            .pending
            .iter()
            .filter(|pending| pending.request.tool_use_id.as_deref() == Some(id))
            .map(|pending| pending.key)
            .collect();
        self.pending
            .retain(|pending| pending.request.tool_use_id.as_deref() != Some(id));
        closed
    }
    pub(super) fn clear(&mut self) {
        self.pending.clear();
    }
    pub(super) fn answer(
        &mut self,
        key: ApprovalKey,
        intent: ApprovalIntent,
    ) -> Option<ApprovalReply> {
        let position = self.pending.iter().position(|pending| pending.key == key)?;
        let pending = self.pending.remove(position)?;
        let (message, grants) = match intent {
            ApprovalIntent::AllowForSession => (pending.remembered, pending.grants),
            ApprovalIntent::AllowOnce => (
                protocol::permission_response(&pending.request, PermissionAnswer::AllowOnce),
                vec![],
            ),
            ApprovalIntent::Deny => (
                protocol::permission_response(&pending.request, PermissionAnswer::Deny),
                vec![],
            ),
            ApprovalIntent::DenyAndStop => (
                protocol::permission_response(&pending.request, PermissionAnswer::DenyAndStop),
                vec![],
            ),
        };
        Some(ApprovalReply { message, grants })
    }
}

fn view(request: &PermissionRequest, cwd: Option<&Path>) -> ApprovalView {
    let field = |name: &str| request.input.get(name).and_then(serde_json::Value::as_str);
    let is_edit = matches!(
        request.tool_name.as_str(),
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
    );
    let tool = super::events::describe_tool(
        request.tool_use_id.as_ref().unwrap_or(&request.request_id),
        &request.tool_name,
        &request.input,
        cwd,
    );
    let kind = if request.tool_name == "Bash" {
        ApprovalKind::Command {
            command: field("command").map(str::to_string),
            purpose: request
                .description
                .as_deref()
                .or_else(|| field("description"))
                .and_then(|text| text.lines().next())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string),
        }
    } else if is_edit {
        ApprovalKind::FileChange {
            path: (!tool.summary.is_empty()).then(|| tool.summary.clone()),
        }
    } else {
        ApprovalKind::OtherTool
    };
    let question = match &kind {
        ApprovalKind::Command { .. } => "Allow Bash to run this command?".to_string(),
        ApprovalKind::FileChange { path: Some(path) } => format!("Allow Claude to edit {path}?"),
        ApprovalKind::FileChange { path: None } | ApprovalKind::OtherTool => {
            format!("Allow Claude to use {}?", tool.display_name)
        }
    };
    let remember = match protocol::session_scope(request) {
        SessionScope::OnceOnly(protocol::OnceReason::Edit) => {
            RememberScope::OnceOnly(OnceReason::FileChanges)
        }
        SessionScope::OnceOnly(protocol::OnceReason::NoSafeRule) => {
            RememberScope::OnceOnly(OnceReason::NoSafeScope)
        }
        SessionScope::Remembers(labels) => {
            let command = field("command");
            let shown_in_detail = request.tool_name == "Bash"
                && command.is_some_and(|command| labels.len() == 1 && labels[0] == command);
            let meaning = if labels.iter().any(|label| label.contains('*')) {
                if request.tool_name == "Bash" {
                    RuleMeaning::Arguments
                } else {
                    RuleMeaning::Pattern
                }
            } else if request.tool_name.starts_with("mcp__") {
                RuleMeaning::OneTool
            } else {
                RuleMeaning::Exact
            };
            RememberScope::Rules {
                labels,
                meaning,
                shown_in_detail,
            }
        }
    };
    ApprovalView {
        tool,
        kind,
        question,
        affected_path: request.blocked_path.clone(),
        remember,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Incoming;
    use serde_json::json;

    fn request(id: &str, tool: &str, input: serde_json::Value) -> PermissionRequest {
        let Incoming::PermissionRequest(request) = Incoming::from_value(json!({
            "type":"control_request", "request_id":id,
            "request":{"subtype":"can_use_tool", "tool_name":tool, "tool_use_id":"t1", "input":input}
        })) else {
            panic!("approval fixture")
        };
        request
    }

    #[test]
    fn every_recorded_request_keeps_identical_reply_bytes_and_grants_for_all_intents() {
        for entry in
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).unwrap()
        {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
            {
                continue;
            }
            for line in std::fs::read_to_string(path).unwrap().lines() {
                let line: serde_json::Value = serde_json::from_str(line).unwrap();
                if line["dir"] != "out" {
                    continue;
                }
                let Incoming::PermissionRequest(request) =
                    Incoming::from_value(line["msg"].clone())
                else {
                    continue;
                };
                for (intent, wire) in [
                    (ApprovalIntent::AllowOnce, PermissionAnswer::AllowOnce),
                    (
                        ApprovalIntent::AllowForSession,
                        PermissionAnswer::AllowForSession,
                    ),
                    (ApprovalIntent::Deny, PermissionAnswer::Deny),
                    (ApprovalIntent::DenyAndStop, PermissionAnswer::DenyAndStop),
                ] {
                    let mut approvals = Approvals::default();
                    let shown = approvals.request(&request, None);
                    let reply = approvals.answer(shown.key(), intent).unwrap();
                    assert_eq!(
                        serde_json::to_vec(&reply.message).unwrap(),
                        serde_json::to_vec(&protocol::permission_response(&request, wire)).unwrap()
                    );
                    let expected: Vec<_> = if intent == ApprovalIntent::AllowForSession {
                        protocol::session_rules(&request)
                            .iter()
                            .map(|rule| (rule.cli_rule(), rule.label()))
                            .collect()
                    } else {
                        vec![]
                    };
                    assert_eq!(
                        reply
                            .grants
                            .iter()
                            .map(|grant| (grant.rule.clone(), grant.label.clone()))
                            .collect::<Vec<_>>(),
                        expected
                    );
                    assert!(
                        approvals.answer(shown.key(), intent).is_none(),
                        "already consumed"
                    );
                }
            }
        }
    }

    #[test]
    fn keys_cannot_cross_controllers_or_survive_process_end_and_requests_stay_distinct() {
        let request = request("r1", "Bash", json!({"command":"touch a"}));
        let mut first = Approvals::default();
        let mut other = Approvals::default();
        let a = first.request(&request, None);
        let b = other.request(&request, None);
        assert_ne!(a.key(), b.key());
        assert!(
            first
                .answer(b.key(), ApprovalIntent::AllowForSession)
                .is_none()
        );
        let second = first.request(
            &PermissionRequest {
                request_id: "r2".to_string(),
                ..request.clone()
            },
            None,
        );
        assert_ne!(a.key(), second.key(), "two callbacks for one tool");
        first.answer(a.key(), ApprovalIntent::Deny).unwrap();
        assert!(first.answer(a.key(), ApprovalIntent::AllowOnce).is_none());
        assert!(
            first
                .answer(second.key(), ApprovalIntent::AllowOnce)
                .is_some()
        );
        let stale = first.request(&request, None).key();
        first.clear();
        let current = first.request(&request, None).key();
        assert_ne!(stale, current);
        assert!(
            first
                .answer(stale, ApprovalIntent::AllowForSession)
                .is_none()
        );
        assert!(first.answer(current, ApprovalIntent::AllowOnce).is_some());
    }

    #[test]
    fn approval_presentation_preserves_names_paths_purpose_and_once_only_edits() {
        let mut approvals = Approvals::default();
        let bash = approvals.request(
            &request(
                "r1",
                "Bash",
                json!({"command":"touch a", "description":"Create a\nextra"}),
            ),
            None,
        );
        assert_eq!(bash.view().question, "Allow Bash to run this command?");
        assert!(
            matches!(&bash.view().kind, ApprovalKind::Command { purpose: Some(purpose), .. } if purpose == "Create a")
        );
        assert_eq!(
            bash.view().remember,
            RememberScope::Rules {
                labels: vec!["touch a".to_string()],
                meaning: RuleMeaning::Exact,
                shown_in_detail: true
            }
        );
        let edit = approvals.request(
            &request("r2", "Edit", json!({"file_path":"/work/app/src/main.rs"})),
            Some(Path::new("/work/app")),
        );
        assert_eq!(edit.view().question, "Allow Claude to edit src/main.rs?");
        assert!(
            matches!(&edit.view().kind, ApprovalKind::FileChange { path: Some(path) } if path == "src/main.rs")
        );
        assert_eq!(
            edit.view().remember,
            RememberScope::OnceOnly(OnceReason::FileChanges)
        );
        let mcp = approvals.request(
            &request("r3", "mcp__linear__search", json!({"query":"billing"})),
            None,
        );
        assert_eq!(mcp.view().question, "Allow Claude to use linear · search?");
    }
}
