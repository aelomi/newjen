mod common;

use std::time::{Duration, Instant};

use newjen_core::agent::{ContextFill, TokenCounts};
use newjen_core::providers::Provider;
use newjen_core::session::SessionConfig;
use newjen_core::store::{Project, Store};
use newjen_core::thread::Thread;
use rusqlite::Connection;

const TIMEOUT: Duration = Duration::from_secs(10);

struct Setup {
    _home: tempfile::TempDir,
    _project_dir: tempfile::TempDir,
    store: Store,
    project: Project,
    config: SessionConfig,
}

fn setup() -> Setup {
    let home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(project_dir.path()).unwrap();
    let mut config = SessionConfig::new(project_dir.path());
    config.program = env!("CARGO_BIN_EXE_newjen-fake-claude").into();
    config.env.push((
        "NEWJEN_FAKE_FIXTURE".to_string(),
        common::fixture_path("multi_turn").display().to_string(),
    ));
    Setup {
        _home: home,
        _project_dir: project_dir,
        store,
        project,
        config,
    }
}

fn send_and_finish(thread: &mut Thread, text: &str) {
    let turns = thread.transcript().usage().turns();
    thread.send(text).unwrap();
    let events = thread.events();
    let deadline = Instant::now() + TIMEOUT;
    while thread.transcript().usage().turns() == turns || thread.status().is_busy() {
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => {
                assert!(
                    Instant::now() < deadline,
                    "{:#?}",
                    thread.transcript().items()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn haiku(input: u64, cached: u64, written: u64, output: u64) -> TokenCounts {
    TokenCounts {
        input,
        cached_input: cached,
        cache_write_input: written,
        output,
        reasoning_output: 0,
    }
}

#[test]
fn a_thread_saves_each_turns_usage_and_a_reloaded_thread_shows_the_same() {
    let setup = setup();
    let mut thread = Thread::new_draft(setup.store.clone(), &setup.project, setup.config.clone());
    thread.set_active(true);
    send_and_finish(&mut thread, "one");
    send_and_finish(&mut thread, "two");

    let usage = thread.transcript().usage().clone();
    assert_eq!(usage.turns(), 2);
    assert_eq!(usage.total(), haiku(4, 31240, 27607, 10));
    assert_eq!(
        usage.context(),
        Some(ContextFill {
            used: 2 + 13608 + 15859 + 6,
            window: Some(1_000_000)
        })
    );

    assert!(setup.store.flush(Duration::from_secs(5)));
    let rows = setup.store.turn_usage(thread.id()).unwrap();
    assert_eq!(rows.iter().map(|row| row.turn).collect::<Vec<_>>(), [0, 1]);
    assert!(rows.iter().all(|row| row.provider == Provider::Claude
        && row.model == "claude-haiku-5-5"
        && row.context_window == Some(1_000_000)));
    assert_eq!(rows[0].tokens, haiku(2, 15381, 13999, 4));
    assert_eq!(rows[0].context_used, Some(29386));
    assert_eq!(rows[1].tokens, haiku(2, 15859, 13608, 6));

    let saved = setup.store.thread(thread.id()).unwrap().unwrap();
    let reloaded = Thread::load(
        setup.store.clone(),
        &setup.project,
        saved,
        setup.config.clone(),
    )
    .unwrap();
    assert_eq!(reloaded.transcript().usage(), &usage);
}

#[test]
fn usage_rows_lost_to_a_failed_write_are_written_again_with_the_next_turn() {
    let setup = setup();
    let mut thread = Thread::new_draft(setup.store.clone(), &setup.project, setup.config.clone());
    thread.set_active(true);
    let other = Connection::open(setup.store.path()).unwrap();
    other
        .execute_batch(
            "CREATE TRIGGER refuse_usage BEFORE INSERT ON turn_usage
                 BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
        )
        .unwrap();
    send_and_finish(&mut thread, "one");
    assert!(setup.store.flush(Duration::from_secs(5)));
    assert!(setup.store.turn_usage(thread.id()).unwrap().is_empty());
    assert!(setup.store.write_errors().try_recv().is_ok());

    other.execute_batch("DROP TRIGGER refuse_usage;").unwrap();
    send_and_finish(&mut thread, "two");
    assert!(setup.store.flush(Duration::from_secs(5)));
    let rows = setup.store.turn_usage(thread.id()).unwrap();
    assert_eq!(rows.iter().map(|row| row.turn).collect::<Vec<_>>(), [0, 1]);
}
