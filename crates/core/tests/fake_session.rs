//! Runs real `Session`s against stand-in processes: the `newjen-fake-claude` test binary
//! (which replays a fixture) and a tiny shell script (for shutdown).

mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use newjen_core::protocol::{Incoming, PermissionAnswer};
use newjen_core::session::{Session, SessionConfig, SessionEvent};
use newjen_core::transcript::{Item, Status, ToolState, Transcript};

const TIMEOUT: Duration = Duration::from_secs(10);

fn fake_config(fixture: &str) -> SessionConfig {
    let mut config = SessionConfig::new(env!("CARGO_MANIFEST_DIR"));
    config.program = env!("CARGO_BIN_EXE_newjen-fake-claude").into();
    config.env.push((
        "NEWJEN_FAKE_FIXTURE".to_string(),
        common::fixture_path(fixture).display().to_string(),
    ));
    config
}

/// True if a process with this pid exists (a zombie counts).
fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn tool_allow_end_to_end_through_the_fake_cli() {
    let (session, events) = Session::spawn(fake_config("tool_allow")).expect("spawn fake claude");
    let mut transcript = Transcript::new();
    session.send_prompt("Run this exact shell command").unwrap();
    transcript.push_user("Run this exact shell command");

    // Read until the approval request.
    loop {
        let event = common::next_event(&events, TIMEOUT);
        transcript.apply(&event);
        if common::is_permission_request(&event) {
            break;
        }
        assert!(
            !matches!(event, SessionEvent::Exited { .. }),
            "exited early: {event:?}"
        );
    }
    assert_eq!(transcript.status(), Status::Waiting);
    let request = transcript
        .pending_approval()
        .cloned()
        .expect("pending approval");
    session
        .answer(&request, PermissionAnswer::AllowOnce)
        .unwrap();
    transcript.resolve_approval(&request.request_id, PermissionAnswer::AllowOnce);

    // Then the result, then a clean exit (the fake exits 0 once the fixture is replayed).
    let mut saw_result = false;
    let mut stderr = Vec::new();
    let code = loop {
        let event = common::next_event(&events, TIMEOUT);
        transcript.apply(&event);
        match event {
            SessionEvent::Line(Incoming::Result(_)) => saw_result = true,
            SessionEvent::Stderr(line) => stderr.push(line),
            SessionEvent::Exited { code, requested } => {
                assert!(!requested, "the fake exited on its own");
                break code;
            }
            _ => {}
        }
    };
    assert!(saw_result);
    assert_eq!(code, Some(0), "fake claude stderr: {stderr:?}");
    assert!(transcript.items().iter().any(|item| matches!(
        item,
        Item::Tool {
            state: ToolState::Done,
            ..
        }
    )));
    assert!(
        transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::TurnEnd { .. }))
    );
    assert!(wait_until_dead(session.pid(), Duration::from_secs(2)));
}

#[test]
fn fake_cli_rejects_a_wrong_answer() {
    // Answering Deny where the fixture expects allow must make the fake exit with code 3,
    // proving the fake really checks what the session writes.
    let (session, events) = Session::spawn(fake_config("tool_allow")).unwrap();
    session.send_prompt("go").unwrap();
    let request = loop {
        if let SessionEvent::Line(Incoming::PermissionRequest(request)) =
            common::next_event(&events, TIMEOUT)
        {
            break request;
        }
    };
    session.answer(&request, PermissionAnswer::Deny).unwrap();
    let code = loop {
        if let SessionEvent::Exited { code, .. } = common::next_event(&events, TIMEOUT) {
            break code;
        }
    };
    assert_eq!(code, Some(3));
}

#[test]
fn writes_after_shutdown_fail_cleanly() {
    let (session, _events) = Session::spawn(fake_config("text_reply")).unwrap();
    session.shutdown();
    session.shutdown(); // Idempotent.
    assert!(session.send_prompt("hello").is_err());
    assert!(session.interrupt().is_err());
}

#[test]
fn spawn_error_for_missing_program() {
    let mut config = SessionConfig::new(env!("CARGO_MANIFEST_DIR"));
    config.program = "/nonexistent/claude".into();
    assert!(Session::spawn(config).is_err());
}

#[test]
fn shutdown_kills_the_process_group_within_200ms() {
    // A stand-in that ignores SIGTERM (so shutdown must escalate to SIGKILL) and starts a
    // grandchild in its process group (so the whole group must die, not just the child).
    // Ignoring a signal is inherited, so the grandchild `sleep` ignores SIGTERM too.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("stubborn-claude");
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(file, "trap '' TERM").unwrap();
    writeln!(file, "sleep 60 &").unwrap();
    writeln!(file, r#"echo '{{"type":"system","subtype":"ready"}}'"#).unwrap();
    writeln!(file, "wait").unwrap();
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = SessionConfig::new(dir.path());
    config.program = script;
    let (session, events) = Session::spawn(config).unwrap();
    // The script prints its line after starting `sleep`, so the grandchild exists now.
    match common::next_event(&events, TIMEOUT) {
        SessionEvent::Line(Incoming::System(message)) => assert_eq!(message.subtype, "ready"),
        other => panic!("unexpected event {other:?}"),
    }
    let grandchild_pid = find_child_of(session.pid()).expect("the script's sleep");
    let pid = session.pid();

    let started = Instant::now();
    session.shutdown();
    let took = started.elapsed();
    assert!(took < Duration::from_millis(200), "shutdown took {took:?}");

    assert!(
        wait_until_dead(grandchild_pid, Duration::from_secs(2)),
        "grandchild survived"
    );
    // The child itself is reaped by the session's reader thread.
    let (code, requested) = loop {
        if let SessionEvent::Exited { code, requested } = common::next_event(&events, TIMEOUT) {
            break (code, requested);
        }
    };
    assert_eq!(code, None, "killed by a signal, so no exit code");
    assert!(requested, "we asked for this exit, so it must say so");
    assert!(wait_until_dead(pid, Duration::from_secs(2)));

    // A second call returns at once.
    let started = Instant::now();
    session.shutdown();
    assert!(started.elapsed() < Duration::from_millis(20));
}

/// The pid of a child of `parent`, using `pgrep -P`.
fn find_child_of(parent: u32) -> Option<u32> {
    let output = std::process::Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}
