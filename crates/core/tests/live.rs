//! A live test against the real `claude` CLI. It uses your Claude login and spends a few
//! tokens, so it is `#[ignore]`d and never runs in CI. Run it with:
//!
//! ```sh
//! cargo test -p newjen-core --test live -- --ignored
//! ```

mod common;

use std::time::{Duration, Instant};

use newjen_core::protocol::PermissionAnswer;
use newjen_core::session::{Session, SessionConfig, SessionEvent};
use newjen_core::transcript::{Status, Transcript};

#[test]
#[ignore = "uses the real claude CLI and your login"]
fn live_touch_file_with_approval() {
    let project = tempfile::tempdir().unwrap();
    let (session, events) = Session::spawn(SessionConfig::new(project.path())).expect(
        "could not start claude; is it installed (or NEWJEN_CLAUDE set) and are you logged in?",
    );
    let mut transcript = Transcript::with_cwd(project.path());
    let prompt = "Run this exact shell command with the Bash tool: touch live.txt -- then tell me \
                  in one sentence whether it worked.";
    session.send_prompt(prompt).unwrap();
    transcript.push_user(prompt);

    let deadline = Instant::now() + Duration::from_secs(120);
    let mut approvals = 0;
    while !matches!(
        transcript.status(),
        Status::Done | Status::Stopped | Status::Failed
    ) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let event = common::next_event(&events, remaining);
        if let SessionEvent::Stderr(line) = &event {
            eprintln!("claude stderr: {line}");
        }
        transcript.apply(&event);
        if let Some(request) = transcript.pending_approval().cloned() {
            session
                .answer(&request, PermissionAnswer::AllowOnce)
                .unwrap();
            transcript.resolve_approval(&request.request_id, PermissionAnswer::AllowOnce);
            approvals += 1;
        }
    }

    session.shutdown();
    assert_eq!(
        transcript.status(),
        Status::Done,
        "turn did not finish cleanly: {:#?}",
        transcript.items()
    );
    assert!(approvals >= 1, "touch should have needed an approval");
    assert!(
        project.path().join("live.txt").exists(),
        "live.txt was not created"
    );
}

/// Feeds a live thread its events until the turn is over.
fn finish_live_turn(thread: &mut newjen_core::thread::Thread) {
    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(120);
    while thread.status().is_busy() {
        assert!(Instant::now() < deadline, "the live turn did not finish");
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[test]
#[ignore = "uses the real claude CLI and your login"]
fn live_thread_starts_with_session_id_then_resumes_from_its_folder() {
    use newjen_core::store::Store;
    use newjen_core::thread::{IDLE_TIMEOUT, Thread};

    let home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(project_dir.path()).unwrap();
    let mut base = SessionConfig::new(project_dir.path());
    base.extra_args = vec![
        "--model".into(),
        "haiku".into(),
        "--strict-mcp-config".into(),
    ];

    let mut thread = Thread::new_draft(store.clone(), &project, base.clone());
    let session_id = thread.session_id().to_string();
    thread
        .send("Remember the word PELICAN. Reply with just: ok")
        .unwrap();
    finish_live_turn(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:#?}",
        thread.transcript().items()
    );
    assert!(
        thread.session_started(),
        "init marks the conversation as started"
    );
    assert_eq!(
        thread.session_id(),
        session_id,
        "claude kept the id Newjen chose"
    );

    // Let claude go, as the idle timeout would, so the next message starts a new process that
    // must `--resume` the conversation from the same folder.
    assert!(thread.idle_check(Instant::now() + IDLE_TIMEOUT + Duration::from_secs(1)));
    assert!(!thread.has_session());
    thread
        .send("Which word did I ask you to remember? Reply with just the word.")
        .unwrap();
    finish_live_turn(&mut thread);
    assert_eq!(
        thread.status(),
        Status::Done,
        "{:#?}",
        thread.transcript().items()
    );
    assert_eq!(
        thread.session_id(),
        session_id,
        "resumed, not started fresh"
    );
    let last_reply = thread
        .transcript()
        .items()
        .iter()
        .rev()
        .find_map(|item| match item {
            newjen_core::transcript::Item::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        last_reply.to_uppercase().contains("PELICAN"),
        "the resumed conversation remembers: {last_reply:?}"
    );
}
