use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::agent::{
    AgentBatch, AgentEvent, BackgroundTaskView, Checkpoint, ContentKey, ContextFill, ModelTurn,
    TextUpdate, TokenCounts, ToolDescriptor, ToolOutcome, TurnOutcome,
};
use crate::protocol::{BackgroundTask, ContentBlock, Delta, Incoming, StreamEvent};
use crate::session::SessionEvent;

#[derive(Debug, Default)]
pub struct ClaudeEvents {
    generation: Option<u64>,
    approvals: super::approvals::Approvals,
    next_content: u64,
    cwd: Option<PathBuf>,
    open_text: HashMap<(String, u64), ContentKey>,
    previous_total_cost: Option<f64>,
    api_error_text: Option<String>,
    tasks: Vec<BackgroundTask>,
    task_tools: HashMap<String, String>,
    context_windows: HashMap<String, u64>,
    context: Option<ContextSeen>,
}

/// The latest model call of the main conversation in the running turn.
#[derive(Debug)]
struct ContextSeen {
    model: String,
    fill: ContextFill,
}

impl ClaudeEvents {
    pub(crate) fn translate_generation(
        &mut self,
        generation: u64,
        event: &SessionEvent,
        cwd: Option<&Path>,
    ) -> AgentBatch {
        if self.generation != Some(generation) {
            self.finish();
            self.tasks.clear();
            self.task_tools.clear();
            self.generation = Some(generation);
        }
        self.translate(event, cwd)
    }

    pub fn translate(&mut self, event: &SessionEvent, cwd: Option<&Path>) -> AgentBatch {
        let mut events = Vec::new();
        let cwd = self.cwd.clone().or_else(|| cwd.map(Path::to_path_buf));
        let checkpoint = match event {
            SessionEvent::Batch(batch) => return batch.clone(),
            SessionEvent::Line(incoming) => {
                self.incoming(incoming, cwd.as_deref(), &mut events);
                match incoming {
                    Incoming::System(_)
                    | Incoming::Assistant(_)
                    | Incoming::User(_)
                    | Incoming::PermissionRequest(_)
                    | Incoming::ControlCancelRequest { .. }
                    | Incoming::Result(_)
                    | Incoming::RateLimit(_) => Checkpoint::Save,
                    _ => Checkpoint::Transient,
                }
            }
            SessionEvent::Stderr(line) => {
                events.push(AgentEvent::Stderr(line.clone()));
                Checkpoint::Transient
            }
            SessionEvent::WriteFailed(detail) => {
                self.finish();
                events.push(AgentEvent::WriteFailed(detail.clone()));
                Checkpoint::Save
            }
            SessionEvent::InterruptTimedOut => {
                self.finish();
                events.push(AgentEvent::InterruptTimedOut);
                Checkpoint::Save
            }
            SessionEvent::Exited { code, requested } => {
                self.finish();
                self.tasks.clear();
                self.task_tools.clear();
                events.push(AgentEvent::Exited {
                    code: *code,
                    requested: *requested,
                });
                Checkpoint::Save
            }
        };
        AgentBatch { events, checkpoint }
    }

    pub fn answer(
        &mut self,
        key: crate::agent::ApprovalKey,
        intent: crate::agent::ApprovalIntent,
    ) -> Option<crate::agent::ApprovalReply> {
        self.answer_with_note(key, intent, None)
    }

    /// [`ClaudeEvents::answer`] with the user's note for a denial, sent as its `message`.
    pub fn answer_with_note(
        &mut self,
        key: crate::agent::ApprovalKey,
        intent: crate::agent::ApprovalIntent,
        note: Option<&str>,
    ) -> Option<crate::agent::ApprovalReply> {
        self.approvals.answer(key, intent, note)
    }

    fn content_key(&mut self) -> ContentKey {
        self.next_content += 1;
        ContentKey(self.next_content)
    }

    fn take_open(&mut self, message_id: &str) -> Option<ContentKey> {
        let key = self
            .open_text
            .iter()
            .filter(|((id, _), _)| id == message_id)
            .max_by_key(|(_, key)| key.0)
            .map(|(key, _)| key.clone())?;
        self.open_text.remove(&key)
    }

    fn finish(&mut self) {
        self.approvals.clear();
        self.open_text.clear();
        self.context = None;
    }

    fn background(&self) -> AgentEvent {
        AgentEvent::BackgroundTasks(
            self.tasks
                .iter()
                .map(|task| BackgroundTaskView {
                    task_id: task.task_id.clone(),
                    description: task.description.clone(),
                    tool: self.task_tools.get(&task.task_id).cloned(),
                })
                .collect(),
        )
    }

    fn incoming(&mut self, incoming: &Incoming, cwd: Option<&Path>, events: &mut Vec<AgentEvent>) {
        if starts_turn(incoming) {
            events.push(AgentEvent::TurnActive);
        }
        match incoming {
            Incoming::System(message) => {
                if let Some(tasks) = message.background_tasks() {
                    self.tasks = tasks.to_vec();
                    events.push(self.background());
                    return;
                }
                if message.subtype == "task_started"
                    && let (Some(task), Some(tool)) = (&message.task_id, &message.tool_use_id)
                {
                    self.task_tools.insert(task.clone(), tool.clone());
                    events.push(self.background());
                    return;
                }
                if message.subtype != "init" {
                    return;
                }
                events.push(AgentEvent::UserConfirmed);
                if let Some(id) = &message.session_id {
                    events.push(AgentEvent::ConversationEstablished(id.clone()));
                }
                if let Some(model) = &message.model {
                    events.push(AgentEvent::ModelReported(model.clone()));
                }
                if let Some(cwd) = &message.cwd {
                    self.cwd = Some(PathBuf::from(cwd));
                    events.push(AgentEvent::WorkingDirectoryReported(PathBuf::from(cwd)));
                }
            }
            Incoming::StreamEvent(message) => {
                if message.parent_tool_use_id.is_some() {
                    return;
                }
                if let StreamEvent::ContentBlockDelta {
                    index,
                    delta: Delta::Text(text),
                } = &message.event
                    && let Some(id) = &message.api_message_id
                {
                    let wire = (id.clone(), *index);
                    let key = match self.open_text.get(&wire) {
                        Some(key) => *key,
                        None => {
                            let key = self.content_key();
                            self.open_text.insert(wire, key);
                            key
                        }
                    };
                    events.push(AgentEvent::Text {
                        key,
                        update: TextUpdate::Append(text.clone()),
                    });
                }
            }
            Incoming::Assistant(message) => {
                if message.parent_tool_use_id.is_some() {
                    return;
                }
                if message.is_api_error_message {
                    self.api_error_text = Some(
                        message
                            .message
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    return;
                }
                if let (Some(usage), Some(model)) = (&message.message.usage, &message.message.model)
                    && usage.context_tokens() > 0
                {
                    let fill = ContextFill {
                        used: usage.context_tokens(),
                        window: self.context_windows.get(model).copied(),
                    };
                    self.context = Some(ContextSeen {
                        model: model.clone(),
                        fill,
                    });
                    events.push(AgentEvent::ContextUsed {
                        model: model.clone(),
                        fill,
                    });
                }
                for block in &message.message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let key = self
                                .take_open(&message.message.id)
                                .unwrap_or_else(|| self.content_key());
                            events.push(AgentEvent::Text {
                                key,
                                update: TextUpdate::Replace(text.clone()),
                            });
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            events.push(AgentEvent::ToolUpsert(describe_tool(id, name, input, cwd)))
                        }
                        _ => {}
                    }
                }
                if message.aborted {
                    while self.take_open(&message.message.id).is_some() {}
                    events.push(AgentEvent::ReplyInterrupted);
                }
            }
            Incoming::User(message) => {
                for block in message.message.content.blocks() {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } = block
                    {
                        events.extend(
                            self.approvals
                                .settle_tool(tool_use_id)
                                .into_iter()
                                .map(AgentEvent::ApprovalResolved),
                        );
                        let rejected = message
                            .tool_result_meta
                            .iter()
                            .any(|meta| meta.id == *tool_use_id && meta.was_rejected());
                        events.push(AgentEvent::ToolFinished {
                            id: tool_use_id.clone(),
                            outcome: if rejected {
                                ToolOutcome::Denied
                            } else if *is_error {
                                ToolOutcome::Failed
                            } else {
                                ToolOutcome::Completed
                            },
                            output: ContentBlock::tool_result_text(content),
                        });
                    }
                }
            }
            Incoming::PermissionRequest(request) => {
                let id = request.tool_use_id.as_ref().unwrap_or(&request.request_id);
                events.push(AgentEvent::ToolUpsert(describe_tool(
                    id,
                    &request.tool_name,
                    &request.input,
                    cwd,
                )));
                events.push(AgentEvent::ApprovalRequested(
                    self.approvals.request(request, cwd),
                ));
            }
            Incoming::ControlCancelRequest { request_id } => {
                if let Some(key) = self.approvals.cancel(request_id) {
                    events.push(AgentEvent::ApprovalWithdrawn {
                        key,
                        note: "Cancelled by Claude".to_string(),
                    });
                }
            }
            Incoming::Result(result) => {
                let turns = model_turns(result);
                for turn in &turns {
                    if let Some(window) = turn.context_window {
                        self.context_windows.insert(turn.model.clone(), window);
                    }
                }
                if let Some(seen) = &self.context {
                    let window = self.context_windows.get(&seen.model).copied();
                    if window != seen.fill.window {
                        events.push(AgentEvent::ContextUsed {
                            model: seen.model.clone(),
                            fill: ContextFill {
                                used: seen.fill.used,
                                window,
                            },
                        });
                    }
                }
                if !turns.is_empty() {
                    events.push(AgentEvent::TurnUsage(turns));
                }
                let cost_usd = result.turn_cost_usd().or_else(|| {
                    Some(result.total_cost_usd? - self.previous_total_cost.unwrap_or(0.0))
                });
                if result.total_cost_usd.is_some() {
                    self.previous_total_cost = result.total_cost_usd;
                }
                let api_error_text = self.api_error_text.take();
                let outcome = if result.was_stopped() {
                    TurnOutcome::Stopped
                } else if result.is_error {
                    let detail = api_error_text
                        .filter(|text| !text.is_empty())
                        .or_else(|| result.result.clone().filter(|text| !text.is_empty()))
                        .unwrap_or_else(|| {
                            let lines = result.error_lines();
                            if lines.is_empty() {
                                "Claude reported an error but gave no details.".to_string()
                            } else {
                                lines.join("\n")
                            }
                        });
                    TurnOutcome::Failed {
                        title: "Claude stopped".to_string(),
                        detail,
                    }
                } else {
                    TurnOutcome::Completed
                };
                events.push(AgentEvent::TurnFinished {
                    outcome,
                    duration_ms: result.duration_ms,
                    cost_usd,
                    forget_conversation: !result.was_stopped()
                        && result.is_error
                        && result.is_missing_conversation(),
                    queued: !result.was_stopped() && result.another_turn_follows(),
                });
                self.finish();
            }
            Incoming::RateLimit(value) => {
                let Some(info) = value.get("rate_limit_info") else {
                    return;
                };
                let Some(status) = info.get("status").and_then(Value::as_str) else {
                    return;
                };
                let mut windows = Vec::new();
                if let Some(unified) = info.get("unifiedWindows").and_then(Value::as_object) {
                    for (label, window) in unified {
                        windows.push(crate::agent::LimitWindow {
                            label: label.clone(),
                            used_percent: window["utilization"]
                                .as_f64()
                                .filter(|n| n.is_finite())
                                .map(|n| n * 100.0),
                            resets_at_unix_seconds: window["resetsAt"].as_i64().filter(|n| *n >= 0),
                        });
                    }
                }
                if windows.is_empty() {
                    windows.push(crate::agent::LimitWindow {
                        label: info["rateLimitType"].as_str().unwrap_or("usage").into(),
                        used_percent: None,
                        resets_at_unix_seconds: info["resetsAt"].as_i64().filter(|n| *n >= 0),
                    });
                }
                let snapshot = crate::agent::LimitSnapshot {
                    bucket: "claude".into(),
                    windows,
                    explanation: "Claude reported a usage limit. Nothing resumes automatically."
                        .into(),
                };
                let limit = match status {
                    "rejected" => crate::agent::RateLimitEvent::Rejected(snapshot),
                    "allowed_warning" | "warning" => {
                        crate::agent::RateLimitEvent::Warning(snapshot)
                    }
                    _ => crate::agent::RateLimitEvent::Updated(snapshot),
                };
                events.push(AgentEvent::RateLimit(limit));
                if let Some(usage) = crate::usage::ProviderUsage::from_claude_rate_limit(info) {
                    events.push(AgentEvent::ProviderUsage(usage));
                }
                if status == "rejected" {
                    self.finish();
                    events.push(AgentEvent::TurnFinished {
                        outcome: TurnOutcome::Failed {
                            title: "Usage limit reached".into(),
                            detail: "Claude rejected the turn. Continue only after a user action."
                                .into(),
                        },
                        duration_ms: None,
                        cost_usd: None,
                        forget_conversation: false,
                        queued: false,
                    });
                }
            }
            Incoming::ControlRequest(_) | Incoming::ControlResponse(_) | Incoming::Unknown(_) => {}
        }
    }
}

/// One entry per model the turn used, sorted by name so the order never depends on the map.
fn model_turns(result: &crate::protocol::ResultMessage) -> Vec<ModelTurn> {
    let mut turns: Vec<ModelTurn> = result
        .model_usage_since_last_result
        .iter()
        .flatten()
        .map(|(model, usage)| ModelTurn {
            model: model.clone(),
            tokens: TokenCounts {
                input: usage.input_tokens,
                cached_input: usage.cache_read_input_tokens,
                cache_write_input: usage.cache_creation_input_tokens,
                output: usage.output_tokens,
                reasoning_output: usage.thinking_tokens,
            },
            cost_usd: usage.cost_usd,
            context_window: usage.context_window,
        })
        .collect();
    turns.sort_by(|a, b| a.model.cmp(&b.model));
    turns
}

pub fn describe_tool(id: &str, name: &str, input: &Value, cwd: Option<&Path>) -> ToolDescriptor {
    ToolDescriptor {
        id: id.to_string(),
        name: name.to_string(),
        display_name: super::display_name(name),
        summary: super::summarize(name, input, cwd),
        arguments: input.clone(),
    }
}
/// Lines that only come while a turn runs: its `init`, and the main conversation's replies.
/// One between turns means claude started a turn by itself, as it does when a background task
/// finishes: no user message, just `init` and a reply. It is a turn like any other.
///
/// Assumes claude sends `init` only to start a turn, never at spawn or after a result (true in
/// every fixture; checked in `tests/replay.rs`).
fn starts_turn(incoming: &Incoming) -> bool {
    match incoming {
        Incoming::System(message) => message.subtype == "init",
        Incoming::Assistant(message) => message.parent_tool_use_id.is_none(),
        Incoming::StreamEvent(message) => message.parent_tool_use_id.is_none(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_process_drops_transient_maps_and_preserves_cumulative_cost_fallback() {
        let mut codec = ClaudeEvents::default();
        let line = |value| SessionEvent::Line(Incoming::from_value(value));
        codec.translate_generation(1, &line(json!({"type":"system", "subtype":"background_tasks_changed", "tasks":[{"task_id":"b1", "description":"old task"}]})), None);
        codec.translate_generation(1, &line(json!({"type":"system", "subtype":"task_started", "task_id":"b1", "tool_use_id":"t1"})), None);
        assert_eq!(codec.tasks.len(), 1);
        assert_eq!(codec.task_tools.get("b1").map(String::as_str), Some("t1"));
        codec.translate_generation(
            1,
            &line(json!({"type":"result", "is_error":false, "total_cost_usd":1.0})),
            None,
        );
        let ask = line(
            json!({"type":"control_request", "request_id":"r1", "request":{"subtype":"can_use_tool", "tool_name":"Bash", "input":{"command":"touch a"}}}),
        );
        let batch = codec.translate_generation(1, &ask, None);
        let key = batch
            .events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ApprovalRequested(request) => Some(request.key()),
                _ => None,
            })
            .unwrap();
        codec.translate_generation(2, &line(json!({"type":"system", "subtype":"task_started", "task_id":"b2", "tool_use_id":"t2"})), None);
        assert!(codec.tasks.is_empty());
        assert!(!codec.task_tools.contains_key("b1"));
        assert!(
            codec
                .answer(key, crate::agent::ApprovalIntent::AllowForSession)
                .is_none()
        );
        let batch = codec.translate_generation(
            2,
            &line(json!({"type":"result", "is_error":false, "total_cost_usd":1.25})),
            None,
        );
        assert!(batch.events.iter().any(|event| matches!(event, AgentEvent::TurnFinished { cost_usd: Some(cost), .. } if *cost == 0.25)));
    }

    #[test]
    fn subagent_lines_and_empty_usage_leave_the_main_context_alone_and_models_come_out_sorted() {
        let mut codec = ClaudeEvents::default();
        let line = |value| SessionEvent::Line(Incoming::from_value(value));
        let usage = |input| json!({"input_tokens":input,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1});
        let contexts = |batch: &AgentBatch| {
            batch
                .events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::ContextUsed { model, fill } => Some((model.clone(), fill.used)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let main = codec.translate(
            &line(json!({"type":"assistant","message":{"id":"m1","model":"claude-opus-5-5","content":[],"usage":usage(100)}})),
            None,
        );
        assert_eq!(contexts(&main), [("claude-opus-5-5".to_string(), 101)]);
        let sub = codec.translate(
            &line(json!({"type":"assistant","parent_tool_use_id":"t1","message":{"id":"m2","model":"claude-haiku-5-5","content":[],"usage":usage(7)}})),
            None,
        );
        assert!(contexts(&sub).is_empty());
        let synthetic = codec.translate(
            &line(json!({"type":"assistant","message":{"id":"m3","model":"<synthetic>","content":[],"usage":{"input_tokens":0,"output_tokens":0}}})),
            None,
        );
        assert!(contexts(&synthetic).is_empty());
        let done = codec.translate(
            &line(json!({"type":"result","is_error":false,"model_usage_since_last_result":{
                "claude-opus-5-5":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4,"thinkingTokens":1,"costUSD":0.5,"contextWindow":200000},
                "claude-haiku-5-5":{"inputTokens":5,"outputTokens":6,"costUSD":0.25,"contextWindow":1000000}}})),
            None,
        );
        assert_eq!(contexts(&done), [("claude-opus-5-5".to_string(), 101)]);
        let turns = done
            .events
            .iter()
            .find_map(|event| match event {
                AgentEvent::TurnUsage(turns) => Some(turns),
                _ => None,
            })
            .unwrap();
        let names: Vec<_> = turns.iter().map(|turn| turn.model.as_str()).collect();
        assert_eq!(names, ["claude-haiku-5-5", "claude-opus-5-5"]);
        assert_eq!(turns[1].tokens.reasoning_output, 1);
        assert!(done.events.iter().any(|event| matches!(event, AgentEvent::TurnFinished { cost_usd: Some(cost), .. } if *cost == 0.75)));
        let next_turn = codec.translate(
            &line(json!({"type":"assistant","message":{"id":"m4","model":"claude-opus-5-5","content":[],"usage":usage(300)}})),
            None,
        );
        assert_eq!(contexts(&next_turn), [("claude-opus-5-5".to_string(), 301)]);
        let window = next_turn.events.iter().find_map(|event| match event {
            AgentEvent::ContextUsed { fill, .. } => Some(fill.window),
            _ => None,
        });
        assert_eq!(
            window,
            Some(Some(200000)),
            "windows outlive the turn that reported them"
        );
    }
}
