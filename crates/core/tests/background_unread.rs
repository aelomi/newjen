mod common;

use std::time::{Duration, Instant};

use newjen_core::session::SessionConfig;
use newjen_core::store::Store;
use newjen_core::thread::Thread;

#[test]
fn a_turn_claude_starts_after_a_background_task_marks_an_off_screen_thread_unread() {
    let home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(project_dir.path()).unwrap();
    let mut config = SessionConfig::new(project_dir.path());
    config.program = env!("CARGO_BIN_EXE_newjen-fake-claude").into();
    config.env.push((
        "NEWJEN_FAKE_FIXTURE".to_string(),
        common::fixture_path("background_task")
            .display()
            .to_string(),
    ));

    let mut thread = Thread::new_draft(store, &project, config);
    thread.set_active(true);
    thread.send("Run it in the background").unwrap();

    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut turns_ended = 0;
    while turns_ended < 2 {
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
        if was_busy && !thread.status().is_busy() {
            turns_ended += 1;
            if turns_ended == 1 {
                assert!(!thread.is_unread());
                thread.set_active(false);
            }
        }
    }
    assert!(thread.is_unread(), "claude's own turn ended off screen");
}
