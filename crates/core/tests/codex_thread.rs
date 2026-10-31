use newjen_core::agent::{
    AgentConfig, ApprovalIntent, ConversationState, EffortSelection, GrantLifetime, NoteDelivery,
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
    fn rift_tools(&self) -> newjen_core::rift::Tools {
        let mut tools = newjen_core::rift::Tools::find();
        assert!(
            tools.rift.is_absolute() && tools.jj.is_absolute(),
            "This test requires Rift and jj"
        );
        let home = self.dir.path().join("rift-home");
        std::fs::create_dir(&home).unwrap();
        tools.env = vec![
            ("HOME".into(), home.display().to_string()),
            ("JJ_USER".into(), "Test".into()),
            ("JJ_EMAIL".into(), "test@example.com".into()),
        ];
        tools
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
        // Codex's decision carries no text, so a note comes back for the composer.
        let delivered = thread
            .answer_with_note(Some(request.key()), intent, Some(" Not in this folder. "))
            .unwrap();
        assert_eq!(
            delivered,
            if intent.is_deny() {
                NoteDelivery::Kept("Not in this folder.".into())
            } else {
                NoteDelivery::Delivered
            }
        );
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
        assert!(
            !lab.input()
                .iter()
                .any(|m| m.to_string().contains("Not in this folder"))
        );
        let opened = lab
            .input()
            .into_iter()
            .find(|m| m["method"] == "thread/start")
            .unwrap();
        assert_eq!(
            opened["params"]["developerInstructions"],
            thread.landing_mode().instructions(),
            "Newjen's instructions reach Codex as developer instructions: {opened}"
        );
        assert!(!opened.to_string().contains("--append-system-prompt"));
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
    let proposal = thread
        .pending_edit()
        .expect("a file change proposes an edit");
    assert!(matches!(
        proposal.preview(),
        newjen_core::edits::EditPreview::Unavailable(_)
    ));
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
    let tools = lab.rift_tools();
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
fn a_refused_sent_checkpoint_never_delivers_the_held_message_to_codex() {
    let lab = Lab::new("reply");
    let tools = lab.rift_tools();
    let mut thread = lab.draft();
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    let Sent::MakingCopy(job) = thread
        .send("Do not deliver without the checkpoint")
        .unwrap()
    else {
        panic!("copy job expected");
    };
    let copy = job.run(&tools, |_| {}).unwrap();
    let database = rusqlite::Connection::open(lab.store.path()).unwrap();
    database
        .execute_batch("CREATE TRIGGER refuse_sent BEFORE INSERT ON pending_copies WHEN NEW.state = 'sent' BEGIN SELECT RAISE(ABORT, 'disk full'); END;")
        .unwrap();
    let failed = thread.finish_copy(Ok(copy.clone())).unwrap_err();
    assert_eq!(failed.message, "Do not deliver without the checkpoint");
    thread.shutdown_for_quit();
    assert!(thread.transcript().items().is_empty());
    assert!(!lab.input().iter().any(|m| m["method"] == "turn/start"));
    lab.flush();
    assert!(lab.store.thread(thread.id()).unwrap().is_none());
    let pending = lab.store.pending_copies(lab.project.id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].state,
        newjen_core::store::PendingState::MakingCopy
    );
    assert_eq!(
        Thread::recover_pending_copy(&lab.store, &lab.project, &pending[0], &tools).unwrap(),
        newjen_core::thread::CopyRecovery::Draft
    );
    assert!(!copy.path.exists());
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
#[ignore = "spends Codex credits using the installed, unmodified CLI"]
fn live_codex_rift_copy_creates_resumes_and_removes_in_the_copy() {
    use newjen_core::agent::ApprovalKind;
    use newjen_core::rift::{Progress, Tools};
    use std::process::Command;

    fn jj(tools: &Tools, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new(&tools.jj)
            .args(args)
            .current_dir(cwd)
            .envs(tools.env.iter().map(|(name, value)| (name, value)))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "jj {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn agent_cwd() -> PathBuf {
        let output = Command::new("/bin/ps")
            .args(["-axo", "ppid=,pid=,comm="])
            .output()
            .unwrap();
        assert!(output.status.success());
        let processes: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| {
                let mut fields = line.split_whitespace();
                let parent = fields.next().unwrap().parse::<u32>().unwrap();
                let pid = fields.next().unwrap().parse::<u32>().unwrap();
                let program = fields.collect::<Vec<_>>().join(" ");
                (parent, pid, program)
            })
            .collect();
        let mut descendants = vec![std::process::id()];
        let mut at = 0;
        while at < descendants.len() {
            let parent = descendants[at];
            descendants.extend(processes.iter().filter(|p| p.0 == parent).map(|p| p.1));
            at += 1;
        }
        let pids: Vec<_> = processes
            .iter()
            .filter(|p| {
                descendants.contains(&p.1)
                    && Path::new(&p.2)
                        .file_name()
                        .is_some_and(|name| name == "codex")
            })
            .map(|p| p.1)
            .collect();
        assert_eq!(pids.len(), 1, "expected one Codex child: {pids:?}");
        let output = Command::new("/usr/sbin/lsof")
            .args(["-a", "-p", &pids[0].to_string(), "-d", "cwd", "-Fn"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8(output.stdout).unwrap();
        let cwd = text
            .lines()
            .find_map(|line| line.strip_prefix('n'))
            .unwrap();
        PathBuf::from(cwd).canonicalize().unwrap()
    }

    fn complete(thread: &mut Thread, allow_commands: bool, cwd: &Path) -> usize {
        let events = thread.events();
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut approvals = 0;
        while thread.status().is_busy() && Instant::now() < deadline {
            if let Some(request) = thread.transcript().pending_approval().cloned() {
                assert!(allow_commands, "the resume reply must not use tools");
                assert!(matches!(request.view().kind, ApprovalKind::Command { .. }));
                assert_eq!(agent_cwd(), cwd);
                println!(
                    "approval=AllowOnce, cwd={}, command={}",
                    cwd.display(),
                    request.view().tool.summary
                );
                thread
                    .answer(Some(request.key()), ApprovalIntent::AllowOnce)
                    .unwrap();
                approvals += 1;
            }
            match events.try_recv() {
                Ok(event) => thread.apply(event),
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        println!(
            "status={:?}, conversation={:?}, reply={:?}, limit={:?}",
            thread.status(),
            thread.conversation(),
            reply(thread),
            thread.limit_event()
        );
        assert_eq!(
            thread.status(),
            Status::Done,
            "{:?}",
            thread.transcript().items()
        );
        assert_eq!(agent_cwd(), cwd);
        approvals
    }

    let mut tools = Tools::find();
    for (name, program) in [("rift", &tools.rift), ("jj", &tools.jj)] {
        if !program.is_absolute() || !program.is_file() {
            eprintln!("skipped: `{name}` is not installed");
            return;
        }
    }
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let root = dir.path().canonicalize().unwrap();
    let home = root.join("rift-home");
    let project_dir = root.join("project");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&project_dir).unwrap();
    tools.env = vec![
        ("HOME".into(), home.display().to_string()),
        ("JJ_USER".into(), "Test".into()),
        ("JJ_EMAIL".into(), "test@example.com".into()),
    ];
    jj(&tools, &project_dir, &["git", "init", "--colocate"]);
    std::fs::write(project_dir.join("base.txt"), "base\n").unwrap();
    jj(&tools, &project_dir, &["commit", "-m", "base"]);
    std::fs::write(project_dir.join("base.txt"), "uncommitted edit\n").unwrap();
    std::fs::write(
        project_dir.join(".rift.toml"),
        "version = 1\n[[hooks.postcreate]]\nrun = \"echo live-codex-copy-ready\"\n",
    )
    .unwrap();

    let status = providers::detect(Provider::Codex);
    let Availability::Ready {
        catalog,
        account,
        warnings,
        ..
    } = &status.availability
    else {
        panic!("Codex is unavailable: {status:?}");
    };
    println!(
        "Codex billing={:?}, plan={:?}, warnings={warnings:?}",
        account.billing, account.plan
    );
    assert!(catalog.models.iter().any(|model| model.id == "gpt-6-luna"));
    let store = Store::open_in(&root.join("state")).unwrap();
    let project = store.open_project(&project_dir).unwrap();
    let mut config = AgentConfig::cli_default(Provider::Codex);
    config.model = ModelSelection::Model {
        id: "gpt-6-luna".into(),
    };
    config.effort = EffortSelection::Chosen("low".into());
    let mut thread = Thread::draft_with_agent(store.clone(), &project, config, &status).unwrap();
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    let prompt = "Create a file named hello.txt containing exactly hi, using a shell command. Do not delegate.";
    let Sent::MakingCopy(job) = thread.send(prompt).unwrap() else {
        panic!("copy job expected");
    };
    assert!(!thread.has_session());
    assert!(thread.transcript().items().is_empty());
    assert_eq!(store.pending_copies(project.id).unwrap().len(), 1);
    let mut hook_seen = false;
    let made = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                job.run(&tools, |progress| {
                    println!("copy progress: {progress:?}");
                    hook_seen |= progress == Progress::Line("live-codex-copy-ready".into());
                    thread.note_copy_progress(progress);
                })
            })
            .join()
            .unwrap()
    });
    let database = rusqlite::Connection::open(store.path()).unwrap();
    database
        .execute_batch("CREATE TRIGGER hold_first_checkpoint BEFORE INSERT ON threads BEGIN SELECT RAISE(ABORT, 'test holds the first thread checkpoint'); END;")
        .unwrap();
    thread.finish_copy(made).unwrap();
    assert!(store.flush(Duration::from_secs(5)));
    let pending = store.pending_copies(project.id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, newjen_core::store::PendingState::Sent);
    assert_eq!(pending[0].path, thread.copy().unwrap().path);
    assert_eq!(
        pending[0].rift_id.as_deref(),
        Some(thread.copy().unwrap().rift_id.as_str())
    );
    assert!(store.thread(thread.id()).unwrap().is_none());
    println!(
        "delivery: pending_copies=1, state=Sent, Rift id/path matched, thread checkpoint held by test trigger"
    );
    database
        .execute_batch("DROP TRIGGER hold_first_checkpoint;")
        .unwrap();
    assert!(hook_seen);
    let copy = thread.copy().unwrap().clone();
    assert_eq!(thread.cwd(), copy.workdir);
    assert_eq!(
        std::fs::read_to_string(copy.path.join("base.txt")).unwrap(),
        "uncommitted edit\n"
    );
    assert!(
        complete(&mut thread, true, &copy.workdir) > 0,
        "expected a shell approval"
    );
    let users: Vec<_> = thread
        .transcript()
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::User {
                text, unconfirmed, ..
            } => {
                assert!(!unconfirmed);
                Some(text.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(users, [prompt]);
    assert_eq!(
        std::fs::read(copy.workdir.join("hello.txt")).unwrap(),
        b"hi"
    );
    assert!(!project.path.join("hello.txt").exists());
    assert!(store.flush(Duration::from_secs(5)));
    let id = thread.id().to_owned();
    let row = store.thread(&id).unwrap().unwrap();
    assert_eq!(row.workdir, copy.workdir);
    assert_eq!(row.copy.as_ref(), Some(&copy));
    let base = copy.base.as_deref().expect("jj base");
    assert_eq!(
        jj(
            &tools,
            &project.path,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                base,
                "-T",
                "commit_id"
            ]
        ),
        base
    );
    assert_eq!(
        jj(
            &tools,
            &project.path,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                "@-",
                "-T",
                "commit_id"
            ]
        ),
        base
    );
    assert!(
        jj(
            &tools,
            &project.path,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                "divergent()",
                "-T",
                "commit_id"
            ]
        )
        .is_empty()
    );
    assert!(store.pending_copies(project.id).unwrap().is_empty());
    println!(
        "checks: user_items=1, hello.txt=hi only in copy, process_cwd={}, saved_workdir/copy=matched, main_base={base}, divergent=0, pending_copies=0",
        copy.workdir.display()
    );

    let conversation = thread.conversation();
    assert!(matches!(conversation, ConversationState::Established(_)));
    thread.shutdown_for_quit();
    assert!(store.flush(Duration::from_secs(5)));
    drop(thread);
    let row = store.thread(&id).unwrap().unwrap();
    let mut loaded = Thread::load(
        store.clone(),
        &project,
        row,
        SessionConfig::new(&project.path),
    )
    .unwrap();
    assert!(!loaded.has_session());
    assert_eq!(loaded.cwd(), copy.workdir);
    assert_eq!(loaded.conversation(), conversation);
    let old_items = loaded.transcript().items().len();
    loaded
        .send("What file did you create, and where? Do not use tools.")
        .unwrap();
    assert_eq!(complete(&mut loaded, false, &copy.workdir), 0);
    assert_eq!(loaded.conversation(), conversation);
    let resumed = &loaded.transcript().items()[old_items..];
    let answer = resumed
        .iter()
        .filter_map(|item| match item {
            Item::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(answer.contains("hello.txt"), "{answer}");
    assert!(
        !resumed
            .iter()
            .any(|item| matches!(item, Item::Tool { .. } | Item::FreshStart { .. }))
    );
    let costs: Vec<_> = loaded
        .transcript()
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::TurnEnd { cost_usd, .. } => Some(*cost_usd),
            _ => None,
        })
        .collect();
    println!(
        "resume: same conversation, cwd={}, reply={answer:?}, turn_costs_usd={costs:?}",
        loaded.cwd().display()
    );
    let removal = loaded.archive().expect("copy removal");
    let trash = copy.path.parent().unwrap().join(".trash").join(format!(
        "{}-{}",
        copy.rift_id,
        copy.path.file_name().unwrap().to_str().unwrap()
    ));
    std::thread::spawn(move || removal.run(&tools))
        .join()
        .unwrap()
        .unwrap();
    assert!(!copy.path.exists());
    assert!(!trash.exists());
    assert!(store.flush(Duration::from_secs(5)));
    let db = rusqlite::Connection::open(store.path()).unwrap();
    let removed_at: Option<i64> = db
        .query_row(
            "SELECT removed_at FROM copies WHERE thread_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(removed_at.is_some());
    assert!(store.thread(&id).unwrap().unwrap().archived_at.is_some());
    assert!(store.pending_copies(project.id).unwrap().is_empty());
    println!("archive: copy gone, trash entry gone, removed_at={removed_at:?}");
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
