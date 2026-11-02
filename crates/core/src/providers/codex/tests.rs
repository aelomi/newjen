use super::*;
use crate::agent::*;
use crate::providers::{ModelSelection, Provider};
use crate::transcript::{Item, Status, ToolState, Transcript};
use serde_json::{Value, json};

fn config() -> AgentConfig {
    AgentConfig {
        provider: Provider::Codex,
        model: ModelSelection::CliDefault,
        effort: EffortSelection::CliDefault,
        speed: Speed::Normal,
    }
}
fn new() -> (CodexCodec, CodecOutput) {
    CodexCodec::new(config(), "/tmp/newjen-codex-scratch".into(), None).unwrap()
}
fn feed(codec: &mut CodexCodec, value: Value) -> CodecOutput {
    codec.stdout(&value.to_string())
}
fn respond(codec: &mut CodexCodec, out: &CodecOutput, value: Value) -> CodecOutput {
    let message = out
        .messages
        .last()
        .unwrap_or_else(|| panic!("No reply to: {out:?}"));
    feed(codec, json!({"id":message["id"],"result":value}))
}
fn model_page() -> Value {
    json!({"data":[{"model":"test-model","displayName":"Test","hidden":false,"supportedReasoningEfforts":[{"reasoningEffort":"low"}],"defaultReasoningEffort":"low","serviceTiers":[{"id":"priority","name":"Fast","description":"Fast"}],"defaultServiceTier":"default"}],"nextCursor":null})
}
fn open_result() -> Value {
    json!({"thread":{"id":"thread"},"model":"test-model","cwd":"/tmp/newjen-codex-scratch","approvalPolicy":"untrusted","approvalsReviewer":"user","sandbox":{"type":"readOnly","networkAccess":false},"serviceTier":"default"})
}
fn through_models(mut codec: CodexCodec, init: CodecOutput) -> (CodexCodec, CodecOutput) {
    let account = respond(&mut codec, &init, json!({"userAgent":"codex-cli/0.166.0"}));
    assert_eq!(account.messages[0], json!({"method":"initialized"}));
    let models = respond(
        &mut codec,
        &account,
        json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true}),
    );
    let limits = respond(&mut codec, &models, model_page());
    (codec, limits)
}
fn ready() -> CodexCodec {
    let (codec, init) = new();
    let (mut codec, limits) = through_models(codec, init);
    let open = respond(
        &mut codec,
        &limits,
        json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":10,"resetsAt":1800000000}}}),
    );
    assert_eq!(open.messages[0]["method"], "thread/start");
    let established = respond(&mut codec, &open, open_result());
    assert!(matches!(
        established.batch.events[0],
        AgentEvent::ConversationEstablished(_)
    ));
    assert!(matches!(codec.runtime(), CodexRuntime::Ready { .. }));
    codec
}
fn start(codec: &mut CodexCodec, transcript: &mut Transcript) {
    transcript.push_user("go");
    let out = codec.prompt("go").unwrap();
    let active = respond(
        codec,
        &out,
        json!({"turn":{"id":"turn","items":[],"status":"inProgress"}}),
    );
    transcript.apply(&active.batch);
}
fn notification(
    codec: &mut CodexCodec,
    transcript: &mut Transcript,
    method: &str,
    params: Value,
) -> CodecOutput {
    let out = feed(codec, json!({"method":method,"params":params}));
    transcript.apply(&out.batch);
    out
}
fn command(id: &str, status: &str) -> Value {
    json!({"type":"commandExecution","id":id,"command":"touch a","cwd":"/work","status":status,"aggregatedOutput":null})
}
fn item(codec: &mut CodexCodec, transcript: &mut Transcript, value: Value, completed: bool) {
    notification(
        codec,
        transcript,
        if completed {
            "item/completed"
        } else {
            "item/started"
        },
        json!({"threadId":"thread","turnId":"turn","item":value}),
    );
}
fn ask(
    codec: &mut CodexCodec,
    transcript: &mut Transcript,
    id: Value,
    command: bool,
    extra: Value,
) -> ApprovalKey {
    let mut params = json!({"threadId":"thread","turnId":"turn","itemId":"tool","command":"touch a","cwd":"/work"});
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let out = feed(
        codec,
        json!({"id":id,"method":if command {"item/commandExecution/requestApproval"} else {"item/fileChange/requestApproval"},"params":params}),
    );
    assert!(out.messages.is_empty(), "no automatic approval");
    transcript.apply(&out.batch);
    out.batch
        .events
        .iter()
        .find_map(|e| {
            if let AgentEvent::ApprovalRequested(r) = e {
                Some(r.key())
            } else {
                None
            }
        })
        .unwrap()
}
fn finished_count(transcript: &Transcript) -> usize {
    transcript
        .items()
        .iter()
        .filter(|i| matches!(i, Item::TurnEnd { .. } | Item::Error { .. }))
        .count()
}

#[test]
fn handshake_order_and_user_only_admission() {
    let (codec, init) = new();
    assert_eq!(
        init.messages[0],
        json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"newjen","title":"Newjen","version":env!("CARGO_PKG_VERSION")}}})
    );
    let (mut codec, limits) = through_models(codec, init);
    assert!(codec.prompt("early").is_err());
    let open = respond(&mut codec, &limits, json!({"rateLimits":{}}));
    assert_eq!(open.messages[0]["params"]["serviceTier"], "default");
    assert_eq!(open.messages[0]["params"]["sandbox"], "read-only");
    assert!(open.messages[0]["params"].get("model").is_none());
    assert!(open.messages[0]["params"].get("effort").is_none());
    respond(&mut codec, &open, open_result());
    for method in [
        "thread/started",
        "unknown/notification",
        "thread/status/changed",
    ] {
        assert!(
            feed(&mut codec, json!({"method":method,"params":{}}))
                .messages
                .is_empty()
        );
    }
    let prompt = codec.prompt("hi\n\"quoted\"").unwrap();
    assert_eq!(prompt.messages[0]["method"], "turn/start");
    assert_eq!(
        prompt.messages[0]["params"]["serviceTierForTurn"],
        "default"
    );
    assert!(codec.prompt("parallel").is_err());
}

#[test]
fn config_validates_model_effort_and_advertised_fast_tier() {
    for (speed, tier) in [(Speed::Normal, "default"), (Speed::Fast, "priority")] {
        let c = AgentConfig {
            model: ModelSelection::Model {
                id: "test-model".into(),
            },
            effort: EffortSelection::Chosen("low".into()),
            speed,
            ..config()
        };
        let (codec, init) = CodexCodec::new(c, "/tmp/newjen-codex-scratch".into(), None).unwrap();
        let (mut codec, limits) = through_models(codec, init);
        let open = respond(&mut codec, &limits, json!({"rateLimits":{}}));
        assert_eq!(open.messages[0]["params"]["serviceTier"], tier);
        assert!(open.messages[0]["params"].get("effort").is_none());
        let mut opened = open_result();
        opened["serviceTier"] = json!(tier);
        respond(&mut codec, &open, opened);
        let turn = codec.prompt("go").unwrap();
        assert_eq!(turn.messages[0]["params"]["effort"], "low");
        assert_eq!(turn.messages[0]["params"]["serviceTierForTurn"], tier);
    }
    for c in [
        AgentConfig {
            speed: Speed::Fast,
            ..config()
        },
        AgentConfig {
            model: ModelSelection::Model {
                id: "missing".into(),
            },
            ..config()
        },
        AgentConfig {
            model: ModelSelection::Model {
                id: "test-model".into(),
            },
            effort: EffortSelection::Chosen("max".into()),
            ..config()
        },
    ] {
        let (codec, init) = CodexCodec::new(c, "/tmp/newjen-codex-scratch".into(), None).unwrap();
        let (codec, out) = through_models(codec, init);
        assert_eq!(codec.runtime(), &CodexRuntime::Blocked);
        assert!(out.messages.is_empty());
    }
}

#[test]
fn missing_auth_policy_mismatch_and_resume_failure_fail_closed() {
    let (mut codec, init) = new();
    let account = respond(&mut codec, &init, json!({"userAgent":"codex-cli/0.166.0"}));
    let out = respond(
        &mut codec,
        &account,
        json!({"account":null,"requiresOpenaiAuth":true}),
    );
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    for (key, value) in [
        ("sandbox", json!({"type":"workspaceWrite"})),
        ("approvalPolicy", json!("never")),
        ("approvalsReviewer", json!("auto_review")),
        ("cwd", json!("/elsewhere")),
    ] {
        let (codec, init) = new();
        let (mut codec, limits) = through_models(codec, init);
        let open = respond(&mut codec, &limits, json!({"rateLimits":{}}));
        let mut result = open_result();
        result[key] = value;
        let out = respond(&mut codec, &open, result);
        assert_eq!(codec.runtime(), &CodexRuntime::Blocked);
        assert!(out.effects.contains(&ControlEffect::Shutdown));
    }
    let (codec, init) = CodexCodec::new(
        config(),
        "/tmp/newjen-codex-scratch".into(),
        Some("saved".into()),
    )
    .unwrap();
    let (mut codec, limits) = through_models(codec, init);
    let open = respond(&mut codec, &limits, json!({"rateLimits":{}}));
    assert_eq!(open.messages[0]["method"], "thread/resume");
    let out = feed(
        &mut codec,
        json!({"id":open.messages[0]["id"],"error":{"code":-1,"message":"Not found"}}),
    );
    assert_eq!(codec.runtime(), &CodexRuntime::Blocked);
    assert!(out.messages.is_empty());
}

#[test]
fn approvals_preserve_rpc_id_type_and_exact_decision_bytes() {
    for id in [json!(7), json!("7")] {
        for (intent, decision) in [
            (ApprovalIntent::AllowOnce, "accept"),
            (ApprovalIntent::AllowForSession, "acceptForSession"),
            (ApprovalIntent::Deny, "decline"),
            (ApprovalIntent::DenyAndStop, "cancel"),
        ] {
            let mut codec = ready();
            let mut transcript = Transcript::new();
            start(&mut codec, &mut transcript);
            let key = ask(&mut codec, &mut transcript, id.clone(), true, json!({}));
            assert_eq!(transcript.status(), Status::Waiting);
            let out = codec.answer(key, intent).unwrap();
            assert_eq!(
                serde_json::to_vec(&out.messages[0]).unwrap(),
                serde_json::to_vec(&json!({"id":id,"result":{"decision":decision}})).unwrap()
            );
            transcript.apply(&out.batch);
            assert!(transcript.pending_approval().is_none());
            assert!(codec.answer(key, intent).is_none());
            if intent == ApprovalIntent::DenyAndStop {
                assert_eq!(out.messages[1]["method"], "turn/interrupt");
                assert_eq!(
                    out.messages[1]["params"],
                    json!({"threadId":"thread","turnId":"turn"})
                );
                assert!(out.effects.contains(&ControlEffect::ArmStopDeadline));
            }
        }
    }
}

#[test]
fn file_and_incomplete_or_network_commands_never_remember() {
    for (is_command, extra) in [
        (false, json!({"grantRoot":"/"})),
        (true, json!({"cwd":null})),
        (true, json!({"command":null})),
        (true, json!({"cwd":"relative"})),
        (true, json!({"command":""})),
        (true, json!({"kind":"writeStdin"})),
        (true, json!({"kind":"future"})),
        (
            true,
            json!({"networkApprovalContext":{"host":"example.com"}}),
        ),
        (true, json!({"environmentId":"remote"})),
    ] {
        let mut codec = ready();
        let mut t = Transcript::new();
        start(&mut codec, &mut t);
        let key = ask(&mut codec, &mut t, json!(1), is_command, extra);
        assert!(matches!(
            t.pending_approval().unwrap().view().remember,
            RememberScope::OnceOnly(_)
        ));
        let out = codec.answer(key, ApprovalIntent::AllowForSession).unwrap();
        assert_eq!(
            out.messages[0],
            json!({"id":1,"result":{"decision":"accept"}})
        );
        assert!(codec.grants().is_empty());
    }
}

#[test]
fn callbacks_are_independent_and_resolved_before_click_is_harmless() {
    let mut codec = ready();
    let mut t = Transcript::new();
    start(&mut codec, &mut t);
    let a = ask(
        &mut codec,
        &mut t,
        json!(1),
        true,
        json!({"approvalId":"callback-a"}),
    );
    let b = ask(
        &mut codec,
        &mut t,
        json!("1"),
        true,
        json!({"approvalId":"callback-b"}),
    );
    assert_ne!(a, b);
    notification(
        &mut codec,
        &mut t,
        "serverRequest/resolved",
        json!({"threadId":"thread","requestId":1}),
    );
    assert!(codec.answer(a, ApprovalIntent::AllowOnce).is_none());
    assert_eq!(t.pending_approval().unwrap().key(), b);
    let out = codec.answer(b, ApprovalIntent::Deny).unwrap();
    t.apply(&out.batch);
    item(&mut codec, &mut t, command("tool", "failed"), true);
    assert!(matches!(
        t.items()[1],
        Item::Tool {
            state: ToolState::Denied,
            ..
        }
    ));
    assert!(codec.answer(b, ApprovalIntent::AllowOnce).is_none());
    let key = ask(
        &mut codec,
        &mut t,
        json!(2),
        true,
        json!({"itemId":"tool2"}),
    );
    notification(
        &mut codec,
        &mut t,
        "serverRequest/resolved",
        json!({"threadId":"thread","requestId":2}),
    );
    assert!(codec.answer(key, ApprovalIntent::AllowOnce).is_none());
    assert!(matches!(
        t.items()[2],
        Item::Tool {
            state: ToolState::Running,
            ..
        }
    ));
}

#[test]
fn denied_precedence_survives_another_callback_allow_and_completion() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let a = ask(&mut c, &mut t, json!(1), true, json!({}));
    let b = ask(&mut c, &mut t, json!(2), true, json!({}));
    t.apply(&c.answer(a, ApprovalIntent::Deny).unwrap().batch);
    t.apply(&c.answer(b, ApprovalIntent::AllowOnce).unwrap().batch);
    item(&mut c, &mut t, command("tool", "completed"), true);
    assert!(matches!(
        t.items()[1],
        Item::Tool {
            state: ToolState::Denied,
            ..
        }
    ));
}

#[test]
fn text_output_snapshots_and_terminal_items_do_not_duplicate() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    item(
        &mut c,
        &mut t,
        json!({"type":"agentMessage","id":"m","text":"Hel"}),
        false,
    );
    item(
        &mut c,
        &mut t,
        json!({"type":"agentMessage","id":"m","text":"Hel"}),
        false,
    );
    notification(
        &mut c,
        &mut t,
        "item/agentMessage/delta",
        json!({"threadId":"thread","turnId":"turn","itemId":"m","delta":"lo"}),
    );
    assert!(matches!(&t.items()[1], Item::Assistant { text, .. } if text == "Hello"));
    for _ in 0..2 {
        item(
            &mut c,
            &mut t,
            json!({"type":"agentMessage","id":"m","text":"Hello!"}),
            true,
        );
    }
    assert_eq!(t.items().len(), 2);
    assert!(matches!(&t.items()[1],Item::Assistant{text,..} if text=="Hello!"));
    item(&mut c, &mut t, command("tool", "inProgress"), false);
    notification(
        &mut c,
        &mut t,
        "item/commandExecution/outputDelta",
        json!({"threadId":"thread","turnId":"turn","itemId":"tool","delta":"partial"}),
    );
    let mut final_item = command("tool", "completed");
    final_item["aggregatedOutput"] = json!("final");
    final_item["durationMs"] = json!(25);
    item(&mut c, &mut t, final_item, true);
    assert!(
        matches!(&t.items()[2],Item::Tool{output:Some(output),took:Some(took),..} if output=="final" && took.as_millis()==25)
    );
    notification(
        &mut c,
        &mut t,
        "turn/completed",
        json!({"threadId":"thread","turn":{"id":"turn","status":"completed","items":[]}}),
    );
    assert_eq!(t.status(), Status::Done);
    assert!(matches!(
        t.items().last(),
        Some(Item::TurnEnd { cost_usd: None, .. })
    ));
}

#[test]
fn file_preview_updates_same_approval_without_granting_root() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let key = ask(&mut c, &mut t, json!(1), false, json!({"grantRoot":"/"}));
    assert!(matches!(
        t.pending_approval().unwrap().view().kind,
        ApprovalKind::FileChange { path: None }
    ));
    let changes =
        json!([{"path":"/work/a","kind":{"type":"update","move_path":null},"diff":"-a\n+b"}]);
    notification(
        &mut c,
        &mut t,
        "item/fileChange/patchUpdated",
        json!({"threadId":"thread","turnId":"turn","itemId":"tool","changes":changes}),
    );
    let r = t.pending_approval().unwrap();
    assert_eq!(r.key(), key);
    assert_eq!(r.view().tool.arguments, changes);
    assert!(matches!(&r.view().kind,ApprovalKind::FileChange{path:Some(p)} if p=="/work/a"));
    assert!(matches!(
        r.view().remember,
        RememberScope::OnceOnly(OnceReason::FileChanges)
    ));
}

#[test]
fn unsupported_server_requests_always_receive_bounded_errors() {
    for method in [
        "item/tool/requestUserInput",
        "mcpServer/elicitation/request",
        "item/permissions/requestApproval",
        "account/chatgptAuthTokens/refresh",
        "attestation/generate",
        "unknown/future",
    ] {
        let mut c = ready();
        let out = feed(&mut c, json!({"id":"server","method":method,"params":{}}));
        assert_eq!(
            out.messages,
            vec![
                json!({"id":"server","error":{"code":-32601,"message":"Newjen does not support this server request"}})
            ]
        );
        assert!(out.effects.contains(&ControlEffect::Shutdown));
        assert!(c.prompt("retry").is_err());
    }
}

#[test]
fn stop_in_every_state_clears_authority_and_late_replies_cannot_restart() {
    let (c, _) = new();
    let ready = ready();
    let states = [
        CodexRuntime::Initializing,
        CodexRuntime::Opening,
        CodexRuntime::Ready {
            thread_id: "thread".into(),
        },
        CodexRuntime::StartingTurn {
            thread_id: "thread".into(),
        },
        CodexRuntime::Running {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
        },
        CodexRuntime::Stopping {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
        },
        CodexRuntime::Blocked,
        CodexRuntime::Closed,
    ];
    drop(c);
    drop(ready);
    for state in states {
        let (mut c, _) = new();
        c.state = state.clone();
        let out = c.interrupt();
        match state {
            CodexRuntime::Running { .. } => {
                assert_eq!(out.messages[0]["method"], "turn/interrupt");
                assert!(out.effects.contains(&ControlEffect::ArmStopDeadline));
                let ack = respond(&mut c, &out, json!({}));
                assert_eq!(ack.effects, [ControlEffect::StopAcknowledged]);
                assert!(matches!(c.runtime(), CodexRuntime::Stopping { .. }));
            }
            CodexRuntime::Stopping { .. } | CodexRuntime::Closed => {
                assert!(out.messages.is_empty())
            }
            CodexRuntime::Blocked => {
                assert_eq!(c.runtime(), &CodexRuntime::Blocked);
                assert!(out.batch.events.is_empty());
                assert!(out.effects.contains(&ControlEffect::Shutdown));
            }
            _ => {
                assert_eq!(c.runtime(), &CodexRuntime::Closed);
                assert!(out.effects.contains(&ControlEffect::Shutdown));
                assert!(out.messages.is_empty());
            }
        }
        assert!(c.prompt("late").is_err());
        if matches!(c.runtime(), CodexRuntime::Closed) {
            assert!(
                feed(
                    &mut c,
                    json!({"id":1,"result":{"userAgent":"codex-cli/0.166.0"}})
                )
                .messages
                .is_empty()
            );
        }
    }
}

#[test]
fn limits_merge_sparse_values_keep_reset_units_and_never_auto_resume() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    notification(
        &mut c,
        &mut t,
        "account/rateLimits/updated",
        json!({"rateLimits":{"limitId":"codex","primary":null,"secondary":{"usedPercent":94,"resetsAt":1800001000},"spendControlReached":null}}),
    );
    let Some(RateLimitEvent::Warning(s)) = t.rate_limit() else {
        panic!("warning")
    };
    assert_eq!(s.windows.len(), 2);
    assert_eq!(s.windows[0].resets_at_unix_seconds, Some(1800000000));
    let out = notification(
        &mut c,
        &mut t,
        "account/rateLimits/updated",
        json!({"rateLimits":{"limitId":"codex","spendControlReached":true}}),
    );
    assert!(matches!(t.rate_limit(), Some(RateLimitEvent::Rejected(_))));
    assert_eq!(t.status(), Status::Failed);
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    notification(
        &mut c,
        &mut t,
        "turn/completed",
        json!({"threadId":"thread","turn":{"id":"turn","status":"completed"}}),
    );
    notification(
        &mut c,
        &mut t,
        "account/rateLimits/updated",
        json!({"rateLimits":{"limitId":"codex","spendControlReached":false}}),
    );
    assert_eq!(t.status(), Status::Failed);
    assert_eq!(finished_count(&t), 1);
    assert!(c.prompt("blocked").is_err());
}

#[test]
fn retrying_and_terminal_errors_stop_once_and_usage_limits_are_classified() {
    for retry in [true, false] {
        for info in [
            json!("rateLimitExceeded"),
            json!("usageLimitExceeded"),
            json!("sessionBudgetExceeded"),
            json!({"httpConnectionFailed":{"httpStatusCode":429}}),
            json!("other"),
        ] {
            let mut c = ready();
            let mut t = Transcript::new();
            start(&mut c, &mut t);
            let out = notification(
                &mut c,
                &mut t,
                "error",
                json!({"threadId":"thread","turnId":"turn","willRetry":retry,"error":{"message":"failure","codexErrorInfo":info}}),
            );
            assert!(out.effects.contains(&ControlEffect::Shutdown));
            assert!(out.messages.is_empty());
            assert_eq!(t.status(), Status::Failed);
            assert_eq!(
                matches!(t.rate_limit(), Some(RateLimitEvent::Rejected(_))),
                info != json!("other")
            );
            notification(
                &mut c,
                &mut t,
                "turn/completed",
                json!({"threadId":"thread","turn":{"id":"turn","status":"failed","error":{"message":"again"}}}),
            );
            assert_eq!(finished_count(&t), 1);
        }
    }
}

#[test]
fn token_usage_has_no_invented_price_and_signed_counts_are_rejected() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let counts = json!({"inputTokens":10,"cachedInputTokens":4,"outputTokens":3,"reasoningOutputTokens":1,"totalTokens":13});
    notification(
        &mut c,
        &mut t,
        "thread/tokenUsage/updated",
        json!({"threadId":"thread","turnId":"turn","tokenUsage":{"last":counts,"total":counts,"modelContextWindow":128000}}),
    );
    assert_eq!(
        t.usage().context(),
        Some(ContextFill {
            used: 13,
            window: Some(128000)
        })
    );
    assert_eq!(t.usage().main_model(), Some("test-model"));
    let mut invalid = counts;
    invalid["inputTokens"] = json!(-1);
    let out = notification(
        &mut c,
        &mut t,
        "thread/tokenUsage/updated",
        json!({"threadId":"thread","turnId":"turn","tokenUsage":{"last":invalid,"total":invalid}}),
    );
    assert!(out.effects.contains(&ControlEffect::Shutdown));
}

#[test]
fn process_grants_never_cross_codecs_or_survive_transport_failure() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let key = ask(&mut c, &mut t, json!(1), true, json!({}));
    c.answer(key, ApprovalIntent::AllowForSession).unwrap();
    assert_eq!(c.grants()[0].lifetime, GrantLifetime::Process);
    let mut other = ready();
    assert!(other.answer(key, ApprovalIntent::AllowForSession).is_none());
    c.transport_failed("gone");
    assert!(c.grants().is_empty());
}

#[test]
fn unsolicited_turn_malformed_completion_and_duplicate_ids_fail_closed() {
    let mut c = ready();
    let out = feed(
        &mut c,
        json!({"method":"turn/started","params":{"threadId":"thread","turn":{"id":"surprise","status":"inProgress"}}}),
    );
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    item(&mut c, &mut t, command("tool", "inProgress"), true);
    assert_eq!(t.status(), Status::Failed);
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    ask(&mut c, &mut t, json!(1), true, json!({}));
    let out = feed(
        &mut c,
        json!({"id":1,"method":"item/fileChange/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"tool"}}),
    );
    assert_eq!(out.messages[0]["error"]["code"], -32600);
}

fn fixture(name: &str) -> Vec<Value> {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/codex/{name}.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}
fn recorded_result(rows: &[Value], method: &str) -> Value {
    let id = &rows
        .iter()
        .find(|r| r["dir"] == "in" && r["msg"]["method"] == method)
        .unwrap()["msg"]["id"];
    rows.iter()
        .find(|r| r["dir"] == "out" && &r["msg"]["id"] == id && r["msg"].get("method").is_none())
        .unwrap()["msg"]["result"]
        .clone()
}
fn recorded_ready(rows: &[Value], resume: Option<String>) -> (CodexCodec, Transcript) {
    let bootstrap = fixture("read-only");
    let (mut c, init) =
        CodexCodec::new(config(), "/tmp/newjen-codex-scratch".into(), resume.clone()).unwrap();
    let account = respond(&mut c, &init, recorded_result(rows, "initialize"));
    let models = respond(
        &mut c,
        &account,
        recorded_result(&bootstrap, "account/read"),
    );
    let limits = respond(&mut c, &models, recorded_result(&bootstrap, "model/list"));
    let open = respond(
        &mut c,
        &limits,
        recorded_result(&bootstrap, "account/rateLimits/read"),
    );
    let mut opened = recorded_result(
        rows,
        if resume.is_some() {
            "thread/resume"
        } else {
            "thread/start"
        },
    );
    // The runtime intentionally rejects workspace-write. Replay its item traffic under the
    // supported read-only startup policy; test the original response rejection separately.
    opened["sandbox"] = json!({"type":"readOnly","networkAccess":false});
    let out = respond(&mut c, &open, opened);
    assert!(
        matches!(c.runtime(), CodexRuntime::Ready { .. }),
        "{:?}",
        out
    );
    let mut t = Transcript::new();
    t.apply(&open.batch);
    t.apply(&out.batch);
    (c, t)
}
fn replay(name: &str, turns: usize, decisions: usize, stops: usize) -> Transcript {
    let rows = fixture(name);
    let (mut c, mut t) = recorded_ready(&rows, None);
    let mut request_ids = std::collections::HashMap::<ValueKey, Value>::new();
    let mut started = false;
    let mut queued_interrupt = None;
    let mut answers = 0;
    let mut ended = 0;
    let mut interrupted = 0;
    for row in &rows {
        let m = &row["msg"];
        let method = m["method"].as_str();
        if row["dir"] == "in" {
            match method {
                Some("turn/start") => {
                    started = true;
                    let text = m["params"]["input"][0]["text"].as_str().unwrap();
                    t.push_user(text);
                    let out = c.prompt(text).unwrap();
                    assert_eq!(out.messages[0]["params"]["input"], m["params"]["input"]);
                    request_ids
                        .insert(ValueKey(m["id"].to_string()), out.messages[0]["id"].clone());
                }
                Some("turn/interrupt") if started => {
                    let message = queued_interrupt
                        .take()
                        .unwrap_or_else(|| c.interrupt().messages.remove(0));
                    assert_eq!(message["params"], m["params"]);
                    request_ids.insert(ValueKey(m["id"].to_string()), message["id"].clone());
                }
                None if started && m.get("result").is_some() => {
                    let pending = c
                        .pending
                        .iter()
                        .find(|(_, p)| serde_json::to_value(&p.rpc_id).unwrap() == m["id"])
                        .map(|(k, _)| *k)
                        .expect("recorded pending approval");
                    let intent = match m["result"]["decision"].as_str().unwrap() {
                        "accept" => ApprovalIntent::AllowOnce,
                        "acceptForSession" => ApprovalIntent::AllowForSession,
                        "decline" => ApprovalIntent::Deny,
                        "cancel" => ApprovalIntent::DenyAndStop,
                        other => panic!("{other}"),
                    };
                    let mut out = c.answer(pending, intent).unwrap();
                    assert_eq!(
                        serde_json::to_vec(&out.messages[0]).unwrap(),
                        serde_json::to_vec(m).unwrap()
                    );
                    if out.messages.len() > 1 {
                        queued_interrupt = Some(out.messages.remove(1));
                    }
                    t.apply(&out.batch);
                    answers += 1;
                }
                _ => (),
            }
        } else if started {
            let mut m = m.clone();
            if method.is_none() {
                let Some(id) = request_ids.remove(&ValueKey(m["id"].to_string())) else {
                    continue;
                };
                m["id"] = id;
            }
            let out = feed(&mut c, m.clone());
            assert!(
                !out.effects.contains(&ControlEffect::Shutdown),
                "{name}: {m}\n{out:?}"
            );
            t.apply(&out.batch);
            if method == Some("item/completed") {
                let item = &m["params"]["item"];
                match item["type"].as_str().unwrap() {
                    "agentMessage" if !item["text"].as_str().unwrap().is_empty() => {
                        let actual = t
                            .items()
                            .iter()
                            .rev()
                            .find_map(|i| {
                                if let Item::Assistant { text, .. } = i {
                                    Some(text)
                                } else {
                                    None
                                }
                            })
                            .unwrap();
                        assert_eq!(actual, &item["text"].as_str().unwrap());
                    }
                    "commandExecution" | "fileChange" => {
                        let actual=t.items().iter().find(|i|matches!(i,Item::Tool{tool_use_id,..} if tool_use_id==item["id"].as_str().unwrap())).unwrap();
                        let expected = match item["status"].as_str().unwrap() {
                            "completed" => ToolState::Done,
                            "declined" => ToolState::Denied,
                            "failed" => ToolState::Failed,
                            other => panic!("{other}"),
                        };
                        assert!(matches!(actual,Item::Tool{state,..} if *state==expected));
                        if let Some(expected) = item["aggregatedOutput"].as_str() {
                            assert!(
                                matches!(actual,Item::Tool{output:Some(output),..} if output==expected)
                            );
                        }
                    }
                    _ => (),
                }
            }
            if method == Some("turn/completed") {
                ended += 1;
                let stopped = m["params"]["turn"]["status"] == "interrupted";
                interrupted += usize::from(stopped);
                assert_eq!(
                    t.status(),
                    if stopped {
                        Status::Stopped
                    } else {
                        Status::Done
                    }
                );
                assert!(t.pending_approval().is_none());
                assert!(c.pending.is_empty());
            }
        }
    }
    assert_eq!((ended, answers, interrupted), (turns, decisions, stops));
    assert_eq!(finished_count(&t), turns);
    assert!(t.items().iter().all(|i| !matches!(
        i,
        Item::TurnEnd {
            cost_usd: Some(_),
            ..
        }
    )));
    t
}
#[derive(Debug, PartialEq, Eq, Hash)]
struct ValueKey(String);

#[test]
fn recorded_read_only_text_allow_deny_cancel_cache_file_and_interrupt() {
    replay("read-only", 9, 10, 2);
}
#[test]
fn recorded_workspace_write_commands_and_patch() {
    replay("workspace-write", 2, 6, 0);
}
#[test]
fn recorded_resume_establishes_id_without_duplicating_saved_history_or_starting_a_turn() {
    let rows = fixture("resume");
    let id = recorded_result(&rows, "thread/resume")["thread"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (c, t) = recorded_ready(&rows, Some(id.clone()));
    assert_eq!(t.session_id(), Some(id.as_str()));
    assert!(t.items().is_empty());
    assert_eq!(t.status(), Status::Idle);
    assert_eq!(c.runtime(), &CodexRuntime::Ready { thread_id: id });
}

#[test]
#[ignore = "Uses the owner's installed Codex CLI and subscription for one tiny turn"]
fn live_codex_text_reply() {
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    let mut command = Command::new(crate::env::find_cli(crate::env::Cli::Codex));
    crate::env::configure(&mut command);
    command
        .arg("app-server")
        .current_dir("/tmp/newjen-codex-scratch");
    let mut probe =
        crate::probe::Probe::spawn(&mut command, Instant::now() + Duration::from_secs(90)).unwrap();
    let cfg = AgentConfig {
        model: ModelSelection::Model {
            id: "gpt-6-luna".into(),
        },
        effort: EffortSelection::Chosen("low".into()),
        ..config()
    };
    let (mut c, mut out) = CodexCodec::new(cfg, "/tmp/newjen-codex-scratch".into(), None).unwrap();
    let mut t = Transcript::new();
    loop {
        for message in out.messages {
            probe.send(&message).unwrap();
        }
        t.apply(&out.batch);
        assert!(
            !out.effects.contains(&ControlEffect::Shutdown),
            "{:?}",
            out.batch
        );
        if matches!(c.runtime(), CodexRuntime::Ready { .. }) {
            break;
        }
        out = c.stdout(std::str::from_utf8(&probe.line().unwrap()).unwrap());
    }
    t.push_user("Reply exactly OK; use no tools.");
    out = c.prompt("Reply exactly OK; use no tools.").unwrap();
    loop {
        for message in out.messages {
            probe.send(&message).unwrap();
        }
        t.apply(&out.batch);
        assert!(
            !out.effects.contains(&ControlEffect::Shutdown),
            "{:?}",
            out.batch
        );
        if matches!(t.status(), Status::Done) {
            break;
        }
        out = c.stdout(std::str::from_utf8(&probe.line().unwrap()).unwrap());
    }
    assert!(
        t.items()
            .iter()
            .any(|i| matches!(i,Item::Assistant{text,..} if text.contains("OK")))
    );
}

#[test]
fn recorded_credit_backed_admission_opens_and_allows_turns() {
    let rows = fixture("read-only");
    let (mut c, mut t) = recorded_ready(&rows, None);
    let Some(RateLimitEvent::Warning(snapshot)) = t.rate_limit() else {
        panic!("credits warning expected")
    };
    assert!(snapshot.explanation.contains("paid from Codex credits"));
    assert!(
        snapshot
            .windows
            .iter()
            .any(|w| w.resets_at_unix_seconds.is_some())
    );
    assert!(matches!(c.runtime(), CodexRuntime::Ready { .. }));
    t.push_user("go");
    let out = c.prompt("go").unwrap();
    assert_eq!(out.messages[0]["method"], "turn/start");
}

#[test]
fn snapshot_admission_respects_credits_and_spend_controls() {
    for (credits, spend, ordinary, reached, rejected) in [
        (Value::Null, false, Some(false), "", true),
        (
            json!({"hasCredits":false,"unlimited":false}),
            false,
            None,
            "primary",
            true,
        ),
        (
            json!({"hasCredits":false,"unlimited":false}),
            false,
            Some(false),
            "",
            true,
        ),
        (
            json!({"hasCredits":true,"unlimited":false}),
            true,
            Some(false),
            "primary",
            true,
        ),
        (
            json!({"hasCredits":false,"unlimited":true}),
            false,
            Some(false),
            "primary",
            false,
        ),
    ] {
        let (c, init) = new();
        let (mut c, limits) = through_models(c, init);
        let report = json!({"ordinaryUsageAllowed":ordinary,"rateLimits":{
            "limitId":"codex","rateLimitReachedType":reached,"credits":credits,
            "spendControlReached":spend,"primary":{"usedPercent":100,"resetsAt":1800000000}
        }});
        let out = respond(&mut c, &limits, report.clone());
        let mut t = Transcript::new();
        t.apply(&out.batch);
        if rejected {
            assert_eq!(c.runtime(), &CodexRuntime::Blocked);
            assert!(out.messages.is_empty());
            assert!(out.effects.contains(&ControlEffect::Shutdown));
            let Some(RateLimitEvent::Rejected(snapshot)) = t.rate_limit() else {
                panic!("rejection expected")
            };
            assert_eq!(snapshot.windows[0].resets_at_unix_seconds, Some(1800000000));
            assert_eq!(t.status(), Status::Failed);
            t.apply(&c.interrupt().batch);
            t.apply(&c.transport_failed("process exited").batch);
            assert_eq!(t.status(), Status::Failed);
            assert_eq!(finished_count(&t), 1);
            assert!(c.prompt("blocked").is_err());
        } else {
            assert_eq!(out.messages[0]["method"], "thread/start");
            let Some(RateLimitEvent::Warning(snapshot)) = t.rate_limit() else {
                panic!("credits warning expected")
            };
            assert!(snapshot.explanation.contains("paid from Codex credits"));
            respond(&mut c, &out, open_result());
            let updated = notification(&mut c, &mut t, "account/rateLimits/updated", report);
            assert!(updated.messages.is_empty());
            assert!(updated.effects.is_empty());
            assert!(matches!(t.rate_limit(), Some(RateLimitEvent::Warning(_))));
            assert!(c.prompt("go").is_ok());
        }
    }
}

#[test]
fn cli_limit_refusals_override_credit_backed_admission_without_retry() {
    for method in ["error", "turn/completed"] {
        let rows = fixture("read-only");
        let (mut c, mut t) = recorded_ready(&rows, None);
        let CodexRuntime::Ready { thread_id } = c.runtime().clone() else {
            panic!("ready")
        };
        let prompt = c.prompt("go").unwrap();
        let active = respond(
            &mut c,
            &prompt,
            json!({"turn":{"id":"turn","status":"inProgress"}}),
        );
        t.apply(&active.batch);
        let error = json!({"message":"Usage limit reached","codexErrorInfo":"usageLimitExceeded"});
        let params = if method == "error" {
            json!({"threadId":thread_id,"turnId":"turn","willRetry":true,"error":error})
        } else {
            json!({"threadId":thread_id,"turn":{"id":"turn","status":"failed","error":error}})
        };
        let out = notification(&mut c, &mut t, method, params);
        let Some(RateLimitEvent::Rejected(snapshot)) = t.rate_limit() else {
            panic!("CLI refusal must reject even with credits")
        };
        assert!(
            snapshot
                .windows
                .iter()
                .any(|w| w.resets_at_unix_seconds.is_some())
        );
        assert_eq!(t.status(), Status::Failed);
        assert_eq!(c.runtime(), &CodexRuntime::Blocked);
        assert!(out.messages.is_empty());
        assert!(out.effects.contains(&ControlEffect::Shutdown));
        assert!(c.prompt("retry").is_err());
    }
}

#[test]
fn real_workspace_write_open_is_rejected_and_file_grants_expire_on_shutdown() {
    let rows = fixture("workspace-write");
    let (c, init) = new();
    let (mut c, limits) = through_models(c, init);
    let open = respond(&mut c, &limits, json!({"rateLimits":{}}));
    let out = respond(&mut c, &open, recorded_result(&rows, "thread/start"));
    assert_eq!(c.runtime(), &CodexRuntime::Blocked);
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let key = ask(
        &mut c,
        &mut t,
        json!(1),
        true,
        json!({"environmentId":"local"}),
    );
    c.answer(key, ApprovalIntent::AllowForSession).unwrap();
    let stale = ask(&mut c, &mut t, json!(2), false, json!({}));
    let out = c.shutdown();
    assert_eq!(c.runtime(), &CodexRuntime::Closed);
    assert!(c.grants().is_empty());
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    assert!(c.answer(stale, ApprovalIntent::AllowOnce).is_none());
}

#[test]
fn reasoning_and_plan_never_become_assistant_replies_and_foreign_turns_do_not_mutate() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    for kind in ["reasoning", "plan"] {
        item(
            &mut c,
            &mut t,
            json!({"type":kind,"id":kind,"text":"hidden","summary":["hidden"]}),
            true,
        );
    }
    for method in [
        "item/reasoning/textDelta",
        "item/reasoning/summaryTextDelta",
        "item/plan/delta",
    ] {
        let out = notification(
            &mut c,
            &mut t,
            method,
            json!({"threadId":"thread","turnId":"turn","itemId":"reason","delta":"hidden"}),
        );
        assert!(out.batch.events.is_empty());
    }
    notification(
        &mut c,
        &mut t,
        "item/agentMessage/delta",
        json!({"threadId":"other","turnId":"turn","itemId":"m","delta":"wrong"}),
    );
    assert_eq!(t.items().len(), 1);
}

#[test]
fn policy_drift_and_stop_deadline_expiry_fail_closed() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    c.interrupt();
    let out = c.transport_failed("Stop deadline elapsed");
    t.apply(&out.batch);
    assert_eq!(t.status(), Status::Failed);
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    let mut c = ready();
    let out = feed(
        &mut c,
        json!({"method":"thread/settings/updated","params":{"threadId":"thread","threadSettings":{"model":"test-model","cwd":"/tmp/newjen-codex-scratch","approvalPolicy":"never","approvalsReviewer":"user","sandboxPolicy":{"type":"readOnly"},"serviceTier":"default"}}}),
    );
    assert_eq!(c.runtime(), &CodexRuntime::Blocked);
    assert!(out.effects.contains(&ControlEffect::Shutdown));
}

#[test]
fn bad_pagination_and_unavailable_rate_limit_read_do_not_start_turns() {
    let (mut c, init) = new();
    let account = respond(&mut c, &init, json!({"userAgent":"codex-cli/0.166.0"}));
    let models = respond(
        &mut c,
        &account,
        json!({"account":{"type":"apiKey"},"requiresOpenaiAuth":true}),
    );
    let mut page = model_page();
    page["nextCursor"] = json!("page");
    let next = respond(&mut c, &models, page.clone());
    assert_eq!(next.messages[0]["params"]["cursor"], "page");
    let out = respond(&mut c, &next, page);
    assert!(out.effects.contains(&ControlEffect::Shutdown));
    let (c, init) = new();
    let (mut c, limits) = through_models(c, init);
    let out = feed(
        &mut c,
        json!({"id":limits.messages[0]["id"],"error":{"code":-1,"message":"Unavailable"}}),
    );
    assert_eq!(out.messages[0]["method"], "thread/start");
    assert_eq!(c.runtime(), &CodexRuntime::Opening);
}

#[test]
fn completed_item_closes_unanswered_callbacks_and_late_approvals_cannot_erase_denial() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let a = ask(&mut c, &mut t, json!(1), true, json!({}));
    item(&mut c, &mut t, command("tool", "declined"), true);
    assert!(t.pending_approval().is_none());
    assert!(c.answer(a, ApprovalIntent::AllowOnce).is_none());
    let late = feed(
        &mut c,
        json!({"id":"late","method":"item/commandExecution/requestApproval","params":{"threadId":"thread","turnId":"turn","itemId":"tool","command":"touch a","cwd":"/work"}}),
    );
    assert_eq!(late.messages[0]["error"]["code"], -32600);
    assert!(c.pending.is_empty());
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let a = ask(&mut c, &mut t, json!(1), true, json!({}));
    t.apply(&c.answer(a, ApprovalIntent::Deny).unwrap().batch);
    let b = ask(&mut c, &mut t, json!(2), true, json!({}));
    assert!(matches!(
        t.items()[1],
        Item::Tool {
            state: ToolState::Denied,
            ..
        }
    ));
    t.apply(&c.answer(b, ApprovalIntent::AllowOnce).unwrap().batch);
    assert!(matches!(
        t.items()[1],
        Item::Tool {
            state: ToolState::Denied,
            ..
        }
    ));
}

#[test]
fn empty_deltas_do_not_create_blank_reply_rows() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    notification(
        &mut c,
        &mut t,
        "item/agentMessage/delta",
        json!({"threadId":"thread","turnId":"turn","itemId":"m","delta":""}),
    );
    assert_eq!(t.items().len(), 1);
}

fn limits_result() -> Value {
    json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":10,"resetsAt":1800000000}}})
}

#[test]
fn instructions_reach_codex_as_developer_instructions_when_the_thread_opens() {
    let (mut codec, init) = new();
    codec.set_instructions(Some("Use jj, not git.".into()));
    let (mut codec, limits) = through_models(codec, init);
    let open = respond(&mut codec, &limits, limits_result());
    assert_eq!(open.messages[0]["method"], "thread/start");
    assert_eq!(
        open.messages[0]["params"]["developerInstructions"],
        "Use jj, not git."
    );
    let (mut codec, init) = CodexCodec::new(
        config(),
        "/tmp/newjen-codex-scratch".into(),
        Some("thread".into()),
    )
    .unwrap();
    codec.set_instructions(Some("Use jj, not git.".into()));
    let (mut codec, limits) = through_models(codec, init);
    let open = respond(&mut codec, &limits, limits_result());
    assert_eq!(open.messages[0]["method"], "thread/resume");
    assert_eq!(
        open.messages[0]["params"]["developerInstructions"],
        "Use jj, not git."
    );

    let (mut codec, init) = new();
    codec.set_instructions(Some("  ".into()));
    let (mut codec, limits) = through_models(codec, init);
    let open = respond(&mut codec, &limits, limits_result());
    assert!(
        open.messages[0]["params"]
            .get("developerInstructions")
            .is_none()
    );
    assert!(
        !open.messages[0]["params"]
            .to_string()
            .contains("--append-system-prompt")
    );
}

#[test]
fn file_change_previews_become_one_patch_or_are_honestly_unavailable() {
    use crate::edits::{EditPreview, FileChange};
    let cwd = std::path::Path::new("/work");
    let tool = |arguments| ToolDescriptor {
        id: "tool".into(),
        name: "File change".into(),
        display_name: "File change".into(),
        summary: String::new(),
        arguments,
    };
    let proposal = edit_proposal(
        &tool(json!([
            {"path":"/work/a.txt","kind":{"type":"update","move_path":null},"diff":"@@ -1 +1 @@\n-a\n+b\n"},
            {"path":"/work/new.txt","kind":{"type":"add"},"diff":"one\ntwo"},
            {"path":"/work/old.txt","kind":{"type":"delete"},"diff":"gone\n"},
            {"path":"/work/m.txt","kind":{"type":"update","move_path":"/work/n.txt"},"diff":"@@ -1 +1 @@\n-m\n+n\n\n\nMoved to: /work/n.txt"}
        ])),
        cwd,
    );
    assert_eq!(proposal.path, std::path::PathBuf::from("/work/a.txt"));
    let FileChange::Patch(patch) = &proposal.change else {
        panic!("patch")
    };
    assert_eq!(
        patch,
        "diff --git \"a/work/a.txt\" \"b/work/a.txt\"\n--- \"a/work/a.txt\"\n+++ \"b/work/a.txt\"\n@@ -1 +1 @@\n-a\n+b\n\
         diff --git \"a/work/new.txt\" \"b/work/new.txt\"\n--- /dev/null\n+++ \"b/work/new.txt\"\n@@ -0,0 +1,2 @@\n+one\n+two\n\\ No newline at end of file\n\
         diff --git \"a/work/old.txt\" \"b/work/old.txt\"\n--- \"a/work/old.txt\"\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-gone\n\
         diff --git \"a/work/m.txt\" \"b/work/m.txt\"\n--- \"a/work/m.txt\"\n+++ \"b/work/m.txt\"\n@@ -1 +1 @@\n-m\n+n\n"
    );
    assert!(matches!(proposal.preview(), EditPreview::Patch(_)));
    for arguments in [Value::Null, json!([]), json!([{"path":"/work/a.txt"}])] {
        let proposal = edit_proposal(&tool(arguments), cwd);
        assert!(matches!(proposal.change, FileChange::Unavailable));
        assert!(matches!(proposal.preview(), EditPreview::Unavailable(_)));
    }
}

#[test]
fn a_denial_note_is_kept_because_codex_decisions_carry_no_text() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    let key = ask(&mut c, &mut t, json!(1), false, json!({"grantRoot":"/"}));
    let out = c.answer(key, ApprovalIntent::Deny).unwrap();
    assert_eq!(out.messages[0]["result"], json!({"decision":"decline"}));
    assert!(!out.messages[0].to_string().contains("message"));
}

fn pending(codec: &mut CodexCodec, extra: Value) -> (String, Option<ApprovalRisk>) {
    let mut t = Transcript::new();
    start(codec, &mut t);
    ask(codec, &mut t, json!(1), true, extra);
    let request = t.pending_approval().unwrap();
    (request.headline().to_string(), request.risk())
}

#[test]
fn recorded_command_approvals_say_what_runs_and_tag_nothing_invented() {
    let mut seen = 0;
    for name in ["read-only", "workspace-write", "runtime-command-accept"] {
        let path = format!(
            "{}/tests/fixtures/codex/{name}.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            if line["dir"] != "out"
                || line["msg"]["method"] != "item/commandExecution/requestApproval"
            {
                continue;
            }
            let params = line["msg"]["params"].clone();
            let command = params["command"].as_str().unwrap().to_string();
            let inner = command
                .strip_prefix("/bin/zsh -lc '")
                .and_then(|rest| rest.strip_suffix('\''))
                .unwrap_or(&command);
            let program = inner.split(' ').next().unwrap();
            let extra = json!({"command":params["command"],"cwd":"/tmp/newjen-codex-scratch","commandActions":params["commandActions"]});
            let mut codec = ready();
            let (headline, risk) = pending(&mut codec, extra);
            assert_eq!(headline, format!("Run {program}"), "{command}");
            assert_eq!(risk, None, "{command}");
            seen += 1;
        }
    }
    assert!(seen >= 5);
}

#[test]
fn command_headlines_come_from_action_types_else_the_first_word() {
    let read = |name: &str| json!({"type":"read","command":"cat","name":name,"path":format!("/work/src/{name}")});
    let cases = [
        (
            json!({"command":"cat a.rs","commandActions":[read("a.rs")]}),
            "Read a.rs",
        ),
        (
            json!({"commandActions":[read("a.rs"), read("a.rs")]}),
            "Read a.rs",
        ),
        (
            json!({"commandActions":[read("a.rs"), read("b.rs")]}),
            "Read 2 files",
        ),
        (
            json!({"commandActions":[{"type":"read","path":"/work/x/c.rs"}]}),
            "Read c.rs",
        ),
        (
            json!({"commandActions":[{"type":"search","command":"rg","query":"fn main","path":null}]}),
            "Search for \"fn main\"",
        ),
        (
            json!({"commandActions":[{"type":"search","command":"rg","query":null}]}),
            "Search files",
        ),
        (
            json!({"commandActions":[{"type":"listFiles","command":"ls","path":"src/app"}]}),
            "List files in app",
        ),
        (
            json!({"commandActions":[{"type":"listFiles","command":"ls","path":null}]}),
            "List files",
        ),
        (
            json!({"commandActions":[read("a.rs"), {"type":"search","command":"rg","query":"x"}]}),
            "Run touch",
        ),
        (
            json!({"commandActions":[{"type":"unknown","command":"touch a"}]}),
            "Run touch",
        ),
        (json!({"commandActions":[]}), "Run touch"),
        (json!({"commandActions":"junk"}), "Run touch"),
        (
            json!({"command":"/bin/zsh -lc 'cd sub && cargo test'"}),
            "Run cargo",
        ),
        (
            json!({"kind":"writeStdin","command":null}),
            "Send input to a running command",
        ),
        (json!({"command":null}), "Run a command"),
    ];
    for (extra, expected) in cases {
        let mut codec = ready();
        assert_eq!(pending(&mut codec, extra.clone()).0, expected, "{extra}");
    }
}

#[test]
fn command_risks_follow_the_command_and_codexs_network_context() {
    let cases = [
        (
            json!({"command":"/bin/zsh -lc 'rm -rf build'"}),
            Some(ApprovalRisk::DeletesFiles),
        ),
        (
            json!({"command":"git reset --hard"}),
            Some(ApprovalRisk::RewritesHistory),
        ),
        (
            json!({"command":"cat /etc/hosts"}),
            Some(ApprovalRisk::OutsideProject),
        ),
        (
            json!({"command":"cat ../../x","cwd":"/tmp/newjen-codex-scratch/a"}),
            Some(ApprovalRisk::OutsideProject),
        ),
        (
            json!({"command":"cat ../x","cwd":"/tmp/newjen-codex-scratch/a"}),
            None,
        ),
        (
            json!({"command":"curl https://example.com"}),
            Some(ApprovalRisk::UsesNetwork),
        ),
        (
            json!({"command":"touch a","networkApprovalContext":{"host":"example.com"}}),
            Some(ApprovalRisk::UsesNetwork),
        ),
        (
            json!({"command":"rm a","networkApprovalContext":{"host":"example.com"}}),
            Some(ApprovalRisk::DeletesFiles),
        ),
        (json!({"command":"touch a"}), None),
    ];
    for (extra, expected) in cases {
        let mut codec = ready();
        assert_eq!(pending(&mut codec, extra.clone()).1, expected, "{extra}");
    }
}

#[test]
fn file_change_headlines_and_risks_follow_the_changes_and_update_in_place() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    ask(&mut c, &mut t, json!(1), false, json!({"grantRoot":"/"}));
    let shown = |t: &Transcript| {
        let request = t.pending_approval().unwrap();
        (request.headline().to_string(), request.risk())
    };
    assert_eq!(shown(&t), ("Edit files".to_string(), None));
    let update = |kind: &str, path: &str| json!({"path":path,"kind":{"type":kind,"move_path":null},"diff":"-a\n+b"});
    let cases = [
        (
            json!([update("update", "/tmp/newjen-codex-scratch/a.rs")]),
            "Edit a.rs",
            None,
        ),
        (
            json!([update("add", "/tmp/newjen-codex-scratch/new.rs")]),
            "Create new.rs",
            None,
        ),
        (
            json!([update("delete", "/tmp/newjen-codex-scratch/old.rs")]),
            "Delete old.rs",
            Some(ApprovalRisk::DeletesFiles),
        ),
        (
            json!([
                update("update", "/tmp/newjen-codex-scratch/a"),
                update("update", "/tmp/newjen-codex-scratch/b"),
                update("update", "/tmp/newjen-codex-scratch/c")
            ]),
            "Edit 3 files",
            None,
        ),
        (
            json!([
                update("add", "/tmp/newjen-codex-scratch/a"),
                update("add", "/tmp/newjen-codex-scratch/b")
            ]),
            "Create 2 files",
            None,
        ),
        (
            json!([
                update("add", "/tmp/newjen-codex-scratch/a"),
                update("update", "/tmp/newjen-codex-scratch/b")
            ]),
            "Change 2 files",
            None,
        ),
        (
            json!([
                update("update", "/tmp/newjen-codex-scratch/a"),
                update("delete", "/tmp/newjen-codex-scratch/b")
            ]),
            "Change 2 files",
            Some(ApprovalRisk::DeletesFiles),
        ),
        (
            json!([update("update", "/etc/hosts")]),
            "Edit hosts",
            Some(ApprovalRisk::OutsideProject),
        ),
        (
            json!([update("update", "/tmp/newjen-codex-scratch/src/a.rs")]),
            "Edit a.rs",
            None,
        ),
    ];
    for (changes, headline, risk) in cases {
        notification(
            &mut c,
            &mut t,
            "item/fileChange/patchUpdated",
            json!({"threadId":"thread","turnId":"turn","itemId":"tool","changes":changes}),
        );
        assert_eq!(shown(&t), (headline.to_string(), risk), "{changes}");
        assert_eq!(
            t.pending_approval().unwrap().view().question,
            "Allow these file changes once?"
        );
    }
}

#[test]
fn recorded_codex_turns_read_as_plain_english_steps() {
    use crate::steps::{WorkState, activity_line, work_blocks};
    let read_only = work_blocks(&replay("read-only", 9, 10, 2), None);
    assert_eq!(read_only.len(), 9);
    let labels = |blocks: &[crate::steps::WorkBlock]| -> Vec<String> {
        blocks
            .iter()
            .flat_map(|block| block.steps.iter().map(|step| step.label.headline.clone()))
            .collect()
    };
    let labels_read_only = labels(&read_only);
    assert!(!labels_read_only.is_empty());
    assert_eq!(
        labels_read_only[..3],
        [
            "Ran touch once.txt",
            "Run touch denied.txt",
            "Run touch cancelled.txt"
        ],
        "{labels_read_only:?}"
    );
    assert!(
        read_only
            .iter()
            .any(|block| block.state == WorkState::Stopped)
    );

    let transcript = replay("workspace-write", 2, 6, 0);
    let blocks = work_blocks(&transcript, None);
    assert_eq!(blocks.len(), 2);
    let all = labels(&blocks);
    assert!(
        all.contains(&"Ran touch workspace.txt".to_string()),
        "{all:?}"
    );
    assert!(
        all.contains(&"Created patch-workspace.txt".to_string()),
        "{all:?}"
    );
    let patch = blocks
        .iter()
        .flat_map(|block| &block.steps)
        .find(|step| step.label.headline == "Created patch-workspace.txt")
        .unwrap();
    assert_eq!(patch.state, ToolState::Done);
    assert_eq!(patch.label.detail, None);
    assert!(matches!(
        activity_line(&transcript).as_deref(),
        Some("Answered")
    ));
}

fn token_counts(input: u64, cached: u64, output: u64) -> Value {
    json!({"inputTokens":input,"cachedInputTokens":cached,"outputTokens":output,"reasoningOutputTokens":0,"totalTokens":input + output})
}
fn token_usage(c: &mut CodexCodec, t: &mut Transcript, total: Value, last: Value) -> CodecOutput {
    notification(
        c,
        t,
        "thread/tokenUsage/updated",
        json!({"threadId":"thread","turnId":"turn","tokenUsage":{"last":last,"total":total,"modelContextWindow":1000}}),
    )
}
fn complete(c: &mut CodexCodec, t: &mut Transcript) -> CodecOutput {
    notification(
        c,
        t,
        "turn/completed",
        json!({"threadId":"thread","turn":{"id":"turn","status":"completed"}}),
    )
}
fn turn_usage(out: &CodecOutput) -> Option<&Vec<ModelTurn>> {
    out.batch.events.iter().find_map(|event| match event {
        AgentEvent::TurnUsage(turns) => Some(turns),
        _ => None,
    })
}

#[test]
fn a_turns_tokens_are_the_growth_of_the_thread_total_with_cache_moved_out_of_input() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    token_usage(
        &mut c,
        &mut t,
        token_counts(100, 0, 10),
        token_counts(100, 0, 10),
    );
    token_usage(
        &mut c,
        &mut t,
        token_counts(260, 100, 25),
        token_counts(160, 100, 15),
    );
    let out = complete(&mut c, &mut t);
    let turns = turn_usage(&out).expect("usage at the turn's end");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].model, "test-model");
    assert_eq!(
        turns[0].tokens,
        TokenCounts {
            input: 160,
            cached_input: 100,
            cache_write_input: 0,
            output: 25,
            reasoning_output: 0,
        }
    );
    assert_eq!(turns[0].tokens.total(), 285);
    assert_eq!(turns[0].cost_usd, None);
    assert_eq!(turns[0].context_window, Some(1000));
    let position =
        |wanted: fn(&AgentEvent) -> bool| out.batch.events.iter().position(wanted).unwrap();
    assert!(
        position(|e| matches!(e, AgentEvent::TurnUsage(_)))
            < position(|e| matches!(e, AgentEvent::TurnFinished { .. }))
    );

    start(&mut c, &mut t);
    token_usage(
        &mut c,
        &mut t,
        token_counts(400, 150, 40),
        token_counts(140, 50, 15),
    );
    let out = complete(&mut c, &mut t);
    assert_eq!(turn_usage(&out).unwrap()[0].tokens.total(), 155);
    assert_eq!(t.usage().turns(), 2);
    assert_eq!(t.usage().total().total(), 440);
    assert_eq!(t.usage().last_turn().unwrap().total(), 155);
    assert_eq!(
        t.usage().context(),
        Some(ContextFill {
            used: 155,
            window: Some(1000)
        })
    );
}

#[test]
fn a_resumed_threads_first_notification_recovers_where_the_total_stood() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    token_usage(
        &mut c,
        &mut t,
        token_counts(5000, 4000, 100),
        token_counts(200, 150, 20),
    );
    let out = complete(&mut c, &mut t);
    assert_eq!(turn_usage(&out).unwrap()[0].tokens.total(), 220);
}

#[test]
fn a_failed_or_stopped_turn_still_reports_what_it_used_and_an_unused_one_reports_nothing() {
    let mut c = ready();
    let mut t = Transcript::new();
    start(&mut c, &mut t);
    token_usage(
        &mut c,
        &mut t,
        token_counts(100, 0, 10),
        token_counts(100, 0, 10),
    );
    let out = c.interrupt();
    t.apply(&out.batch);
    assert_eq!(turn_usage(&out), None, "interrupt is only requested here");
    let out = notification(
        &mut c,
        &mut t,
        "turn/completed",
        json!({"threadId":"thread","turn":{"id":"turn","status":"interrupted"}}),
    );
    assert_eq!(turn_usage(&out).unwrap()[0].tokens.total(), 110);

    start(&mut c, &mut t);
    let out = complete(&mut c, &mut t);
    assert_eq!(turn_usage(&out), None);
    assert_eq!(t.usage().turns(), 1);
}

#[test]
fn recorded_codex_threads_report_the_growth_of_total_and_the_fill_of_the_last_call() {
    let write = replay("workspace-write", 2, 6, 0);
    let usage = write.usage();
    assert_eq!(usage.main_model(), Some("gpt-6-luna"));
    assert_eq!(usage.turns(), 2);
    assert_eq!(usage.total().total(), 84047);
    assert_eq!(usage.last_turn().unwrap().total(), 84047 - 36048);
    assert_eq!(usage.total().cached_input, 59136);
    assert_eq!(usage.total().output, 378);
    assert_eq!(
        usage.context(),
        Some(ContextFill {
            used: 24030,
            window: Some(258400)
        })
    );
    assert_eq!(usage.cost_usd(), None);

    let read = replay("read-only", 9, 10, 2);
    let usage = read.usage();
    assert_eq!(usage.total().total(), 284817);
    assert_eq!(
        usage.turns(),
        8,
        "the repeated report after the last stop adds nothing"
    );
    assert_eq!(usage.last_turn().unwrap().total(), 284817 - 241788);
}
