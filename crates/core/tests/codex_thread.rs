use newjen_core::agent::{
    AgentConfig, ApprovalIntent, ConversationState, EffortSelection, GrantLifetime,
};
use newjen_core::providers::{self, Availability, ModelSelection, Provider};
use newjen_core::thread::{Sent, WorksIn};
use newjen_core::{Item, Project, SessionConfig, Status, Store, Thread};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Lab {
    dir: tempfile::TempDir,
    store: Store,
    project: Project,
    config: SessionConfig,
}
impl Lab {
    fn new(fixture: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let project_dir = root.join("project");
        std::fs::create_dir(&project_dir).unwrap();
        let store = Store::open_in(&root.join("state")).unwrap();
        let project = store.open_project(&project_dir).unwrap();
        let mut config = SessionConfig::new(&project_dir);
        config.agent = AgentConfig::cli_default(Provider::Codex);
        config.program = env!("CARGO_BIN_EXE_newjen-fake-codex").into();
        config.interrupt_ack_timeout = Duration::from_millis(150);
        config.env = vec![
            (
                "NEWJEN_FAKE_FIXTURE".into(),
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("tests/fixtures/codex/runtime-{fixture}.jsonl"))
                    .display()
                    .to_string(),
            ),
            (
                "NEWJEN_FAKE_INPUT_LOG".into(),
                root.join("input.jsonl").display().to_string(),
            ),
        ];
        Self {
            dir,
            store,
            project,
            config,
        }
    }
    fn draft(&self) -> Thread {
        Thread::new_draft(self.store.clone(), &self.project, self.config.clone())
    }
    fn input(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("input.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn flush(&self) {
        assert!(self.store.flush(Duration::from_secs(5)));
    }
}
fn until(thread: &mut Thread, condition: impl Fn(&Thread) -> bool) {
    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition(thread) {
        assert!(
            Instant::now() < deadline,
            "timeout: {:?}",
            thread.transcript().items()
        );
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}
fn finish(thread: &mut Thread) {
    until(thread, |thread| !thread.status().is_busy());
}
fn reply(thread: &Thread) -> String {
    thread
        .transcript()
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn send_to_reply_uses_one_user_turn_and_the_shared_presentation() {
    let lab = Lab::new("recorded-reply");
    let mut thread = lab.draft();
    assert_eq!(thread.conversation(), ConversationState::Uncreated);
    thread
        .send("Reply with exactly: Hello Newjen. Do not use tools.")
        .unwrap();
    let unavailable = providers::ProviderStatus {
        provider: Provider::Codex,
        availability: Availability::NotInstalled {
            hint: "test".into(),
        },
    };
    assert!(
        thread
            .set_agent_config(AgentConfig::cli_default(Provider::Codex), &unavailable)
            .is_err()
    );
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:?}",
        thread.transcript().items()
    );
    assert!(reply(&thread).contains("Hello Newjen"));
    assert_eq!(thread.presentation().label, "Codex");
    assert_eq!(thread.transcript().model(), Some("test-model"));
    assert_eq!(
        thread.conversation(),
        ConversationState::Established("codex-thread".into())
    );
    assert_eq!(
        lab.input()
            .iter()
            .filter(|m| m["method"] == "turn/start")
            .count(),
        1
    );
}

#[test]
fn command_allow_deny_and_exact_session_grants_use_the_right_decision() {
    for (fixture, intent) in [
        ("command-accept", ApprovalIntent::AllowOnce),
        ("command-decline", ApprovalIntent::Deny),
        ("command-acceptForSession", ApprovalIntent::AllowForSession),
    ] {
        let lab = Lab::new(fixture);
        let mut thread = lab.draft();
        thread.send("Do the command").unwrap();
        until(&mut thread, |thread| {
            thread.transcript().pending_approval().is_some()
        });
        let request = thread.transcript().pending_approval().unwrap().clone();
        thread.answer(Some(request.key()), intent).unwrap();
        // A stale click must not send a second answer.
        thread.answer(Some(request.key()), intent).unwrap();
        finish(&mut thread);
        assert_eq!(
            thread.status(),
            Status::Done,
            "{:?}",
            thread.transcript().items()
        );
        let grants = thread.grant_views();
        if intent == ApprovalIntent::AllowForSession {
            assert_eq!(grants.len(), 1);
            assert_eq!(grants[0].lifetime, GrantLifetime::Process);
            assert!(grants[0].shown.contains("touch exact.txt"));
            thread.revoke(&grants[0].id);
            assert!(thread.grant_views().is_empty());
            assert!(!thread.has_session());
        } else {
            assert!(grants.is_empty());
        }
        lab.flush();
        assert!(lab.store.grants(thread.id()).unwrap().is_empty());
        assert_eq!(
            lab.input()
                .iter()
                .filter(|m| m.get("result").is_some())
                .count(),
            1
        );
    }
}

#[test]
fn file_edits_are_once_only_even_when_always_allow_is_requested() {
    let lab = Lab::new("edit-accept");
    let mut thread = lab.draft();
    thread.send("Edit once").unwrap();
    until(&mut thread, |thread| {
        thread.transcript().pending_approval().is_some()
    });
    thread
        .answer(None, ApprovalIntent::AllowForSession)
        .unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:?}",
        thread.transcript().items()
    );
    assert!(thread.grant_views().is_empty());
}

#[test]
fn deny_and_stop_sends_cancel_and_interrupt() {
    let lab = Lab::new("command-cancel");
    let mut thread = lab.draft();
    thread.send("Ask first").unwrap();
    until(&mut thread, |thread| {
        thread.transcript().pending_approval().is_some()
    });
    thread.stop().unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Stopped,
        "{:?}",
        thread.transcript().items()
    );
    assert!(
        lab.input()
            .iter()
            .any(|m| m["result"]["decision"] == "cancel")
    );
    assert!(lab.input().iter().any(|m| m["method"] == "turn/interrupt"));
}

#[test]
fn interrupt_waits_for_completion_and_acknowledgement_alone_cannot_hang() {
    for (fixture, expected) in [("interrupt", Status::Stopped), ("no-stop", Status::Failed)] {
        let lab = Lab::new(fixture);
        let mut thread = lab.draft();
        thread.send("Work").unwrap();
        until(&mut thread, |thread| {
            thread.transcript().unconfirmed_message().is_none()
        });
        thread.stop().unwrap();
        finish(&mut thread);
        assert_eq!(
            thread.status(),
            expected,
            "{:?}",
            thread.transcript().items()
        );
        if expected == Status::Failed {
            assert!(!thread.has_session());
        }
    }
}

#[test]
fn relaunch_resumes_the_server_id_in_the_saved_workdir_without_an_automatic_turn() {
    let lab = Lab::new("reply");
    let mut thread = lab.draft();
    thread.send("First").unwrap();
    finish(&mut thread);
    let id = thread.id().to_owned();
    thread.shutdown_for_quit();
    lab.flush();
    let row = lab.store.thread(&id).unwrap().unwrap();
    let mut loaded =
        Thread::load(lab.store.clone(), &lab.project, row, lab.config.clone()).unwrap();
    assert!(!loaded.has_session());
    assert_eq!(loaded.agent_config().unwrap().provider, Provider::Codex);
    loaded.send("Second").unwrap();
    finish(&mut loaded);
    assert_eq!(
        loaded.status(),
        Status::Done,
        "{:?}",
        loaded.transcript().items()
    );
    let inputs = lab.input();
    let resume = inputs
        .iter()
        .find(|m| m["method"] == "thread/resume")
        .unwrap();
    assert_eq!(resume["params"]["threadId"], "codex-thread");
    assert_eq!(
        resume["params"]["cwd"],
        lab.project.path.display().to_string()
    );
    assert_eq!(
        inputs
            .iter()
            .filter(|m| m["method"] == "turn/start")
            .count(),
        2
    );
}

#[test]
fn a_codex_rift_copy_delivers_the_held_message_once_in_the_copy() {
    let lab = Lab::new("reply");
    let mut tools = newjen_core::rift::Tools::find();
    assert!(
        tools.rift.is_absolute() && tools.jj.is_absolute(),
        "This test requires Rift and jj"
    );
    let home = lab.dir.path().join("rift-home");
    std::fs::create_dir(&home).unwrap();
    tools.env = vec![
        ("HOME".into(), home.display().to_string()),
        ("JJ_USER".into(), "Test".into()),
        ("JJ_EMAIL".into(), "test@example.com".into()),
    ];
    let mut thread = lab.draft();
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    let Sent::MakingCopy(job) = thread.send("Only in the copy").unwrap() else {
        panic!("copy job expected");
    };
    assert!(lab.input().is_empty());
    let copy = job.run(&tools, |_| {}).unwrap();
    let cwd = copy.workdir.clone();
    thread.finish_copy(Ok(copy)).unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:?}",
        thread.transcript().items()
    );
    assert_eq!(thread.cwd(), cwd);
    assert!(
        thread
            .finish_copy(Err(newjen_core::rift::CopyError::Failed {
                detail: "late".into(),
                command: "rift".into(),
                output: Vec::new()
            }))
            .is_err()
    );
    let inputs = lab.input();
    let open = inputs
        .iter()
        .find(|m| m["method"] == "thread/start")
        .unwrap();
    assert_eq!(open["params"]["cwd"], cwd.display().to_string());
    assert_eq!(
        inputs
            .iter()
            .filter(|m| m["method"] == "turn/start")
            .count(),
        1
    );
    let removal = thread.archive().unwrap();
    removal.run(&tools).unwrap();
}

#[test]
fn rejected_limit_stops_without_opening_or_starting_a_turn_and_keeps_the_reset() {
    let lab = Lab::new("limit");
    let mut thread = lab.draft();
    thread.send("Do not retry").unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Failed,
        "{:?}",
        thread.transcript().items()
    );
    assert!(!thread.has_session());
    assert_eq!(thread.paused_until(1_700_000_000), Some(1_800_000_000));
    assert!(
        thread
            .send("Bypass the UI")
            .unwrap_err()
            .to_string()
            .contains("paused")
    );

    std::thread::sleep(Duration::from_millis(200));
    let events = thread.events();
    while let Ok(event) = events.try_recv() {
        thread.apply(event);
    }
    assert_eq!(thread.status(), Status::Failed);
    assert!(
        !lab.input()
            .iter()
            .any(|m| m["method"] == "turn/start" || m["method"] == "thread/start")
    );
    lab.flush();
    let row = lab.store.thread(thread.id()).unwrap().unwrap();
    let loaded = Thread::load(lab.store.clone(), &lab.project, row, lab.config.clone()).unwrap();
    assert_eq!(loaded.paused_until(1_700_000_000), Some(1_800_000_000));
    assert!(
        loaded.transcript().items().iter().any(
            |item| matches!(item, Item::Error { detail, .. } if detail.contains("1800000000"))
        )
    );
}

#[test]
fn a_remembered_command_does_not_allow_a_different_command_or_a_stale_callback() {
    let lab = Lab::new("two-commands");
    let mut thread = lab.draft();
    thread.send("Two commands").unwrap();
    until(&mut thread, |thread| {
        thread.transcript().pending_approval().is_some()
    });
    let first = thread.transcript().pending_approval().unwrap().key();
    thread
        .answer(Some(first), ApprovalIntent::AllowForSession)
        .unwrap();
    until(&mut thread, |thread| {
        thread.transcript().pending_approval().is_some()
    });
    let second = thread.transcript().pending_approval().unwrap().key();
    assert_ne!(first, second);
    thread
        .answer(Some(first), ApprovalIntent::AllowForSession)
        .unwrap();
    assert_eq!(
        thread.transcript().pending_approval().unwrap().key(),
        second
    );
    thread.answer(Some(second), ApprovalIntent::Deny).unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:?}",
        thread.transcript().items()
    );
    assert_eq!(thread.grant_views().len(), 1);
    assert!(!thread.grant_views()[0].shown.contains("another"));
}

#[test]
fn stop_during_initialization_cancels_the_only_held_user_message() {
    let lab = Lab::new("initializing");
    let mut thread = lab.draft();
    thread.send("Do not send after stop").unwrap();
    thread.stop().unwrap();
    assert_eq!(thread.status(), Status::Stopped);
    assert!(!thread.has_session());
    assert_eq!(thread.conversation(), ConversationState::Uncreated);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!lab.input().iter().any(|m| m["method"] == "turn/start"));
}

#[test]
fn loading_codex_from_the_apps_default_transport_selects_the_codex_executable() {
    let lab = Lab::new("reply");
    let mut thread = lab.draft();
    thread.send("Save").unwrap();
    finish(&mut thread);
    thread.shutdown_for_quit();
    lab.flush();
    let row = lab.store.thread(thread.id()).unwrap().unwrap();
    let loaded = Thread::load(
        lab.store.clone(),
        &lab.project,
        row,
        SessionConfig::new(&lab.project.path),
    )
    .unwrap();
    assert_eq!(
        loaded.session_config().program,
        newjen_core::find_cli(newjen_core::Cli::Codex)
    );
    assert!(!loaded.has_session());
}

#[test]
#[ignore = "spends Codex credits using the installed, unmodified CLI"]
fn live_codex_thread_reply() {
    let status = providers::detect(Provider::Codex);
    let Availability::Ready { catalog, .. } = &status.availability else {
        panic!("Codex is unavailable: {status:?}");
    };
    assert!(catalog.models.iter().any(|model| model.id == "gpt-6-luna"));
    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().canonicalize().unwrap();
    let store = Store::open_in(&root.join("state")).unwrap();
    let project = store.open_project(&root).unwrap();
    let mut config = AgentConfig::cli_default(Provider::Codex);
    config.model = ModelSelection::Model {
        id: "gpt-6-luna".into(),
    };
    config.effort = EffortSelection::Chosen("low".into());
    let mut thread = Thread::draft_with_agent(store, &project, config, &status).unwrap();
    thread
        .send("Reply with exactly: Hello Newjen. Do not use tools or delegate.")
        .unwrap();
    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(90);
    while thread.status().is_busy() && Instant::now() < deadline {
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    println!(
        "provider={}, model={:?}, conversation={:?}, status={:?}, reply={:?}",
        thread.provider(),
        thread.transcript().model(),
        thread.conversation(),
        thread.status(),
        reply(&thread)
    );
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:?}",
        thread.transcript().items()
    );
    assert!(reply(&thread).contains("Hello Newjen"));
    thread.shutdown_for_quit();
}

#[test]
fn claude_limit_rejection_also_stops_and_persists_reset_without_a_repair() {
    let lab = Lab::new("reply");
    let mut config = SessionConfig::new(&lab.project.path);
    config.program = env!("CARGO_BIN_EXE_newjen-fake-claude").into();
    config.env = vec![(
        "NEWJEN_FAKE_FIXTURE".into(),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/claude/limit-rejected.jsonl")
            .display()
            .to_string(),
    )];
    let mut thread = Thread::new_draft(lab.store.clone(), &lab.project, config.clone());
    thread.send("No retry after rejection").unwrap();
    finish(&mut thread);
    assert_eq!(thread.status(), Status::Failed);
    assert!(!thread.has_session());
    lab.flush();
    let row = lab.store.thread(thread.id()).unwrap().unwrap();
    let loaded = Thread::load(lab.store.clone(), &lab.project, row, config).unwrap();
    assert!(
        loaded.transcript().items().iter().any(
            |item| matches!(item, Item::Error { detail, .. } if detail.contains("1800000000"))
        )
    );
}

#[test]
fn an_active_limit_rejection_cannot_be_replaced_by_a_late_success_or_another_turn() {
    let lab = Lab::new("active-limit");
    let mut thread = lab.draft();
    thread.send("One user turn").unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Failed,
        "{:?}",
        thread.transcript().items()
    );
    std::thread::sleep(Duration::from_millis(200));
    let events = thread.events();
    while let Ok(event) = events.try_recv() {
        thread.apply(event);
    }
    assert_eq!(thread.status(), Status::Failed);
    assert!(!thread.has_session());
    assert_eq!(
        lab.input()
            .iter()
            .filter(|m| m["method"] == "turn/start")
            .count(),
        1
    );
}

#[test]
fn plan_exhaustion_with_available_credits_warns_and_finishes_the_user_turn() {
    let lab = Lab::new("credits");
    let mut thread = lab.draft();
    thread.send("Credits may pay").unwrap();
    finish(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:?}",
        thread.transcript().items()
    );
    assert!(
        matches!(thread.transcript().rate_limit(),Some(newjen_core::agent::RateLimitEvent::Warning(limit)) if limit.explanation.contains("credits"))
    );
    assert_eq!(
        lab.input()
            .iter()
            .filter(|m| m["method"] == "turn/start")
            .count(),
        1
    );
}

#[test]
fn an_explicit_clear_account_refresh_lifts_the_pause_without_starting_a_turn() {
    let lab = Lab::new("limit");
    let mut thread = lab.draft();
    thread.send("One submitted message").unwrap();
    finish(&mut thread);
    assert!(thread.paused_until(1_700_000_000).is_some());
    let before = lab.input().len();
    let status=providers::ProviderStatus { provider:Provider::Codex, availability:Availability::Ready {
        binary:lab.config.program.clone(),version:"0.166.0".into(),account:providers::Account { billing:providers::Billing::Subscription,plan:None,email:None },
        catalog:providers::ModelCatalog { source:providers::CatalogSource::AccountModels,models:vec![] },
        rate_limits:Some(Box::new(serde_json::from_value(serde_json::json!({"rateLimits":{"primary":{"usedPercent":12,"resetsAt":1800000000},"spendControlReached":false}})).unwrap())),warnings:vec![]
    } };
    thread.refresh_limits(&status);
    assert_eq!(thread.paused_until(1_700_000_000), None);
    assert!(!thread.has_session());
    assert_eq!(lab.input().len(), before);
    lab.flush();
    let loaded = Thread::load(
        lab.store.clone(),
        &lab.project,
        lab.store.thread(thread.id()).unwrap().unwrap(),
        lab.config.clone(),
    )
    .unwrap();
    assert_eq!(loaded.paused_until(1_700_000_000), None);
}
