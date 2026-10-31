use super::{
    CodecOutput, CodexCodec,
    wire::{ItemKind, Status},
};
use crate::agent::*;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

impl CodexCodec {
    pub(super) fn item(
        &mut self,
        item: Value,
        completed: bool,
        out: &mut CodecOutput,
    ) -> Result<()> {
        let id = item["id"].as_str().context("Item missing id")?.to_owned();
        if self.finalized.contains(&id) {
            return Ok(());
        }
        let kind: ItemKind =
            serde_json::from_value(item.get("type").context("Item missing type")?.clone())?;
        match kind {
            ItemKind::AgentMessage => {
                let text = item["text"].as_str().context("Message missing text")?;
                if completed || (!text.is_empty() && !self.content.contains_key(&id)) {
                    let key = self.content_key(&id);
                    out.event(AgentEvent::Text {
                        key,
                        update: if completed {
                            TextUpdate::Replace(text.into())
                        } else {
                            TextUpdate::Append(text.into())
                        },
                    });
                }
                if completed {
                    self.finalized.insert(id);
                }
                return Ok(());
            }
            ItemKind::UserMessage | ItemKind::HookPrompt | ItemKind::Reasoning | ItemKind::Plan => {
                return Ok(());
            }
            _ => (),
        }
        let tool = describe(&id, kind, &item);
        self.tools.insert(id.clone(), tool.clone());
        out.event(AgentEvent::ToolUpsert(tool.clone()));
        self.update_approvals(&tool, out);
        if completed {
            let outcome = match kind {
                ItemKind::Command
                | ItemKind::FileChange
                | ItemKind::McpTool
                | ItemKind::DynamicTool => {
                    match serde_json::from_value::<Status>(item["status"].clone())? {
                        Status::Completed if item["success"] == false => ToolOutcome::Failed,
                        Status::Completed => ToolOutcome::Completed,
                        Status::Declined => ToolOutcome::Denied,
                        Status::Failed | Status::Interrupted => ToolOutcome::Failed,
                        Status::InProgress | Status::Unknown => {
                            bail!("Nonterminal tool status in completed item")
                        }
                    }
                }
                ItemKind::WebSearch | ItemKind::ImageView => ToolOutcome::Completed,
                ItemKind::Other => ToolOutcome::Failed,
                ItemKind::AgentMessage
                | ItemKind::UserMessage
                | ItemKind::HookPrompt
                | ItemKind::Reasoning
                | ItemKind::Plan => unreachable!("text handled above"),
            };
            let output = item["aggregatedOutput"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| {
                    item.get("result")
                        .filter(|v| !v.is_null())
                        .map(Value::to_string)
                })
                .or_else(|| self.outputs.remove(&id))
                .unwrap_or_else(|| match kind {
                    ItemKind::FileChange => tool.arguments.to_string(),
                    ItemKind::WebSearch | ItemKind::ImageView | ItemKind::Command => String::new(),
                    _ => "Rich output unavailable".into(),
                });
            out.event(AgentEvent::ToolFinished {
                id: id.clone(),
                outcome,
                output,
            });
            if let Some(duration_ms) = item["durationMs"].as_u64() {
                out.event(AgentEvent::ToolDuration {
                    id: id.clone(),
                    duration_ms,
                });
            }
            let resolved: Vec<_> = self
                .pending
                .iter()
                .filter(|(_, pending)| pending.approval.item_id == id)
                .map(|(key, _)| *key)
                .collect();
            for key in resolved {
                self.pending.remove(&key);
                out.event(AgentEvent::ApprovalResolved(key));
            }
            self.finalized.insert(id);
        }
        Ok(())
    }
    pub(super) fn patch_updated(&mut self, params: Value, out: &mut CodecOutput) -> Result<()> {
        let thread = params["threadId"]
            .as_str()
            .context("Patch missing thread id")?;
        let turn = params["turnId"].as_str().context("Patch missing turn id")?;
        if !self.active(thread, turn) {
            return Ok(());
        }
        let id = params["itemId"].as_str().context("Patch missing item id")?;
        let changes = params.get("changes").context("Patch missing changes")?;
        self.item(
            json!({"type":"fileChange","id":id,"changes":changes,"status":"inProgress"}),
            false,
            out,
        )
    }
}
fn describe(id: &str, kind: ItemKind, item: &Value) -> ToolDescriptor {
    let (name, summary, arguments) = match kind {
        ItemKind::Command => (
            "Command".into(),
            item["command"]
                .as_str()
                .unwrap_or("Command details unavailable")
                .into(),
            json!({"command":item["command"],"cwd":item["cwd"]}),
        ),
        ItemKind::FileChange => {
            let summary = item["changes"]
                .as_array()
                .map(|changes| {
                    changes
                        .iter()
                        .filter_map(|c| c["path"].as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "Change preview unavailable".into());
            ("File change".into(), summary, item["changes"].clone())
        }
        ItemKind::McpTool => (
            format!(
                "{} · {}",
                item["server"].as_str().unwrap_or("MCP"),
                item["tool"].as_str().unwrap_or("Tool")
            ),
            "Tool call".into(),
            item["arguments"].clone(),
        ),
        ItemKind::DynamicTool => (
            "Tool".into(),
            item["tool"].as_str().unwrap_or("Tool call").into(),
            item["arguments"].clone(),
        ),
        ItemKind::WebSearch => (
            "Web search".into(),
            item["query"].as_str().unwrap_or_default().into(),
            json!({"query":item["query"]}),
        ),
        ItemKind::ImageView => (
            "View image".into(),
            item["path"].as_str().unwrap_or_default().into(),
            json!({"path":item["path"]}),
        ),
        ItemKind::Other
        | ItemKind::AgentMessage
        | ItemKind::UserMessage
        | ItemKind::HookPrompt
        | ItemKind::Reasoning
        | ItemKind::Plan => (
            "Agent activity".into(),
            "Details unavailable".into(),
            Value::Null,
        ),
    };
    ToolDescriptor {
        id: id.into(),
        display_name: name.clone(),
        name,
        summary,
        arguments,
    }
}
