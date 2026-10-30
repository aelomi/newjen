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
fn child_identifies_newjen_and_drops_nested_session_vars() {
    // A stand-in that reports what it sees on stderr. When the tests run inside a Claude Code
    // terminal, `CLAUDECODE` is set in this process, so this also checks it is stripped.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("env-claude");
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(
        file,
        r#"echo "client=$CLAUDE_AGENT_SDK_CLIENT_APP nested=${{CLAUDECODE-unset}}" >&2"#
    )
    .unwrap();
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = SessionConfig::new(dir.path());
    config.program = script;
    let (_session, events) = Session::spawn(config).unwrap();
    let expected = format!("client=newjen/{} nested=unset", env!("CARGO_PKG_VERSION"));
    loop {
        match common::next_event(&events, TIMEOUT) {
            SessionEvent::Stderr(line) => {
                assert_eq!(line.trim_end(), expected);
                break;
            }
            SessionEvent::Exited { .. } => panic!("exited before reporting its environment"),
            _ => {}
        }
    }
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
    let grandchild_pid = find_sleep_child_of(session.pid()).expect("the script's sleep");
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

#[test]
fn shutdown_is_quick_even_when_claude_never_reads_stdin() {
    // A stand-in that never reads stdin. A prompt far bigger than the pipe buffer (64 KB on
    // macOS) fills the pipe, so writing it blocks until the process dies. That must happen on
    // the writer thread: neither `send_prompt` nor `shutdown` may wait for it.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("deaf-claude");
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(file, r#"echo '{{"type":"system","subtype":"ready"}}'"#).unwrap();
    writeln!(file, "exec sleep 60").unwrap();
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = SessionConfig::new(dir.path());
    config.program = script;
    let (session, events) = Session::spawn(config).unwrap();
    assert!(matches!(
        common::next_event(&events, TIMEOUT),
        SessionEvent::Line(Incoming::System(_))
    ));

    // 1 MB. Before the writer thread existed, this call blocked forever. The limit is loose
    // because encoding 1 MB of JSON in a debug build alone takes a few tens of milliseconds.
    let huge_prompt = "x".repeat(1024 * 1024);
    let started = Instant::now();
    session.send_prompt(&huge_prompt).unwrap();
    session.send_prompt("and another").unwrap();
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(500),
        "send_prompt took {took:?}"
    );

    let started = Instant::now();
    session.shutdown();
    let took = started.elapsed();
    assert!(took < Duration::from_millis(200), "shutdown took {took:?}");

    let requested = loop {
        if let SessionEvent::Exited { requested, .. } = common::next_event(&events, TIMEOUT) {
            break requested;
        }
    };
    assert!(requested);
    assert!(session.send_prompt("too late").is_err());
}

#[test]
fn unexpected_exit_stops_the_tools_claude_started() {
    // A stand-in that starts a background tool and then exits on its own, like a crash. The
    // tool shares claude's process group and must not outlive it.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("crashing-claude");
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    writeln!(file, "sleep 60 >/dev/null 2>&1 &").unwrap();
    writeln!(file, r#"echo "{{\"type\":\"system\",\"subtype\":\"$!\"}}""#).unwrap();
    writeln!(file, "exit 7").unwrap();
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut config = SessionConfig::new(dir.path());
    config.program = script;
    let (_session, events) = Session::spawn(config).unwrap();
    let tool_pid: u32 = match common::next_event(&events, TIMEOUT) {
        SessionEvent::Line(Incoming::System(message)) => message.subtype.parse().unwrap(),
        other => panic!("unexpected event {other:?}"),
    };
    let (code, requested) = loop {
        if let SessionEvent::Exited { code, requested } = common::next_event(&events, TIMEOUT) {
            break (code, requested);
        }
    };
    assert_eq!(code, Some(7));
    assert!(!requested);
    assert!(
        wait_until_dead(tool_pid, Duration::from_secs(2)),
        "the crashed claude's tool survived"
    );
}

/// Writes an executable shell script into `dir` and returns a config that runs it as claude.
fn script_config(dir: &std::path::Path, name: &str, lines: &[&str]) -> SessionConfig {
    let script = dir.join(name);
    let mut file = std::fs::File::create(&script).unwrap();
    writeln!(file, "#!/bin/sh").unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
    drop(file);
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut config = SessionConfig::new(dir);
    config.program = script;
    config
}

#[test]
fn writes_fail_at_once_after_claude_exits() {
    let dir = tempfile::tempdir().unwrap();
    let config = script_config(dir.path(), "quitting-claude", &["exit 0"]);
    let (session, events) = Session::spawn(config).unwrap();
    loop {
        if let SessionEvent::Exited { requested, .. } = common::next_event(&events, TIMEOUT) {
            assert!(!requested);
            break;
        }
    }
    // The queue is closed before `Exited` is sent, so nothing is queued into a dead pipe.
    assert!(session.send_prompt("hello").is_err());
    assert!(session.interrupt().is_err());
}

#[test]
fn a_failed_write_is_reported_as_one_error() {
    // A stand-in that closes its stdin but keeps running, so our write fails with a broken
    // pipe while claude is still alive.
    let dir = tempfile::tempdir().unwrap();
    let config = script_config(
        dir.path(),
        "deaf-claude",
        &[
            "exec 0<&-",
            r#"echo '{"type":"system","subtype":"ready"}'"#,
            "exec sleep 60",
        ],
    );
    let (session, events) = Session::spawn(config).unwrap();
    let mut transcript = Transcript::new();
    transcript.apply(&common::next_event(&events, TIMEOUT));

    session.send_prompt("hello").unwrap();
    transcript.push_user("hello");
    loop {
        let event = common::next_event(&events, TIMEOUT);
        transcript.apply(&event);
        match event {
            SessionEvent::WriteFailed(_) => break,
            SessionEvent::Exited { .. } => panic!("exited before reporting the failed write"),
            _ => {}
        }
    }
    assert!(session.send_prompt("again").is_err());

    session.shutdown();
    loop {
        let event = common::next_event(&events, TIMEOUT);
        transcript.apply(&event);
        if matches!(event, SessionEvent::Exited { .. }) {
            break;
        }
    }
    let errors: Vec<&str> = transcript
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Error { title, .. } => Some(title.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(errors, ["Could not reach Claude"]);
    assert_eq!(transcript.status(), Status::Failed);
}

#[test]
fn crash_is_reported_even_when_a_tool_keeps_stdout_open() {
    // The tool inherits claude's stdout and ignores SIGTERM, so stdout never reaches EOF on
    // its own. `Exited` must still arrive, and the tool must be killed (SIGTERM, then SIGKILL).
    let dir = tempfile::tempdir().unwrap();
    let config = script_config(
        dir.path(),
        "crashing-claude",
        &[
            "trap '' TERM",
            "sleep 60 &",
            r#"echo "{\"type\":\"system\",\"subtype\":\"$!\"}""#,
            "exit 7",
        ],
    );
    let (_session, events) = Session::spawn(config).unwrap();
    let tool_pid: u32 = match common::next_event(&events, TIMEOUT) {
        SessionEvent::Line(Incoming::System(message)) => message.subtype.parse().unwrap(),
        other => panic!("unexpected event {other:?}"),
    };
    // Far below the tool's 60 s: before this fix, `Exited` waited for stdout to close.
    let (code, requested) = loop {
        if let SessionEvent::Exited { code, requested } =
            common::next_event(&events, Duration::from_secs(3))
        {
            break (code, requested);
        }
    };
    assert_eq!(code, Some(7));
    assert!(!requested);
    assert!(
        wait_until_dead(tool_pid, Duration::from_secs(1)),
        "the tool holding stdout survived"
    );
}

/// The pid of a `sleep` child of `parent`, using `pgrep -P`. (claude's other child is the
/// owner-pipe watcher, a shell.)
fn find_sleep_child_of(parent: u32) -> Option<u32> {
    pgrep(&["-P", &parent.to_string(), "-x", "sleep"])
        .first()
        .copied()
}

/// The pids `pgrep` prints for `args`.
fn pgrep(args: &[&str]) -> Vec<u32> {
    let output = std::process::Command::new("pgrep")
        .args(args)
        .output()
        .expect("run pgrep");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// The processes in claude's process group: claude, its tools, and the watcher.
fn group_members(pid: u32) -> Vec<u32> {
    pgrep(&["-g", &pid.to_string()])
}

/// Waits for the next event that is not a stdout or stderr line.
fn next_lifecycle_event(events: &async_channel::Receiver<SessionEvent>) -> SessionEvent {
    loop {
        match common::next_event(events, TIMEOUT) {
            SessionEvent::Line(_) | SessionEvent::Stderr(_) => {}
            other => return other,
        }
    }
}

#[test]
fn unsupported_control_requests_get_an_error_reply() {
    // A stand-in that sends a control request Newjen does not handle, then prints the line it
    // gets back on stderr.
    let dir = tempfile::tempdir().unwrap();
    let config = script_config(
        dir.path(),
        "hooked-claude",
        &[
            r#"echo '{"type":"control_request","request_id":"hook_1","request":{"subtype":"hook_callback","callback_id":"c1"}}'"#,
            "read line",
            r#"printf '%s\n' "$line" >&2"#,
            "exec sleep 60",
        ],
    );
    let (session, events) = Session::spawn(config).unwrap();
    let reply = loop {
        if let SessionEvent::Stderr(line) = common::next_event(&events, TIMEOUT) {
            break line;
        }
    };
    let reply: serde_json::Value = serde_json::from_str(&reply).expect("the reply is JSON");
    assert_eq!(
        reply,
        serde_json::json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": "hook_1",
            "error": "Newjen does not support hook_callback yet"}})
    );
    session.shutdown();
}

#[test]
fn claude_and_its_tools_die_when_newjen_dies() {
    // Everything here ignores SIGTERM, so only the watcher's SIGKILL (2 s after its SIGTERM)
    // can end it. Nothing in Newjen signals the group: the session is still "running".
    let dir = tempfile::tempdir().unwrap();
    let config = script_config(
        dir.path(),
        "stubborn-claude",
        &[
            "trap '' TERM",
            "sleep 60 &",
            r#"echo "{\"type\":\"system\",\"subtype\":\"$!\"}""#,
            "wait",
        ],
    );
    let (session, events) = Session::spawn(config).unwrap();
    let tool_pid: u32 = match common::next_event(&events, TIMEOUT) {
        SessionEvent::Line(Incoming::System(message)) => message.subtype.parse().unwrap(),
        other => panic!("unexpected event {other:?}"),
    };
    let pid = session.pid();

    session.simulate_owner_death();
    assert!(
        wait_until_dead(tool_pid, Duration::from_secs(4)),
        "the tool survived its owner"
    );
    // claude is reaped by the waiter thread once it is killed.
    assert!(
        wait_until_dead(pid, Duration::from_secs(1)),
        "claude survived"
    );
    let SessionEvent::Exited { requested, .. } = next_lifecycle_event(&events) else {
        panic!("expected Exited");
    };
    assert!(!requested, "Newjen did not ask for this exit");
}

#[test]
fn no_watcher_is_left_after_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let config = script_config(
        dir.path(),
        "idle-claude",
        &[
            r#"echo '{"type":"system","subtype":"ready"}'"#,
            "exec sleep 60",
        ],
    );
    let (session, events) = Session::spawn(config).unwrap();
    assert!(matches!(
        common::next_event(&events, TIMEOUT),
        SessionEvent::Line(Incoming::System(_))
    ));
    let pid = session.pid();
    assert_eq!(
        group_members(pid).len(),
        2,
        "expected claude and its watcher in the group"
    );

    session.shutdown();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !group_members(pid).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        group_members(pid),
        Vec::<u32>::new(),
        "something outlived shutdown"
    );
}

/// A stand-in that prints one line and then never reads stdin, so it never answers anything.
fn deaf_config(dir: &std::path::Path) -> SessionConfig {
    let mut config = script_config(
        dir,
        "deaf-claude",
        &[
            r#"echo '{"type":"system","subtype":"ready"}'"#,
            "exec sleep 60",
        ],
    );
    config.interrupt_ack_timeout = Duration::from_millis(200);
    config
}

#[test]
fn an_unanswered_interrupt_ends_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let (session, events) = Session::spawn(deaf_config(dir.path())).unwrap();
    let mut transcript = Transcript::new();
    transcript.apply(&common::next_event(&events, TIMEOUT));
    session.send_prompt("hello").unwrap();
    transcript.push_user("hello");

    session.interrupt().unwrap();
    let timed_out = next_lifecycle_event(&events);
    assert_eq!(timed_out, SessionEvent::InterruptTimedOut);
    transcript.apply(&timed_out);
    let exited = next_lifecycle_event(&events);
    assert!(
        matches!(
            exited,
            SessionEvent::Exited {
                requested: true,
                ..
            }
        ),
        "{exited:?}"
    );
    transcript.apply(&exited);
    assert!(wait_until_dead(session.pid(), Duration::from_secs(1)));

    assert_eq!(transcript.status(), Status::Failed);
    assert!(transcript.items().iter().any(|item| matches!(
        item,
        Item::Error { title, .. } if title == "Claude didn't stop, so Newjen ended the session"
    )));
}

#[test]
fn an_answered_interrupt_keeps_the_session() {
    // Answers the first interrupt (Newjen numbers them from 1), then idles.
    let dir = tempfile::tempdir().unwrap();
    let mut config = script_config(
        dir.path(),
        "polite-claude",
        &[
            r#"echo '{"type":"system","subtype":"ready"}'"#,
            "read line",
            r#"echo '{"type":"control_response","response":{"subtype":"success","request_id":"newjen_interrupt_1","response":{}}}'"#,
            "exec sleep 60",
        ],
    );
    config.interrupt_ack_timeout = Duration::from_millis(500);
    let (session, events) = Session::spawn(config).unwrap();
    // Start the clock only once the stand-in runs, so a slow start can't look like silence.
    assert!(matches!(
        common::next_event(&events, TIMEOUT),
        SessionEvent::Line(Incoming::System(_))
    ));
    session.interrupt().unwrap();
    let event = common::next_event(&events, TIMEOUT);
    assert!(
        matches!(event, SessionEvent::Line(Incoming::ControlResponse(_))),
        "{event:?}"
    );
    // Well past the deadline: nothing more happens, and claude is still running.
    std::thread::sleep(Duration::from_millis(1500));
    assert!(events.try_recv().is_err(), "no event expected");
    assert!(is_alive(session.pid()));
    session.shutdown();
}
