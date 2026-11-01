mod common;

use std::time::{Duration, Instant};

use newjen_core::providers::Provider;
use newjen_core::session::SessionConfig;
use newjen_core::store::Store;
use newjen_core::thread::Thread;
use newjen_core::usage;

#[test]
fn a_replayed_claude_turn_updates_the_latest_usage_for_the_app() {
    let codex_before = usage::latest(Provider::Codex);
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(project_dir.path()).unwrap();
    let mut config = SessionConfig::new(project_dir.path());
    config.program = env!("CARGO_BIN_EXE_newjen-fake-claude").into();
    config.env.push((
        "NEWJEN_FAKE_FIXTURE".to_string(),
        common::fixture_path("text_reply").display().to_string(),
    ));
    let mut thread = Thread::new_draft(store, &project, config);
    thread.set_active(true);
    thread.send("hi").unwrap();

    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut finished = false;
    while !finished {
        assert!(
            Instant::now() < deadline,
            "{:#?}",
            thread.transcript().items()
        );
        let Ok(event) = events.try_recv() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        let was_busy = thread.status().is_busy();
        thread.apply(event);
        finished = was_busy && !thread.status().is_busy();
    }

    let latest = usage::latest(Provider::Claude).expect("usage recorded");
    let five = latest
        .windows
        .iter()
        .find(|w| w.key == "five_hour")
        .unwrap();
    assert_eq!(five.used_fraction, Some(0.13));
    assert!(
        latest
            .updated_at_unix_seconds
            .is_some_and(|at| at >= started)
    );
    assert_eq!(usage::latest(Provider::Codex), codex_before);
}
