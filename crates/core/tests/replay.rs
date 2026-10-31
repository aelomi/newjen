//! Replays recorded sessions through `Transcript` and checks the resulting conversation.
//!
//! `out` lines are fed in as `SessionEvent::Line`. For `in` lines we do what the app would do:
//! a user prompt becomes `push_user`, and an approval answer becomes `resolve_approval`.

mod common;

use newjen_core::ApprovalIntent;
use newjen_core::protocol::Incoming;
use newjen_core::providers::claude::events::ClaudeEvents;
use newjen_core::session::SessionEvent;
use newjen_core::transcript::{Item, Status, ToolState, Transcript};
use serde_json::Value;

struct Replay {
    /// The transcript after the whole fixture.
    end: Transcript,
    /// A copy of the transcript right after each `result` line.
    after_each_result: Vec<Transcript>,
    /// True if an approval was ever pending.
    saw_pending_approval: bool,
}

fn replay(name: &str) -> Replay {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    let mut after_each_result = Vec::new();
    let mut saw_pending_approval = false;
    let mut prefixes = Vec::new();
    let mut approval_ids = std::collections::HashMap::new();
    for line in common::load(name) {
        if line.out {
            let incoming = Incoming::from_value(line.msg.clone());
            let is_result = matches!(incoming, Incoming::Result(_));
            let batch = codec.translate(&SessionEvent::Line(incoming), None);
            for event in &batch.events {
                if let newjen_core::agent::AgentEvent::ApprovalRequested(request) = event {
                    approval_ids.insert(
                        request.key(),
                        line.msg["request_id"].as_str().unwrap().to_string(),
                    );
                }
            }
            transcript.apply(&batch);
            saw_pending_approval |= transcript.pending_approval().is_some();
            if is_result {
                after_each_result.push(transcript.clone());
            }
            prefixes.push(snapshot(&transcript, &approval_ids));
            continue;
        }
        match line.msg["type"].as_str() {
            Some("user") => {
                let text = line.msg["message"]["content"].as_str().unwrap_or_default();
                transcript.push_user(text);
            }
            Some("control_response") => {
                let request_id = line.msg["response"]["request_id"].as_str().unwrap();
                let pending = transcript
                    .pending_approval()
                    .expect("an approval should be pending when the fixture answers one");
                assert_eq!(approval_ids[&pending.key()], request_id);
                let key = pending.key();
                let answer = answer_from(&line.msg["response"]["response"]);
                codec.answer(key, answer).expect("live fixture approval");
                transcript.resolve_approval(key, answer);
            }
            _ => {} // initialize / interrupt requests: nothing to record.
        }
        prefixes.push(snapshot(&transcript, &approval_ids));
    }
    let expected: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(
            common::fixture_path(name)
                .parent()
                .unwrap()
                .join("prefixes")
                .join(format!("{name}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(prefixes, expected, "fixture {name}");
    Replay {
        end: transcript,
        after_each_result,
        saw_pending_approval,
    }
}

/// The `ApprovalIntent` a recorded control_response corresponds to.
fn answer_from(response: &Value) -> ApprovalIntent {
    match (
        response["behavior"].as_str(),
        response["interrupt"].as_bool(),
    ) {
        (Some("deny"), Some(true)) => ApprovalIntent::DenyAndStop,
        (Some("deny"), _) => ApprovalIntent::Deny,
        _ if response.get("updatedPermissions").is_some() => ApprovalIntent::AllowForSession,
        _ => ApprovalIntent::AllowOnce,
    }
}

fn assistant_texts(transcript: &Transcript) -> Vec<String> {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn tools(transcript: &Transcript) -> Vec<(String, ToolState)> {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Tool { name, state, .. } => Some((name.clone(), *state)),
            _ => None,
        })
        .collect()
}

fn turn_ends(transcript: &Transcript) -> Vec<(Option<f64>, bool)> {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::TurnEnd {
                cost_usd, stopped, ..
            } => Some((*cost_usd, *stopped)),
            _ => None,
        })
        .collect()
}

fn errors(transcript: &Transcript) -> Vec<(String, String)> {
    transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Error { title, detail, .. } => Some((title.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

fn assert_close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("a cost");
    assert!(
        (actual - expected).abs() < 1e-9,
        "cost {actual} != expected {expected}"
    );
}

#[test]
fn text_reply() {
    let transcript = replay("text_reply").end;
    let items = transcript.items();
    assert_eq!(items.len(), 3, "{items:#?}");
    assert!(
        matches!(&items[0], Item::User { text, .. } if text == "Reply with exactly: hello from claude")
    );
    assert!(matches!(
        &items[1],
        Item::Assistant { text, live: false, interrupted: false, .. } if text == "hello from claude"
    ));
    assert!(matches!(
        &items[2],
        Item::TurnEnd {
            stopped: false,
            duration_ms: Some(963),
            ..
        }
    ));
    assert_close(turn_ends(&transcript)[0].0, 0.00344418);
    assert_eq!(transcript.status(), Status::Done);
    assert_eq!(
        transcript.session_id(),
        Some("74b7cce7-9f89-4b9f-a1b3-9b60af5f6b8a")
    );
    assert_eq!(transcript.model(), Some("claude-haiku-5-5"));
}

#[test]
fn tool_allow() {
    let replay = replay("tool_allow");
    assert!(
        replay.saw_pending_approval,
        "the approval should have been queued"
    );
    let transcript = replay.end;
    assert_eq!(tools(&transcript), [("Bash".to_string(), ToolState::Done)]);
    let Some(Item::Tool {
        summary,
        output,
        took,
        ..
    }) = transcript
        .items()
        .iter()
        .find(|item| matches!(item, Item::Tool { .. }))
    else {
        unreachable!()
    };
    assert_eq!(summary, "touch newjen_probe.txt");
    assert_eq!(output.as_deref(), Some("(Bash completed with no output)"));
    assert!(took.is_some());
    assert!(transcript.pending_approval().is_none());
    assert_eq!(transcript.status(), Status::Done);
    assert_eq!(turn_ends(&transcript).len(), 1);
}

#[test]
fn tool_allow_status_while_waiting() {
    // Stop right after the approval request arrives.
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    for line in common::load("tool_allow") {
        if line.out {
            transcript
                .apply(&codec.translate(&SessionEvent::Line(Incoming::from_value(line.msg)), None));
        } else if line.msg["type"] == "user" {
            transcript.push_user("prompt");
        } else {
            break;
        }
    }
    assert_eq!(transcript.status(), Status::Waiting);
    assert_eq!(
        tools(&transcript),
        [("Bash".to_string(), ToolState::Waiting)]
    );
    let request_id = transcript.pending_approval().unwrap().key();
    transcript.resolve_approval(request_id, ApprovalIntent::AllowOnce);
    assert_eq!(transcript.status(), Status::Working);
    assert_eq!(
        tools(&transcript),
        [("Bash".to_string(), ToolState::Running)]
    );
}

#[test]
fn tool_deny() {
    let transcript = replay("tool_deny").end;
    assert_eq!(
        tools(&transcript),
        [("Bash".to_string(), ToolState::Denied)]
    );
    // A denied call never ran, so it has no duration (the wait for the user doesn't count).
    let denied_took = transcript.items().iter().find_map(|item| match item {
        Item::Tool { took, .. } => Some(*took),
        _ => None,
    });
    assert_eq!(denied_took, Some(None));
    assert_eq!(transcript.status(), Status::Done);
    assert!(assistant_texts(&transcript)[0].starts_with("The command didn't run"));
}

#[test]
fn tool_auto_allowed_readonly() {
    let replay = replay("tool_auto_allowed_readonly");
    assert!(
        !replay.saw_pending_approval,
        "ls should never need an approval"
    );
    assert_eq!(tools(&replay.end), [("Bash".to_string(), ToolState::Done)]);
    assert_eq!(replay.end.status(), Status::Done);
}

#[test]
fn partial_builds_text_from_deltas_then_finalizes() {
    // Feed lines up to (not including) the full assistant line: the deltas alone must have
    // produced a live assistant item.
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    for line in common::load("partial") {
        if !line.out {
            transcript.push_user("Reply with exactly: hello from claude");
            continue;
        }
        if line.msg["type"] == "assistant" {
            break;
        }
        transcript
            .apply(&codec.translate(&SessionEvent::Line(Incoming::from_value(line.msg)), None));
    }
    assert!(matches!(
        transcript.items().last(),
        Some(Item::Assistant { text, live: true, .. }) if text == "hello from claude"
    ));

    // The whole fixture ends in the same shape as the non-streaming text_reply.
    let transcript = replay("partial").end;
    let items = transcript.items();
    assert_eq!(items.len(), 3, "{items:#?}");
    assert!(matches!(&items[0], Item::User { .. }));
    assert!(matches!(
        &items[1],
        Item::Assistant { text, live: false, .. } if text == "hello from claude"
    ));
    assert!(matches!(&items[2], Item::TurnEnd { stopped: false, .. }));
    assert_eq!(transcript.status(), Status::Done);
}

#[test]
fn partial_tool_allow() {
    let replay = replay("partial_tool_allow");
    assert!(replay.saw_pending_approval);
    assert_eq!(tools(&replay.end), [("Bash".to_string(), ToolState::Done)]);
    assert_eq!(
        assistant_texts(&replay.end),
        ["Yes, it worked: `touch newjen_probe.txt` ran with no output and no error."]
    );
    assert_eq!(replay.end.status(), Status::Done);
}

#[test]
fn multi_turn_costs_are_per_turn() {
    let transcript = replay("multi_turn").end;
    let ends = turn_ends(&transcript);
    assert_eq!(ends.len(), 2);
    assert_close(ends[0].0, 0.00295581);
    // Turn 2 = its own model usage, not the running total (0.0058392).
    assert_close(ends[1].0, 0.00288339);
    assert_eq!(assistant_texts(&transcript), ["ok", "pineapple"]);
    assert_eq!(transcript.status(), Status::Done);
}

#[test]
fn resume_cost_is_not_the_running_total() {
    let transcript = replay("resume").end;
    let ends = turn_ends(&transcript);
    assert_eq!(ends.len(), 1);
    assert_close(ends[0].0, 0.00034044); // The running total would be 0.00617964.
    assert_eq!(
        transcript.session_id(),
        Some("98c40b6c-53aa-4f22-bf81-ebdecbe396af")
    );
}

#[test]
fn interrupt_stops_and_marks_the_reply_interrupted() {
    let replay = replay("interrupt");
    let after_stop = &replay.after_each_result[0];
    assert_eq!(after_stop.status(), Status::Stopped);
    assert!(matches!(
        after_stop.items().iter().find(|item| matches!(item, Item::Assistant { .. })),
        Some(Item::Assistant { interrupted: true, live: false, text, .. })
            if text == "**The Last Watch at Hollin Point**\n\nFor"
    ));
    assert_eq!(turn_ends(after_stop), [(Some(0.0), true)]);
    assert!(errors(after_stop).is_empty(), "a stop is not an error");
    // The synthetic "[Request interrupted by user]" line is never a user bubble.
    let user_bubbles = after_stop
        .items()
        .iter()
        .filter(|item| matches!(item, Item::User { .. }))
        .count();
    assert_eq!(user_bubbles, 1);

    // The next turn works normally.
    assert_eq!(replay.end.status(), Status::Done);
    assert_eq!(assistant_texts(&replay.end).last().unwrap(), "still here");
}

#[test]
fn tool_deny_interrupt_stops() {
    let replay = replay("tool_deny_interrupt");
    let after_stop = &replay.after_each_result[0];
    assert_eq!(after_stop.status(), Status::Stopped);
    assert_eq!(tools(after_stop), [("Bash".to_string(), ToolState::Denied)]);
    assert_eq!(turn_ends(after_stop).len(), 1);
    assert!(
        turn_ends(after_stop)[0].1,
        "the turn should be marked stopped"
    );
    assert!(errors(after_stop).is_empty());
    assert_eq!(replay.end.status(), Status::Done);
}

#[test]
fn error_not_logged_in() {
    let transcript = replay("error_not_logged_in").end;
    assert_eq!(transcript.status(), Status::Failed);
    assert_eq!(
        errors(&transcript),
        [(
            "Claude stopped".to_string(),
            "Not logged in · Please run /login".to_string()
        )]
    );
    assert!(
        assistant_texts(&transcript).is_empty(),
        "the API error text must not also appear as an assistant bubble"
    );
}

#[test]
fn error_bad_model() {
    let transcript = replay("error_bad_model").end;
    assert_eq!(transcript.status(), Status::Failed);
    let errors = errors(&transcript);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .1
            .starts_with("There's an issue with the selected model")
    );
    assert!(assistant_texts(&transcript).is_empty());
}

#[test]
fn error_bad_resume() {
    let transcript = replay("error_bad_resume").end;
    assert_eq!(transcript.status(), Status::Failed);
    let errors = errors(&transcript);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].1.contains(
            "No conversation found with session ID: 00000000-0000-4000-8000-000000000000"
        )
    );
}

#[test]
fn bad_resume_forgets_the_session_id_so_a_retry_starts_fresh() {
    // Without this, every restart would `--resume` the same missing session and fail again.
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "system", "subtype": "init", "session_id": "gone-session"}),
    );
    assert_eq!(transcript.session_id(), Some("gone-session"));
    for line in common::load("error_bad_resume") {
        if line.out {
            feed(&mut transcript, &mut codec, line.msg);
        }
    }
    assert_eq!(transcript.status(), Status::Failed);
    assert_eq!(transcript.session_id(), None);

    // The fresh session that follows reports its own id as usual.
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "system", "subtype": "init", "session_id": "new-session"}),
    );
    assert_eq!(transcript.session_id(), Some("new-session"));
}

#[test]
fn other_errors_keep_the_session_id() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = replay("error_bad_model").end;
    assert_eq!(transcript.status(), Status::Failed);
    assert!(transcript.session_id().is_some());
    feed(&mut transcript, &mut codec, success_result());
    assert!(transcript.session_id().is_some());
}

#[test]
fn write_allow_session_recording_replays_both_writes() {
    // Recorded live with the OLD "Allow for this session" answer for Write (the session
    // `acceptEdits` mode), after which the CLI ran the second Write without asking. Newjen no
    // longer sends that mode (it also auto-approves `rm` and `mkdir`; see
    // `parse_fixtures::allow_for_session_on_a_write_is_now_plain_allow_once`), so a second
    // Write would now ask again. The recording still checks that the transcript settles an
    // approved call and a call the CLI ran without asking.
    let replay = replay("write_allow_session");
    assert!(replay.saw_pending_approval);
    let approvals = common::load("write_allow_session")
        .iter()
        .filter(|line| line.out && line.msg["type"] == "control_request")
        .count();
    assert_eq!(approvals, 1);
    assert_eq!(
        tools(&replay.end),
        [
            ("Write".to_string(), ToolState::Done),
            ("Write".to_string(), ToolState::Done)
        ]
    );
    assert!(replay.end.pending_approval().is_none());
    assert_eq!(replay.end.status(), Status::Done);
}

#[test]
fn initialize_adds_no_items() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    for line in common::load("initialize") {
        if !line.out && line.msg["type"] == "user" {
            break; // Only the start-up part: hooks and the initialize response.
        }
        if line.out {
            transcript
                .apply(&codec.translate(&SessionEvent::Line(Incoming::from_value(line.msg)), None));
        }
    }
    assert!(transcript.items().is_empty());
    assert_eq!(transcript.status(), Status::Idle);
}

#[test]
fn approval_dropped_when_stdin_closed_while_pending() {
    // The CLI rejects the pending approval itself; the queue must not keep a stale request.
    let transcript = replay("probe_eof_while_permission_pending").end;
    assert!(transcript.pending_approval().is_none());
    assert_eq!(transcript.status(), Status::Done);
    assert!(
        tools(&transcript)
            .iter()
            .all(|(_, state)| *state != ToolState::Waiting)
    );
}

#[test]
fn every_fixture_replays_without_panicking() {
    for name in common::all_fixture_names() {
        let transcript = replay(&name).end;
        // Ids are unique and increasing, so a UI can use them as stable element ids.
        let ids: Vec<u64> = transcript.items().iter().map(Item::id).collect();
        assert!(
            ids.windows(2).all(|pair| pair[0] < pair[1]),
            "{name}: {ids:?}"
        );
    }
}

#[test]
fn no_fixture_leaves_the_thread_working_after_its_last_result() {
    // A bare `init` starts a turn (see `starts_turn`), so one after a result would leave the
    // thread working with nothing running.
    let mut checked = Vec::new();
    for name in common::all_fixture_names() {
        let replay = replay(&name);
        let Some(after_last) = replay.after_each_result.last() else {
            continue;
        };
        assert!(
            !after_last.status().is_busy(),
            "{name}: {:?}",
            after_last.status()
        );
        assert!(!replay.end.status().is_busy(), "{name}");
        checked.push(name);
    }
    assert!(checked.contains(&"resume".to_string()));
    assert!(checked.contains(&"background_task".to_string()));
}

#[test]
fn unexpected_exit_is_an_error_with_stderr() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = replay("text_reply").end;
    transcript.apply(&codec.translate(&SessionEvent::Stderr("something broke".to_string()), None));
    transcript.apply(&codec.translate(
        &SessionEvent::Exited {
            code: Some(1),
            requested: false,
        },
        None,
    ));
    assert_eq!(transcript.status(), Status::Failed);
    let errors = errors(&transcript);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "Claude exited");
    assert!(errors[0].1.contains("code 1"));
    assert!(errors[0].1.contains("something broke"));
}

#[test]
fn exit_after_requested_shutdown_is_not_an_error() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = replay("text_reply").end;
    transcript.apply(&codec.translate(
        &SessionEvent::Exited {
            code: None,
            requested: true,
        },
        None,
    ));
    assert!(errors(&transcript).is_empty());
    assert_eq!(transcript.status(), Status::Done);
}

#[test]
fn requested_exit_mid_turn_ends_as_stopped() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    transcript.push_user("hi");
    transcript.apply(&codec.translate(
        &SessionEvent::Exited {
            code: None,
            requested: true,
        },
        None,
    ));
    assert!(errors(&transcript).is_empty());
    assert_eq!(transcript.status(), Status::Stopped);
}

#[test]
fn crash_after_a_restart_is_still_reported() {
    // Crash, then the app restarts (shutting down the already-dead session), then the new
    // session crashes too. Both crashes must show, and the spinner must stop.
    let crash = SessionEvent::Exited {
        code: Some(1),
        requested: false,
    };
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    transcript.push_user("a");
    transcript.apply(&codec.translate(&crash, None));
    transcript.push_user("again");
    transcript.apply(&codec.translate(&crash, None));
    assert_eq!(errors(&transcript).len(), 2);
    assert_eq!(transcript.status(), Status::Failed);
}

#[test]
fn exit_after_a_failed_turn_adds_no_second_error() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = replay("error_bad_resume").end;
    transcript.apply(&codec.translate(
        &SessionEvent::Exited {
            code: Some(1),
            requested: false,
        },
        None,
    ));
    assert_eq!(errors(&transcript).len(), 1);
}

#[test]
fn a_failed_write_and_a_crash_show_one_error_in_either_order() {
    let crash = SessionEvent::Exited {
        code: Some(1),
        requested: false,
    };
    let write_failed = SessionEvent::WriteFailed("Broken pipe (os error 32)".to_string());
    for (first, second, title) in [
        (&crash, &write_failed, "Claude exited"),
        (&write_failed, &crash, "Could not reach Claude"),
    ] {
        let mut codec = ClaudeEvents::default();
        let mut transcript = Transcript::new();
        transcript.push_user("hi");
        transcript.apply(&codec.translate(first, None));
        transcript.apply(&codec.translate(second, None));
        let errors = errors(&transcript);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, title);
        assert_eq!(transcript.status(), Status::Failed);
    }
}

/// Feeds one raw stdout line to the transcript.
fn feed(transcript: &mut Transcript, codec: &mut ClaudeEvents, line: Value) {
    transcript.apply(&codec.translate(&SessionEvent::Line(Incoming::from_value(line)), None));
}

fn success_result() -> Value {
    serde_json::json!({"type": "result", "subtype": "success", "is_error": false,
                       "terminal_reason": "completed", "duration_ms": 10})
}

#[test]
fn approved_subagent_tool_settles_when_its_result_arrives() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    transcript.push_user("use a subagent");
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "tool_use", "id": "task_1", "name": "Task", "input": {"description": "Write"}}]}}),
    );
    // The subagent's approval request has no parent_tool_use_id, so its card is shown.
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "control_request", "request_id": "r1", "request": {
            "subtype": "can_use_tool", "tool_name": "Write", "tool_use_id": "sub_t",
            "input": {"file_path": "/tmp/a.txt"}, "agent_id": "agent_1"}}),
    );
    transcript.resolve_approval(
        transcript.pending_approval().unwrap().key(),
        ApprovalIntent::AllowOnce,
    );
    // Its result arrives on a subagent line.
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "user", "parent_tool_use_id": "task_1", "message": {
            "role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "sub_t", "content": "ok"}]}}),
    );
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "task_1", "content": "done"}]}}),
    );
    feed(&mut transcript, &mut codec, success_result());

    assert_eq!(transcript.status(), Status::Done);
    assert_eq!(
        tools(&transcript),
        [
            ("Task".to_string(), ToolState::Done),
            ("Write".to_string(), ToolState::Done)
        ]
    );
}

#[test]
fn stop_during_thinking_leaves_the_previous_reply_alone() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    transcript.push_user("one");
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "text", "text": "First reply"}]}}),
    );
    feed(&mut transcript, &mut codec, success_result());
    transcript.push_user("two");
    // Stopped while Claude was still thinking: the aborted line has no text.
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "assistant", "aborted": true, "message": {"id": "m2",
            "content": [{"type": "thinking", "thinking": ""}]}}),
    );
    assert!(matches!(
        transcript.items().iter().find(|item| matches!(item, Item::Assistant { .. })),
        Some(Item::Assistant { interrupted: false, text, .. }) if text == "First reply"
    ));
}

#[test]
fn a_cancelled_approval_is_dropped_and_its_tool_fails() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    transcript.push_user("go");
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "control_request", "request_id": "r1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "t1",
            "input": {"command": "ls"}}}),
    );
    assert_eq!(transcript.status(), Status::Waiting);
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "control_cancel_request", "request_id": "r1"}),
    );
    assert!(transcript.pending_approval().is_none());
    assert_eq!(transcript.status(), Status::Working);
    assert!(matches!(
        &transcript.items()[1],
        Item::Tool { state: ToolState::Failed, output: Some(output), .. }
            if output == "Cancelled by Claude"
    ));
}

#[test]
fn an_interrupt_timeout_fails_the_turn_with_one_error() {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    transcript.push_user("go");
    feed(
        &mut transcript,
        &mut codec,
        serde_json::json!({"type": "assistant", "message": {"id": "m1", "content": [
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "sleep 99"}}]}}),
    );
    transcript.apply(&codec.translate(&SessionEvent::InterruptTimedOut, None));
    // The shutdown that follows must not change the outcome.
    transcript.apply(&codec.translate(
        &SessionEvent::Exited {
            code: None,
            requested: true,
        },
        None,
    ));
    assert_eq!(transcript.status(), Status::Failed);
    assert_eq!(
        tools(&transcript),
        [("Bash".to_string(), ToolState::Failed)]
    );
    let titles: Vec<String> = errors(&transcript)
        .into_iter()
        .map(|(title, _)| title)
        .collect();
    assert_eq!(titles, ["Claude didn't stop, so Newjen ended the session"]);
}

#[test]
fn background_task_then_a_turn_claude_starts_by_itself() {
    let replay = replay("background_task");
    let [first, second] = replay.after_each_result.as_slice() else {
        panic!("two turns: the user's, then claude's own");
    };
    // The user's turn is over while the command still runs.
    assert_eq!(first.status(), Status::Done);
    let running: Vec<&str> = first
        .background_tasks()
        .iter()
        .map(|task| task.description.as_str())
        .collect();
    assert_eq!(running, ["Sleep 6 seconds then echo finished"]);
    assert!(matches!(
        first
            .items()
            .iter()
            .find(|item| matches!(item, Item::Tool { .. })),
        Some(Item::Tool {
            state: ToolState::Done,
            background: true,
            ..
        })
    ));

    // The task finished, and claude replied about it with no message from the user.
    let transcript = &replay.end;
    assert_eq!(second.items(), transcript.items());
    assert_eq!(transcript.status(), Status::Done);
    assert!(transcript.background_tasks().is_empty());
    let users = transcript
        .items()
        .iter()
        .filter(|item| matches!(item, Item::User { .. }))
        .count();
    assert_eq!(users, 1);
    assert_eq!(turn_ends(transcript).len(), 2);
    assert_eq!(
        assistant_texts(transcript),
        [
            "started",
            "The background command finished with exit code 0 and printed \"finished\"."
        ]
    );
    assert!(matches!(
        transcript
            .items()
            .iter()
            .find(|item| matches!(item, Item::Tool { .. })),
        Some(Item::Tool {
            state: ToolState::Done,
            background: false,
            ..
        })
    ));
    // Each turn's own cost, not the running total.
    assert_close(turn_ends(transcript)[1].0, 0.00042816);
}

fn snapshot(
    transcript: &Transcript,
    approval_ids: &std::collections::HashMap<newjen_core::ApprovalKey, String>,
) -> Value {
    let items: Vec<Value> = transcript
        .items()
        .iter()
        .map(|item| {
            let mut value: Value = serde_json::from_str(&item.to_saved().1).unwrap();
            if let Some(object) = value.as_object_mut() {
                object.remove("took");
            }
            value
        })
        .collect();
    serde_json::json!({"items":items,"status":transcript.status(),"dirty":transcript.dirty_from(),
        "session":transcript.session_id(),"model":transcript.model(),
        "pending":transcript.pending_approval().map(|r| &approval_ids[&r.key()]),
        "background":transcript.background_tasks().iter().map(|t| (&t.task_id, &t.description)).collect::<Vec<_>>()})
}

#[test]
fn every_fixture_prefix_preserves_the_original_projection() {
    for name in common::all_fixture_names() {
        replay(&name);
    }
}
