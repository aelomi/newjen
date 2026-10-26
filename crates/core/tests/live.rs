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
