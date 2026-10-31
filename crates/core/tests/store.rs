//! The SQLite store: what is written comes back, a shorter transcript leaves no stale rows,
//! and a failed write is reported instead of crashing.

use std::path::PathBuf;
use std::time::Duration;

use newjen_core::rift::Copy;
use newjen_core::store::{Grant, Store, ThreadRow};
use newjen_core::transcript::Status;

const FLUSH: Duration = Duration::from_secs(5);

fn thread_row(id: &str, project_id: i64, created_at: i64) -> ThreadRow {
    ThreadRow {
        id: id.to_string(),
        project_id,
        workdir: PathBuf::from("/tmp/project"),
        title: format!("Thread {id}"),
        provider: "claude".to_string(),
        session_id: format!("session-{id}"),
        session_started: false,
        status: Status::Done,
        unread: false,
        created_at,
        updated_at: created_at,
        archived_at: None,
        copy: None,
    }
}

fn copy(name: &str) -> Copy {
    Copy {
        path: PathBuf::from(format!("/tmp/.rifts/project/{name}")),
        workdir: PathBuf::from(format!("/tmp/.rifts/project/{name}/app")),
        source: PathBuf::from("/tmp/project"),
        rift_id: format!("01RIFT{}", name.to_uppercase()),
        base: Some("4f0b21f3aa".to_string()),
    }
}

fn rows(texts: &[&str]) -> Vec<(u32, String)> {
    texts.iter().map(|text| (1, text.to_string())).collect()
}

fn saved_json(store: &Store, thread_id: &str) -> Vec<(usize, String)> {
    store
        .items(thread_id)
        .unwrap()
        .into_iter()
        .map(|item| (item.ix, item.json))
        .collect()
}

#[test]
fn projects_threads_items_and_grants_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project_dir = home.path().join("my project");
    let project = store.open_project(&project_dir).unwrap();
    assert_eq!(project.path, project_dir);
    // Opening it again finds the same project and records the time.
    let again = store.open_project(&project_dir).unwrap();
    assert_eq!(again.id, project.id);
    assert_eq!(again.created_at, project.created_at);
    assert!(again.last_opened_at >= project.last_opened_at);

    let mut row = thread_row("t1", project.id, 1_000);
    store.save_thread(row.clone());
    store.replace_items("t1", 0, rows(&["a", "b"]));
    store.add_grant(
        "t1",
        Grant {
            rule: "Bash(touch one.txt)".to_string(),
            label: "touch one.txt".to_string(),
            created_at: 5,
        },
    );
    store.add_grant(
        "t1",
        Grant {
            rule: "mcp__pencil__get_style".to_string(),
            label: "pencil · get_style".to_string(),
            created_at: 6,
        },
    );
    assert!(store.flush(FLUSH));

    assert_eq!(store.thread("t1").unwrap(), Some(row.clone()));
    assert_eq!(
        saved_json(&store, "t1"),
        [(0, "a".to_string()), (1, "b".to_string())]
    );
    let grants = store.grants("t1").unwrap();
    let rules: Vec<&str> = grants.iter().map(|grant| grant.rule.as_str()).collect();
    assert_eq!(rules, ["Bash(touch one.txt)", "mcp__pencil__get_style"]);

    // Updating the row changes what a conversation changes, nothing else.
    row.title = "Renamed".to_string();
    row.session_started = true;
    row.status = Status::Stopped;
    row.updated_at = 2_000;
    let mut sneaky = row.clone();
    sneaky.created_at = 9_999;
    store.save_thread(sneaky);
    store.set_unread("t1", true);
    store.remove_grant("t1", "Bash(touch one.txt)");
    // The same rule twice keeps one row (with the newer label).
    store.add_grant(
        "t1",
        Grant {
            rule: "mcp__pencil__get_style".to_string(),
            label: "newer label".to_string(),
            created_at: 7,
        },
    );
    assert!(store.flush(FLUSH));
    let saved = store.thread("t1").unwrap().unwrap();
    assert_eq!(saved.title, "Renamed");
    assert!(saved.session_started);
    assert_eq!(saved.status, Status::Stopped);
    assert!(saved.unread);
    assert_eq!(saved.updated_at, 2_000);
    assert_eq!(saved.created_at, 1_000);
    let grants = store.grants("t1").unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].label, "newer label");
}

#[test]
fn replacing_items_from_an_index_leaves_no_stale_rows() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    store.save_thread(thread_row("t", project.id, 1));
    store.replace_items("t", 0, rows(&["0", "1", "2", "3"]));
    // Rows from 1 on are rewritten, and there are fewer of them now.
    store.replace_items("t", 1, rows(&["1b"]));
    assert!(store.flush(FLUSH));
    assert_eq!(
        saved_json(&store, "t"),
        [(0, "0".to_string()), (1, "1b".to_string())]
    );
    // Appending only writes the new rows.
    store.replace_items("t", 2, rows(&["2c", "3c"]));
    assert!(store.flush(FLUSH));
    assert_eq!(saved_json(&store, "t").len(), 4);
    // From 0 with nothing: the transcript is empty.
    store.replace_items("t", 0, Vec::new());
    assert!(store.flush(FLUSH));
    assert!(saved_json(&store, "t").is_empty());
}

#[test]
fn threads_are_listed_newest_first_without_archived_ones() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    let other = store.open_project(&home.path().join("q")).unwrap();
    assert_ne!(project.id, other.id);
    store.save_thread(thread_row("old", project.id, 1));
    store.save_thread(thread_row("new", project.id, 3));
    store.save_thread(thread_row("middle", project.id, 2));
    store.save_thread(thread_row("gone", project.id, 4));
    store.save_thread(thread_row("elsewhere", other.id, 5));
    store.archive("gone", 10);
    // Activity does not reorder the list.
    let mut old = thread_row("old", project.id, 1);
    old.updated_at = 100;
    store.save_thread(old);
    assert!(store.flush(FLUSH));

    let ids: Vec<String> = store
        .threads(project.id)
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(ids, ["new", "middle", "old"]);
    // An archived thread is still there, just hidden.
    assert_eq!(store.thread("gone").unwrap().unwrap().archived_at, Some(10));
}

#[test]
fn a_failed_write_is_reported_and_later_writes_still_work() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    let errors = store.write_errors();
    // Items for a thread that does not exist break the foreign key.
    store.replace_items("no-such-thread", 0, rows(&["x"]));
    store.save_thread(thread_row("t", project.id, 1));
    assert!(store.flush(FLUSH));

    let error = errors.try_recv().expect("the failed write is reported");
    let message = error.to_string();
    assert!(message.starts_with("Couldn't save threads: "), "{message}");
    assert!(message.contains("FOREIGN KEY"), "{message}");
    assert!(errors.try_recv().is_err(), "only one write failed");
    assert!(store.thread("t").unwrap().is_some());
    assert!(store.items("no-such-thread").unwrap().is_empty());
}

#[test]
fn a_checkpoint_saves_the_row_and_its_items_together_or_not_at_all() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    let errors = store.write_errors();

    store.save_checkpoint(thread_row("t", project.id, 1), 0, rows(&["a", "b"]));
    assert!(store.flush(FLUSH));
    assert!(store.thread("t").unwrap().is_some());
    assert_eq!(saved_json(&store, "t").len(), 2);

    // A row the database refuses (no such project): its items are not saved either.
    store.save_checkpoint(thread_row("orphan", 9_999, 1), 0, rows(&["x"]));
    assert!(store.flush(FLUSH));
    assert!(errors.try_recv().is_ok());
    assert!(store.thread("orphan").unwrap().is_none());
    assert!(store.items("orphan").unwrap().is_empty());
}

#[test]
fn reopening_keeps_everything_at_the_same_schema_version() {
    let home = tempfile::tempdir().unwrap();
    let project_id = {
        let store = Store::open_in(home.path()).unwrap();
        let project = store.open_project(&home.path().join("p")).unwrap();
        store.save_thread(thread_row("t", project.id, 1));
        store.replace_items("t", 0, rows(&["a"]));
        assert!(store.flush(FLUSH));
        project.id
    };
    let store = Store::open_in(home.path()).unwrap();
    assert_eq!(store.threads(project_id).unwrap().len(), 1);
    assert_eq!(saved_json(&store, "t"), [(0, "a".to_string())]);

    let connection = rusqlite::Connection::open(store.path()).unwrap();
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
}

#[test]
fn deleting_a_thread_takes_its_rows_and_grants_with_it() {
    // Not something Newjen does yet (it archives), but the schema must not leave orphans
    // when it does.
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    let mut row = thread_row("t", project.id, 1);
    row.copy = Some(copy("c"));
    store.save_thread(row);
    store.replace_items("t", 0, rows(&["a"]));
    store.add_grant(
        "t",
        Grant {
            rule: "Bash(ls)".to_string(),
            label: "ls".to_string(),
            created_at: 1,
        },
    );
    assert!(store.flush(FLUSH));

    let connection = rusqlite::Connection::open(store.path()).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    connection
        .execute("DELETE FROM projects WHERE id = ?1", [project.id])
        .unwrap();
    assert!(store.thread("t").unwrap().is_none());
    assert!(store.items("t").unwrap().is_empty());
    assert!(store.grants("t").unwrap().is_empty());
    assert_eq!(store.copy_owner(&copy("c").path).unwrap(), None);
}

#[test]
fn a_threads_copy_is_saved_with_its_row_and_never_changes_after() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    let mut row = thread_row("t1", project.id, 1);
    row.copy = Some(copy("first"));
    store.save_checkpoint(row.clone(), 0, rows(&["a"]));
    store.save_thread(thread_row("t2", project.id, 2));
    assert!(store.flush(FLUSH));
    assert_eq!(
        store.thread("t1").unwrap().unwrap().copy,
        Some(copy("first"))
    );
    let listed: Vec<Option<Copy>> = store
        .threads(project.id)
        .unwrap()
        .into_iter()
        .map(|row| row.copy)
        .collect();
    assert_eq!(listed, [None, Some(copy("first"))]);

    row.title = "Renamed".to_string();
    row.copy = Some(copy("second"));
    store.save_thread(row);
    assert!(store.flush(FLUSH));
    let saved = store.thread("t1").unwrap().unwrap();
    assert_eq!(saved.title, "Renamed");
    assert_eq!(saved.copy, Some(copy("first")));
}

#[test]
fn archived_threads_copies_wait_to_be_removed_until_marked() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(&home.path().join("p")).unwrap();
    for (id, copy) in [
        ("archived", Some(copy("a"))),
        ("visible", Some(copy("v"))),
        ("plain", None),
    ] {
        let mut row = thread_row(id, project.id, 1);
        row.copy = copy;
        store.save_thread(row);
    }
    store.archive("archived", 10);
    store.archive("plain", 10);
    assert!(store.flush(FLUSH));

    assert_eq!(
        store.copies_to_remove().unwrap(),
        [("archived".to_string(), copy("a"))]
    );
    let owner = |name: &str| store.copy_owner(&copy(name).path).unwrap();
    assert_eq!(owner("a").as_deref(), Some("archived"));
    assert_eq!(owner("v").as_deref(), Some("visible"));
    assert_eq!(owner("elsewhere"), None);

    store.mark_copy_removed("archived", 20);
    store.mark_copy_removed("archived", 30);
    assert!(store.flush(FLUSH));
    assert!(store.copies_to_remove().unwrap().is_empty());
    assert_eq!(owner("a"), None, "a removed copy belongs to no one");
    let removed_at: i64 = rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT removed_at FROM copies WHERE thread_id = 'archived'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(removed_at, 20, "marking it again changes nothing");
}
