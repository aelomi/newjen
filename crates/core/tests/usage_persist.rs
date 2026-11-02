mod common;

use std::time::{Duration, Instant};

use newjen_core::providers::Provider;
use newjen_core::session::SessionConfig;
use newjen_core::store::Store;
use newjen_core::thread::Thread;
use newjen_core::usage;

#[test]
fn the_plan_report_a_turn_brings_is_saved_as_it_arrives() {
    let home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    usage::attach_store(&store);
    assert!(store.provider_usages().unwrap().is_empty());

    let project = store.open_project(project_dir.path()).unwrap();
    let mut config = SessionConfig::new(project_dir.path());
    config.program = env!("CARGO_BIN_EXE_newjen-fake-claude").into();
    config.env.push((
        "NEWJEN_FAKE_FIXTURE".to_string(),
        common::fixture_path("text_reply").display().to_string(),
    ));
    let mut thread = Thread::new_draft(store.clone(), &project, config);
    thread.set_active(true);
    thread.send("hi").unwrap();
    let events = thread.events();
    let deadline = Instant::now() + Duration::from_secs(10);
    while usage::latest(Provider::Claude).is_none() || thread.status().is_busy() {
        assert!(Instant::now() < deadline);
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }

    assert!(store.flush(Duration::from_secs(5)));
    let saved = store.provider_usages().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(Some(&saved[0]), usage::latest(Provider::Claude).as_ref());
    let five = saved[0]
        .windows
        .iter()
        .find(|w| w.key == "five_hour")
        .unwrap();
    assert_eq!(five.used_fraction, Some(0.13));
    assert_eq!(five.length_seconds, Some(18_000));
}
