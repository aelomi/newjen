//! Step labels, work blocks, the activity line and thread groups, over recorded Claude sessions
//! (the Codex recordings are covered in `providers/codex/tests.rs`).

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use newjen_core::ApprovalIntent;
use newjen_core::agent::{AgentConfig, ApprovalIntent as Intent};
use newjen_core::protocol::Incoming;
use newjen_core::providers::Provider;
use newjen_core::providers::claude::events::ClaudeEvents;
use newjen_core::session::SessionEvent;
use newjen_core::steps::SidebarGroup;
use newjen_core::steps::{WorkState, activity_line, work_blocks};
use newjen_core::transcript::{Status, ToolState, Transcript};
use newjen_core::{SessionConfig, Store, Thread};
use serde_json::Value;

/// The transcript after every line of the fixture.
fn replay(name: &str) -> Vec<Transcript> {
    let mut codec = ClaudeEvents::default();
    let mut transcript = Transcript::new();
    let mut states = Vec::new();
    for line in common::load(name) {
        if line.out {
            let batch = codec.translate(
                &SessionEvent::Line(Incoming::from_value(line.msg.clone())),
                None,
            );
            transcript.apply(&batch);
        } else {
            match line.msg["type"].as_str() {
                Some("user") => {
                    transcript.push_user(line.msg["message"]["content"].as_str().unwrap_or(""))
                }
                Some("control_response") => {
                    let key = transcript.pending_approval().expect("pending").key();
                    let answer = answer_from(&line.msg["response"]["response"]);
                    codec.answer(key, answer).expect("live fixture approval");
                    transcript.resolve_approval(key, answer);
                }
                _ => {}
            }
        }
        states.push(transcript.clone());
    }
    states
}

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

fn waiting(states: &[Transcript]) -> &Transcript {
    states
        .iter()
        .find(|t| t.status() == Status::Waiting)
        .expect("the fixture asks for an approval")
}

fn minutes(secs: u64) -> String {
    format!("{}m {}s", secs / 60, secs % 60)
}

#[test]
fn an_allowed_command_reads_by_its_description_while_it_waits_and_after() {
    let states = replay("tool_allow");
    let asking = waiting(&states);
    assert_eq!(
        activity_line(asking).as_deref(),
        Some("Create empty file newjen_probe.txt via touch?")
    );
    let block = &work_blocks(asking, Some(Duration::from_secs(3)))[0];
    assert_eq!(block.state, WorkState::WaitingForYou);
    assert_eq!(block.steps[0].state, ToolState::Waiting);
    assert_eq!(
        block.steps[0].label.headline,
        "Create empty file newjen_probe.txt via touch"
    );
    assert_eq!(
        block.steps[0].label.detail.as_deref(),
        Some("touch newjen_probe.txt")
    );

    let end = states.last().unwrap();
    let blocks = work_blocks(end, None);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].state, WorkState::Done);
    assert_eq!(blocks[0].step_count, 1);
    assert_eq!(blocks[0].steps[0].state, ToolState::Done);
    assert!(blocks[0].elapsed.is_some());
    let summary = blocks[0].summary(minutes).unwrap();
    assert!(
        summary.starts_with("Worked for ") && summary.ends_with(" · 1 step"),
        "{summary}"
    );
    assert_eq!(activity_line(end).as_deref(), Some("Answered"));
}

#[test]
fn a_denied_command_is_not_described_as_done() {
    let states = replay("tool_deny");
    let end = states.last().unwrap();
    let block = &work_blocks(end, None)[0];
    assert_eq!(block.steps[0].state, ToolState::Denied);
    assert_eq!(
        block.steps[0].label.headline,
        "Run touch command exactly as given"
    );
    assert_eq!(block.state, WorkState::Done);
}

#[test]
fn two_new_files_in_a_row_read_as_one_step() {
    let states = replay("write_allow_session");
    assert_eq!(
        activity_line(waiting(&states)).as_deref(),
        Some("Create a.txt?")
    );
    let block = &work_blocks(states.last().unwrap(), None)[0];
    assert_eq!(block.step_count, 2);
    assert_eq!(block.steps.len(), 1);
    assert_eq!(block.steps[0].label.headline, "Created 2 files");
    assert_eq!(block.steps[0].label.detail.as_deref(), Some("a.txt, b.txt"));
}

#[test]
fn a_background_turn_claude_starts_is_its_own_block() {
    let states = replay("background_task");
    let blocks = work_blocks(states.last().unwrap(), None);
    assert_eq!(blocks.len(), 2);
    assert_eq!(
        blocks[0].steps[0].label.headline,
        "Sleep 6 seconds then echo finished"
    );
    assert!(blocks[1].steps.is_empty());
    assert!(blocks.iter().all(|block| block.state == WorkState::Done));
}

#[test]
fn replies_and_read_only_commands() {
    let states = replay("text_reply");
    let end = states.last().unwrap();
    let blocks = work_blocks(end, None);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].steps.is_empty() && blocks[0].tools.is_none());
    assert_eq!(activity_line(end).as_deref(), Some("Answered"));

    let states = replay("tool_auto_allowed_readonly");
    let block = &work_blocks(states.last().unwrap(), None)[0];
    assert_eq!(
        block.steps[0].label.headline,
        "List all entries in current directory"
    );
    assert_eq!(block.steps[0].label.detail.as_deref(), Some("ls -la"));

    let states = replay("multi_turn");
    assert_eq!(work_blocks(states.last().unwrap(), None).len(), 2);
}

#[test]
fn a_stopped_turn_is_stopped_and_the_next_one_is_not() {
    let states = replay("interrupt");
    let blocks = work_blocks(states.last().unwrap(), None);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].state, WorkState::Stopped);
    assert_eq!(blocks[1].state, WorkState::Done);
    assert_eq!(
        activity_line(states.last().unwrap()).as_deref(),
        Some("Answered")
    );
}

struct Lab {
    _dir: tempfile::TempDir,
    store: Store,
    project: newjen_core::Project,
    config: SessionConfig,
}
impl Lab {
    fn codex(fixture: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let project_dir = root.join("project");
        std::fs::create_dir(&project_dir).unwrap();
        let store = Store::open_in(&root.join("state")).unwrap();
        let project = store.open_project(&project_dir).unwrap();
        let mut config = SessionConfig::new(&project_dir);
        config.agent = AgentConfig::cli_default(Provider::Codex);
        config.program = env!("CARGO_BIN_EXE_newjen-fake-codex").into();
        config.env = vec![(
            "NEWJEN_FAKE_FIXTURE".into(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/codex/runtime-{fixture}.jsonl"))
                .display()
                .to_string(),
        )];
        Self {
            _dir: dir,
            store,
            project,
            config,
        }
    }
    fn draft(&self) -> Thread {
        Thread::new_draft(self.store.clone(), &self.project, self.config.clone())
    }
}
fn until(thread: &mut Thread, condition: impl Fn(&Thread) -> bool) {
    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition(thread) {
        assert!(Instant::now() < deadline, "timeout");
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

const NOW: i64 = 1_700_000_000;

#[test]
fn a_codex_thread_moves_from_needs_you_to_done() {
    let lab = Lab::codex("command-accept");
    let mut thread = lab.draft();
    assert_eq!(thread.sidebar_group(NOW), SidebarGroup::Done);
    assert_eq!(thread.activity_line(NOW), None);
    thread.send("Do the command").unwrap();
    until(&mut thread, |thread| {
        thread.transcript().pending_approval().is_some()
    });
    assert_eq!(thread.sidebar_group(NOW), SidebarGroup::NeedsYou);
    assert_eq!(thread.activity_line(NOW).as_deref(), Some("Run touch?"));
    let blocks = thread.work_blocks();
    assert_eq!(blocks[0].state, WorkState::WaitingForYou);
    assert_eq!(blocks[0].steps[0].label.headline, "Run touch exact.txt");

    let key = thread.transcript().pending_approval().unwrap().key();
    thread.answer(Some(key), Intent::AllowOnce).unwrap();
    until(&mut thread, |thread| !thread.status().is_busy());
    assert_eq!(thread.sidebar_group(NOW), SidebarGroup::Done);
    assert_eq!(thread.activity_line(NOW).as_deref(), Some("Answered"));
    let blocks = thread.work_blocks();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].state, WorkState::Done);
}

#[test]
fn a_codex_file_change_approval_without_a_preview_still_asks_plainly() {
    let lab = Lab::codex("edit-accept");
    let mut thread = lab.draft();
    thread.send("Edit a file").unwrap();
    until(&mut thread, |thread| {
        thread.transcript().pending_approval().is_some()
    });
    assert_eq!(thread.activity_line(NOW).as_deref(), Some("Edit files?"));
}

#[test]
fn a_refused_thread_needs_you_until_the_limit_resets() {
    let lab = Lab::codex("limit");
    let mut thread = lab.draft();
    thread.send("Do not retry").unwrap();
    until(&mut thread, |thread| thread.status() == Status::Failed);
    assert_eq!(thread.sidebar_group(NOW), SidebarGroup::NeedsYou);
    let line = thread.activity_line(NOW).unwrap();
    assert!(line.starts_with("Usage limit reached · resets "), "{line}");
    assert_eq!(
        thread.activity_line(1_900_000_000).as_deref(),
        Some("Failed")
    );
}
