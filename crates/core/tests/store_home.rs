//! `NEWJEN_HOME` decides where the database lives.
//!
//! This is the only test in this file on purpose: it changes the process's environment, and
//! each file in `tests/` is its own program, so no other test can run at the same time and see
//! the change.

use newjen_core::store::{DB_FILE, Store};

#[test]
fn newjen_home_holds_a_freshly_created_database() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("nested").join("newjen-home");
    // SAFETY: changing the environment is only unsound while another thread reads it. This
    // test is alone in its program (see the file docs) and starts no threads before this.
    unsafe { std::env::set_var("NEWJEN_HOME", &home) };

    let store = Store::open().expect("open the store in NEWJEN_HOME");
    assert_eq!(store.path(), home.join(DB_FILE));
    assert!(
        home.join(DB_FILE).is_file(),
        "the folder and file are created"
    );

    // The schema is in place: a project and a thread can be saved and read back.
    let project = store.open_project(&temp.path().join("project")).unwrap();
    assert!(store.threads(project.id).unwrap().is_empty());
    let connection = rusqlite::Connection::open(store.path()).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, newjen_core::store::SCHEMA_VERSION);
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        [
            "app_state",
            "copies",
            "copy_cleanup",
            "grants",
            "pending_copies",
            "project_agent_choices",
            "projects",
            "provider_usage",
            "thread_items",
            "thread_limits",
            "threads",
            "turn_usage"
        ]
    );
}
