//! Runs real `Thread`s against a shell-script stand-in for claude, with a real store in a temp
//! folder. No network, no Claude login.
//!
//! The stand-in (`STAND_IN`) records the arguments of every start in `spawns.log` and every
//! stdin line in `stdin.log`. Empty files in its folder switch on odd behaviour, so a test can
//! change it between turns:
//! - `new_in_use` / `resume_in_use`: with `--session-id` / `--resume`, print claude's
//!   "already in use" error on stderr and exit 1, without reading stdin.
//! - `new_missing` / `resume_missing`: print claude's "No conversation found" result and exit
//!   1, without reading stdin. claude 2.1.314 checks `--resume` as it starts, before it reads
//!   a message (checked live), so this also happens with no message at all.
//! - `other_in_use`: print another error that says "already in use" (a busy port) on stderr
//!   and exit 1, without reading stdin.
//! - `crash`: read a message, then exit 1 without answering.
//! - `vanish`: delete the stand-in as it starts, so the next start fails.
//! - `ask`: ask to approve `touch one.txt` (with the CLI's usual suggestion) before replying.
//! - `hang`: start the turn (a running `sleep` tool call and a reply being streamed) and never
//!   finish it.
//! - `slow_stop`: ignore SIGTERM, as a claude busy with its exit hooks might, so stopping it
//!   takes the whole grace period before SIGKILL.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use newjen_core::protocol::PermissionAnswer;
use newjen_core::session::SessionConfig;
use newjen_core::store::{Project, Store};
use newjen_core::thread::{CONTINUE_MESSAGE, IDLE_TIMEOUT, NEW_THREAD_TITLE, Thread, finish_quit};
use newjen_core::transcript::{Item, STOPPED_TOOL_NOTE, Status, ToolState};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(10);
const FLUSH: Duration = Duration::from_secs(5);

const STAND_IN: &str = r##"#!/bin/sh
dir="$FAKE_DIR"
{ for arg in "$@"; do printf '%s\n' "$arg"; done; echo "---"; } >> "$dir/spawns.log"
if [ -f "$dir/slow_stop" ]; then trap '' TERM; fi
if [ -f "$dir/vanish" ]; then rm -f "$0"; fi
mode=""; sid=""; prev=""
for arg in "$@"; do
  case "$prev" in
    --session-id) mode=new; sid="$arg" ;;
    --resume) mode=resume; sid="$arg" ;;
  esac
  prev="$arg"
done
if [ -f "$dir/${mode}_in_use" ]; then
  # The real CLI takes a moment to start before it says this.
  sleep 0.2
  echo "Error: Session ID $sid is already in use." >&2
  exit 1
fi
if [ -f "$dir/other_in_use" ]; then
  sleep 0.2
  echo "Error: listen EADDRINUSE: address is already in use :::3000" >&2
  exit 1
fi
if [ -f "$dir/${mode}_missing" ]; then
  sleep 0.2
  printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: '"$sid"'"]}'
  exit 1
fi
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/stdin.log"
  if [ -f "$dir/crash" ]; then
    exit 1
  fi
  printf '%s\n' '{"type":"system","subtype":"init","session_id":"'"$sid"'"}'
  if [ -f "$dir/hang" ]; then
    printf '%s\n' '{"type":"assistant","message":{"id":"m0","content":[{"type":"tool_use","id":"t0","name":"Bash","input":{"command":"sleep 60"}}]}}'
    printf '%s\n' '{"type":"stream_event","api_message_id":"m1","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Still going"}}}'
    exec sleep 60
  fi
  if [ -f "$dir/ask" ]; then
    printf '%s\n' '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"t1","input":{"command":"touch one.txt"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch one.txt"}],"behavior":"allow","destination":"localSettings"}]}}'
    IFS= read -r answer
    printf '%s\n' "$answer" >> "$dir/stdin.log"
    printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"","is_error":false}]}}'
  fi
  printf '%s\n' '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"done"}]}}'
  printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":5}'
done
"##;

/// A temp folder with the stand-in, a store and a project.
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    project: Project,
    config: SessionConfig,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("claude-stand-in");
        std::fs::write(&script, STAND_IN).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let project_dir = dir.path().join("project");
        std::fs::create_dir(&project_dir).unwrap();

        let store = Store::open_in(&dir.path().join("home")).unwrap();
        let project = store.open_project(&project_dir).unwrap();
        let mut config = SessionConfig::new(&project_dir);
        config.program = script;
        config
            .env
            .push(("FAKE_DIR".to_string(), dir.path().display().to_string()));
        Fixture {
            dir,
            store,
            project,
            config,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Turns one of the stand-in's switches on or off.
    fn switch(&self, name: &str, on: bool) {
        let path = self.path(name);
        if on {
            std::fs::write(path, "").unwrap();
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    fn draft(&self) -> Thread {
        Thread::new_draft(self.store.clone(), &self.project, self.config.clone())
    }

    /// The thread as a relaunched Newjen would load it from the store.
    fn reload(&self, id: &str) -> Thread {
        assert!(self.store.flush(FLUSH));
        let row = self.store.thread(id).unwrap().expect("the thread is saved");
        Thread::load(self.store.clone(), &self.project, row, self.config.clone()).unwrap()
    }

    /// The arguments of every start of the stand-in, in order.
    fn spawns(&self) -> Vec<Vec<String>> {
        let log = std::fs::read_to_string(self.path("spawns.log")).unwrap_or_default();
        let mut spawns = Vec::new();
        let mut current = Vec::new();
        for line in log.lines() {
            if line == "---" {
                spawns.push(std::mem::take(&mut current));
            } else {
                current.push(line.to_string());
            }
        }
        spawns
    }

    /// Every line the stand-ins read from stdin, decoded.
    fn stdin(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("stdin.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).expect("stdin lines are JSON"))
            .collect()
    }

    /// The user messages the stand-ins received, in order.
    fn messages_received(&self) -> Vec<String> {
        self.stdin()
            .iter()
            .filter(|line| line["type"] == "user")
            .map(|line| line["message"]["content"].as_str().unwrap().to_string())
            .collect()
    }

    /// Waits until the stand-in has been started `count` times in all.
    fn wait_for_spawns(&self, count: usize) {
        let deadline = Instant::now() + TIMEOUT;
        while self.spawns().len() < count {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the stand-in"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until the stand-ins have received `count` user messages in all.
    fn wait_for_messages(&self, count: usize) {
        let deadline = Instant::now() + TIMEOUT;
        while self.messages_received().len() < count {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the stand-in"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A second connection to the database, to change it behind the store's back.
    fn database(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.store.path()).unwrap()
    }
}

/// The value after `flag` in `args`, if the flag is there.
fn flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.get(at + 1).map(String::as_str)
}

/// Every value given with `--allowedTools`.
fn allowed_tools(args: &[String]) -> Vec<&str> {
    args.windows(2)
        .filter(|pair| pair[0] == "--allowedTools")
        .map(|pair| pair[1].as_str())
        .collect()
}

/// Feeds the thread its events until `done` says so.
fn pump_until(thread: &mut Thread, what: &str, done: impl Fn(&Thread) -> bool) {
    let events = thread.events();
    let deadline = Instant::now() + TIMEOUT;
    while !done(thread) {
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(async_channel::TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {what}: {:#?}",
                    thread.transcript().items()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(async_channel::TryRecvError::Closed) => panic!("event channel closed"),
        }
    }
}

/// Feeds events until the turn is over.
fn finish_turn(thread: &mut Thread) {
    pump_until(thread, "the turn to end", |thread| {
        !thread.status().is_busy()
    });
}

/// Waits a little and feeds the thread whatever arrived, for checking that nothing more
/// happens.
fn settle(thread: &mut Thread) {
    std::thread::sleep(Duration::from_millis(500));
    while let Ok(event) = thread.events().try_recv() {
        thread.apply(event);
    }
}

/// The texts of the user's messages in the thread, in order.
fn user_texts(thread: &Thread) -> Vec<String> {
    thread
        .transcript()
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn errors(thread: &Thread) -> Vec<(String, String)> {
    thread
        .transcript()
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Error { title, detail, .. } => Some((title.clone(), detail.clone())),
            _ => None,
        })
        .collect()
}

/// A short name for each item, to compare transcripts at a glance.
fn kinds(thread: &Thread) -> Vec<&'static str> {
    thread
        .transcript()
        .items()
        .iter()
        .map(|item| match item {
            Item::User { .. } => "user",
            Item::Assistant { .. } => "assistant",
            Item::Tool { .. } => "tool",
            Item::TurnEnd { .. } => "turn_end",
            Item::Error { .. } => "error",
            Item::Stopped { .. } => "stopped",
            Item::FreshStart { .. } => "fresh_start",
            Item::Unreadable { .. } => "unreadable",
        })
        .collect()
}

#[test]
fn a_draft_is_saved_by_its_first_message() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    assert!(thread.is_draft());
    assert_eq!(thread.title(), NEW_THREAD_TITLE);
    assert!(!thread.has_session(), "claude starts only when needed");
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );

    thread
        .send("  Fix the   login bug\nIt fails on Safari")
        .unwrap();
    assert!(!thread.is_draft());
    assert_eq!(thread.title(), "Fix the login bug");
    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Done);
    assert!(thread.session_started(), "init arrived");

    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(flag(&spawns[0], "--session-id"), Some(thread.session_id()));
    assert_eq!(flag(&spawns[0], "--resume"), None);

    assert!(fixture.store.flush(FLUSH));
    let rows = fixture.store.threads(fixture.project.id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Fix the login bug");
    assert_eq!(rows[0].status, Status::Done);
    assert!(rows[0].session_started);
    assert_eq!(
        fixture.store.items(thread.id()).unwrap().len(),
        thread.transcript().items().len()
    );
}

#[test]
fn a_draft_that_cannot_start_claude_stays_unsaved() {
    let fixture = Fixture::new();
    let mut config = fixture.config.clone();
    config.program = "/nonexistent/claude".into();
    let mut thread = Thread::new_draft(fixture.store.clone(), &fixture.project, config);
    assert!(thread.send("hello").is_err());
    assert!(thread.is_draft());
    assert!(thread.transcript().items().is_empty());
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_finished_thread_loads_back_the_same_and_resumes() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    thread.send("two").unwrap();
    finish_turn(&mut thread);
    let items = thread.transcript().items().to_vec();

    let mut loaded = fixture.reload(thread.id());
    assert_eq!(loaded.transcript().items(), items.as_slice());
    assert_eq!(loaded.status(), Status::Done);
    assert_eq!(loaded.title(), "one");
    assert!(loaded.session_started());
    drop(thread);

    loaded.send("three").unwrap();
    finish_turn(&mut loaded);
    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 2, "one claude per thread instance");
    assert_eq!(flag(&spawns[1], "--resume"), Some(loaded.session_id()));
    // Item ids continue after the saved ones.
    let ids: Vec<u64> = loaded.transcript().items().iter().map(Item::id).collect();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
}

#[test]
fn already_in_use_resumes_and_resends_the_message_once() {
    let fixture = Fixture::new();
    fixture.switch("new_in_use", true);
    let mut thread = fixture.draft();
    thread.send("hello").unwrap();
    finish_turn(&mut thread);

    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 2);
    assert_eq!(flag(&spawns[0], "--session-id"), Some(thread.session_id()));
    assert_eq!(flag(&spawns[1], "--resume"), Some(thread.session_id()));
    // The first claude exited without reading anything; the second got the message.
    assert_eq!(fixture.messages_received(), ["hello"]);
    assert_eq!(thread.status(), Status::Done);
    assert_eq!(kinds(&thread), ["user", "assistant", "turn_end"]);
    assert!(
        errors(&thread).is_empty(),
        "the stderr line is not an error"
    );
    assert!(thread.session_started());
}

#[test]
fn already_in_use_twice_shows_one_error_with_the_reason() {
    let fixture = Fixture::new();
    fixture.switch("new_in_use", true);
    fixture.switch("resume_in_use", true);
    let mut thread = fixture.draft();
    thread.send("hello").unwrap();
    finish_turn(&mut thread);

    assert_eq!(fixture.spawns().len(), 2, "one retry only");
    assert_eq!(thread.status(), Status::Failed);
    let errors = errors(&thread);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "Claude exited");
    assert!(errors[0].1.contains("is already in use"), "{}", errors[0].1);
    assert!(!thread.has_session());
}

/// A thread from an earlier run whose conversation claude has already started.
fn started_thread(fixture: &Fixture) -> Thread {
    let mut thread = fixture.draft();
    thread.send("first").unwrap();
    finish_turn(&mut thread);
    assert!(thread.session_started());
    let id = thread.id().to_string();
    drop(thread);
    fixture.reload(&id)
}

#[test]
fn a_missing_conversation_starts_fresh_and_resends_the_message_once() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    let old_session = thread.session_id().to_string();
    fixture.switch("resume_missing", true);
    thread.send("second").unwrap();
    finish_turn(&mut thread);

    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 3, "first turn, failed resume, fresh start");
    assert_eq!(flag(&spawns[1], "--resume"), Some(old_session.as_str()));
    let new_session = flag(&spawns[2], "--session-id").expect("a fresh conversation");
    assert_ne!(new_session, old_session);
    assert_eq!(thread.session_id(), new_session);
    assert!(thread.session_started());
    // The failed resume read nothing; the fresh start got the message.
    assert_eq!(fixture.messages_received(), ["first", "second"]);
    assert_eq!(thread.status(), Status::Done);
    assert_eq!(
        kinds(&thread),
        [
            "user",
            "assistant",
            "turn_end",
            "user",
            "fresh_start",
            "assistant",
            "turn_end"
        ]
    );
    assert!(errors(&thread).is_empty());

    // The new conversation is saved, so the next launch resumes it.
    assert!(fixture.store.flush(FLUSH));
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.session_id, new_session);
    assert!(row.session_started);
}

#[test]
fn continue_is_not_sent_to_a_conversation_that_starts_fresh() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    let old_session = thread.session_id().to_string();
    fixture.switch("resume_missing", true);
    thread.send(CONTINUE_MESSAGE).unwrap();
    finish_turn(&mut thread);

    assert_eq!(
        fixture.spawns().len(),
        2,
        "first turn and the failed resume only"
    );
    assert_eq!(
        fixture.messages_received(),
        ["first"],
        "Continue is never sent"
    );
    assert_ne!(thread.session_id(), old_session);
    assert!(!thread.session_started());
    assert_eq!(thread.status(), Status::Stopped);
    assert_eq!(kinds(&thread).last(), Some(&"fresh_start"));
    assert!(errors(&thread).is_empty());
    assert!(!thread.has_session());

    // The user's next message starts the new conversation.
    fixture.switch("resume_missing", false);
    thread.send("hello again").unwrap();
    finish_turn(&mut thread);
    let spawns = fixture.spawns();
    assert_eq!(
        flag(spawns.last().unwrap(), "--session-id"),
        Some(thread.session_id())
    );
    assert_eq!(fixture.messages_received(), ["first", "hello again"]);
    assert_eq!(thread.status(), Status::Done);
}

#[test]
fn a_missing_conversation_twice_shows_one_error() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    fixture.switch("resume_missing", true);
    fixture.switch("new_missing", true);
    thread.send("second").unwrap();
    finish_turn(&mut thread);

    assert_eq!(fixture.spawns().len(), 3, "one retry only");
    assert_eq!(thread.status(), Status::Failed);
    let errors = errors(&thread);
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0].1.contains("No conversation found"),
        "{}",
        errors[0].1
    );
    assert_eq!(
        kinds(&thread)
            .iter()
            .filter(|kind| **kind == "fresh_start")
            .count(),
        1
    );
}

#[test]
fn the_retry_allowance_is_per_message() {
    // A repair on one message does not use up the next message's.
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    fixture.switch("resume_missing", true);
    thread.send("two").unwrap();
    finish_turn(&mut thread);
    // claude loses the new conversation too; the next message repairs that again.
    assert!(thread.idle_check(Instant::now() + IDLE_TIMEOUT));
    thread.send("three").unwrap();
    finish_turn(&mut thread);
    assert!(errors(&thread).is_empty(), "{:#?}", errors(&thread));
    assert_eq!(thread.status(), Status::Done);
    assert_eq!(
        kinds(&thread)
            .iter()
            .filter(|kind| **kind == "fresh_start")
            .count(),
        2
    );
}

/// Sends a message with the `ask` switch on and waits for the approval.
fn send_and_wait_for_approval(thread: &mut Thread, text: &str) {
    thread.send(text).unwrap();
    pump_until(thread, "the approval", |thread| {
        thread.transcript().pending_approval().is_some()
    });
    assert_eq!(thread.status(), Status::Waiting);
}

#[test]
fn always_allow_is_saved_and_given_to_every_later_claude_until_revoked() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "make a file");
    thread
        .answer(None, PermissionAnswer::AllowForSession)
        .unwrap();
    finish_turn(&mut thread);
    fixture.switch("ask", false);

    assert_eq!(thread.grants().len(), 1);
    assert_eq!(thread.grants()[0].rule, "Bash(touch one.txt)");
    assert_eq!(thread.grants()[0].label, "touch one.txt");
    assert!(fixture.store.flush(FLUSH));
    let saved = fixture.store.grants(thread.id()).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].rule, "Bash(touch one.txt)");
    // The live claude was told too, for this session only.
    let answer = fixture
        .stdin()
        .into_iter()
        .find(|line| line["type"] == "control_response")
        .unwrap();
    assert_eq!(
        answer["response"]["response"]["updatedPermissions"][0]["destination"],
        "session"
    );

    // A later claude for this thread (here after an idle shutdown) gets the rule at start.
    assert!(thread.idle_check(Instant::now() + IDLE_TIMEOUT));
    thread.send("again").unwrap();
    finish_turn(&mut thread);
    let spawns = fixture.spawns();
    assert_eq!(allowed_tools(&spawns[0]), Vec::<&str>::new());
    assert_eq!(allowed_tools(&spawns[1]), ["Bash(touch one.txt)"]);
    assert_eq!(flag(&spawns[1], "--resume"), Some(thread.session_id()));

    // Revoking while idle stops claude at once; the next one starts without the rule.
    assert!(thread.has_session());
    thread.revoke("Bash(touch one.txt)");
    assert!(!thread.has_session());
    assert!(thread.grants().is_empty());
    thread.send("third").unwrap();
    finish_turn(&mut thread);
    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 3);
    assert_eq!(allowed_tools(&spawns[2]), Vec::<&str>::new());
    assert!(fixture.store.flush(FLUSH));
    assert!(fixture.store.grants(thread.id()).unwrap().is_empty());

    // Grants survive a relaunch.
    thread.idle_check(Instant::now() + IDLE_TIMEOUT);
    fixture.switch("ask", true);
    send_and_wait_for_approval(&mut thread, "once more");
    thread
        .answer(None, PermissionAnswer::AllowForSession)
        .unwrap();
    finish_turn(&mut thread);
    let reloaded = fixture.reload(thread.id());
    assert_eq!(reloaded.grants().len(), 1);
    assert_eq!(
        reloaded.session_config().allowed_tools,
        ["Bash(touch one.txt)"]
    );
}

#[test]
fn allow_once_saves_no_grant() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "make a file");
    thread.answer(None, PermissionAnswer::AllowOnce).unwrap();
    finish_turn(&mut thread);
    assert!(thread.grants().is_empty());
    assert!(fixture.store.flush(FLUSH));
    assert!(fixture.store.grants(thread.id()).unwrap().is_empty());
}

#[test]
fn revoking_mid_turn_stops_claude_when_the_turn_ends() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "one");
    thread
        .answer(None, PermissionAnswer::AllowForSession)
        .unwrap();
    finish_turn(&mut thread);

    send_and_wait_for_approval(&mut thread, "two");
    thread.revoke("Bash(touch one.txt)");
    assert!(thread.has_session(), "never mid-turn");
    thread.answer(None, PermissionAnswer::AllowOnce).unwrap();
    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Done);
    assert!(!thread.has_session(), "stopped once the turn ended");
    assert!(errors(&thread).is_empty());
}

#[test]
fn idle_shutdown_waits_30_minutes_and_never_cuts_a_turn_short() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    let now = Instant::now();
    assert!(!thread.idle_check(now + IDLE_TIMEOUT - Duration::from_secs(60)));
    assert!(thread.has_session());
    assert!(thread.idle_check(now + IDLE_TIMEOUT));
    assert!(!thread.has_session());
    assert_eq!(thread.status(), Status::Done, "the status stays as it was");
    assert!(
        !thread.idle_check(now + IDLE_TIMEOUT * 2),
        "nothing left to stop"
    );
    // Nothing shows up later from the stopped process.
    std::thread::sleep(Duration::from_millis(100));
    while let Ok(event) = thread.events().try_recv() {
        thread.apply(event);
    }
    assert_eq!(kinds(&thread), ["user", "assistant", "turn_end"]);

    // With an approval waiting: never.
    fixture.switch("ask", true);
    send_and_wait_for_approval(&mut thread, "two");
    assert!(!thread.idle_check(Instant::now() + IDLE_TIMEOUT * 2));
    assert!(thread.has_session());
    thread.answer(None, PermissionAnswer::AllowOnce).unwrap();
    finish_turn(&mut thread);
    fixture.switch("ask", false);

    // Mid-turn: never.
    fixture.switch("hang", true);
    thread.send("three").unwrap();
    assert!(!thread.idle_check(Instant::now() + IDLE_TIMEOUT * 2));
    assert!(thread.has_session());
    thread.shutdown_for_quit();
}

#[test]
fn unread_is_set_off_screen_and_cleared_on_screen() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    // Not on screen: a finished turn makes it unread.
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    assert!(thread.is_unread());
    assert!(fixture.store.flush(FLUSH));
    assert!(fixture.store.thread(thread.id()).unwrap().unwrap().unread);

    thread.set_active(true);
    assert!(!thread.is_unread());
    assert!(fixture.store.flush(FLUSH));
    assert!(!fixture.store.thread(thread.id()).unwrap().unwrap().unread);

    // On screen: nothing becomes unread.
    thread.send("two").unwrap();
    finish_turn(&mut thread);
    assert!(!thread.is_unread());

    // Off screen again: an approval request makes it unread at once.
    thread.set_active(false);
    fixture.switch("ask", true);
    send_and_wait_for_approval(&mut thread, "three");
    assert!(thread.is_unread());
    thread.set_active(true);
    assert!(!thread.is_unread());
    thread.answer(None, PermissionAnswer::AllowOnce).unwrap();
    finish_turn(&mut thread);
    assert!(!thread.is_unread());
}

#[test]
fn a_thread_left_waiting_by_a_crash_is_stopped_with_its_approval_expired() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "make a file");
    // Newjen is killed here: no quit handling, only what was saved at the last checkpoint
    // (the approval request).
    let id = thread.id().to_string();
    let mut relaunched = fixture.reload(&id);
    drop(thread);

    assert_eq!(relaunched.status(), Status::Stopped);
    assert!(relaunched.transcript().pending_approval().is_none());
    assert_eq!(kinds(&relaunched), ["user", "tool", "stopped"]);
    assert!(matches!(
        relaunched.transcript().items()[1],
        Item::Tool {
            state: ToolState::Expired,
            ..
        }
    ));
    assert!(!relaunched.has_session(), "nothing restarts on its own");

    // Settled and saved: a second launch shows the same, with one notice.
    let again = fixture.reload(&id);
    assert_eq!(again.transcript().items(), relaunched.transcript().items());
    assert_eq!(again.status(), Status::Stopped);
    drop(again);

    // Continue is an ordinary message, sent to the resumed conversation.
    fixture.switch("ask", false);
    relaunched.send(CONTINUE_MESSAGE).unwrap();
    finish_turn(&mut relaunched);
    let spawns = fixture.spawns();
    assert_eq!(
        flag(spawns.last().unwrap(), "--resume"),
        Some(relaunched.session_id())
    );
    assert_eq!(relaunched.status(), Status::Done);
}

#[test]
fn quitting_mid_turn_saves_the_thread_as_stopped() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "make a file");
    let started = Instant::now();
    thread.shutdown_for_quit();
    assert!(started.elapsed() < Duration::from_millis(400));
    assert!(!thread.has_session());
    assert_eq!(thread.status(), Status::Stopped);

    // Already on disk (shutdown_for_quit waited for it), without another flush.
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.status, Status::Stopped);
    let relaunched = fixture.reload(thread.id());
    assert_eq!(kinds(&relaunched), ["user", "tool", "stopped"]);
    assert_eq!(relaunched.transcript().items(), thread.transcript().items());
}

#[test]
fn quitting_an_idle_thread_changes_nothing() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    let before = thread.transcript().items().to_vec();
    thread.shutdown_for_quit();
    assert!(!thread.has_session());
    let relaunched = fixture.reload(thread.id());
    assert_eq!(relaunched.transcript().items(), before.as_slice());
    assert_eq!(relaunched.status(), Status::Done);

    // A draft has nothing to save.
    let mut draft = fixture.draft();
    draft.shutdown_for_quit();
    assert!(fixture.store.flush(FLUSH));
    assert_eq!(fixture.store.threads(fixture.project.id).unwrap().len(), 1);
}

#[test]
fn a_thread_with_unreadable_rows_still_opens() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    let id = thread.id().to_string();
    drop(thread);
    // A newer Newjen added rows this version cannot read.
    fixture.store.replace_items(
        &id,
        3,
        vec![
            (1, r#"{"type":"hologram","id":40}"#.to_string()),
            (2, r#"{"type":"user","id":41,"text":"v2"}"#.to_string()),
            (1, "{broken".to_string()),
        ],
    );
    let mut loaded = fixture.reload(&id);
    assert_eq!(
        kinds(&loaded),
        [
            "user",
            "assistant",
            "turn_end",
            "unreadable",
            "unreadable",
            "unreadable"
        ]
    );
    // It keeps working, and saving never destroys the rows it could not read.
    loaded.send("two").unwrap();
    finish_turn(&mut loaded);
    assert!(fixture.store.flush(FLUSH));
    let saved = fixture.store.items(&id).unwrap();
    assert_eq!(saved[4].schema_version, 2);
    assert_eq!(saved[4].json, r#"{"type":"user","id":41,"text":"v2"}"#);
    assert_eq!(saved.len(), loaded.transcript().items().len());
}

#[test]
fn archiving_hides_the_thread_and_stops_claude() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    assert!(thread.has_session());
    thread.archive();
    assert!(!thread.has_session());
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    assert!(fixture.store.thread(thread.id()).unwrap().is_some());
}

#[test]
fn load_all_returns_threads_newest_first() {
    let fixture = Fixture::new();
    let mut ids = Vec::new();
    for text in ["first", "second"] {
        let mut thread = fixture.draft();
        thread.send(text).unwrap();
        finish_turn(&mut thread);
        ids.push(thread.id().to_string());
        // Creation times are in milliseconds; keep them apart.
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fixture.store.flush(FLUSH));
    let threads = Thread::load_all(&fixture.store, &fixture.project, &fixture.config).unwrap();
    let titles: Vec<&str> = threads.iter().map(Thread::title).collect();
    assert_eq!(titles, ["second", "first"]);
}

#[test]
fn the_session_config_names_the_project_folder() {
    let fixture = Fixture::new();
    let thread = fixture.draft();
    assert_eq!(thread.session_config().cwd, fixture.project.path);
    assert_eq!(thread.cwd(), Path::new(&fixture.project.path));
}

#[test]
fn try_again_when_the_conversation_is_gone_switches_to_a_new_one() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    let old_session = thread.session_id().to_string();
    // The turn fails for an unrelated reason...
    fixture.switch("crash", true);
    thread.send("second").unwrap();
    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Failed);
    fixture.switch("crash", false);

    // ...and by the time the user presses Try again, claude has lost the conversation too.
    fixture.switch("resume_missing", true);
    thread.restart().unwrap();
    pump_until(&mut thread, "the switch to a new conversation", |thread| {
        !thread.session_started()
    });
    assert_ne!(thread.session_id(), old_session);
    assert!(!thread.has_session());
    assert_eq!(
        errors(&thread).len(),
        1,
        "only the crash: {:#?}",
        errors(&thread)
    );
    assert_eq!(kinds(&thread).last(), Some(&"fresh_start"));
    settle(&mut thread);
    assert_eq!(fixture.spawns().len(), 3, "nothing was resent or retried");

    // The next message starts the new conversation, instead of failing the same way again.
    thread.send("third").unwrap();
    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Done);
    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 4);
    assert_eq!(flag(&spawns[2], "--resume"), Some(old_session.as_str()));
    assert_eq!(flag(&spawns[3], "--session-id"), Some(thread.session_id()));
    assert_eq!(fixture.messages_received(), ["first", "second", "third"]);
    assert_eq!(errors(&thread).len(), 1);
}

#[test]
fn stopping_while_claude_starts_cancels_the_in_use_repair() {
    let fixture = Fixture::new();
    fixture.switch("new_in_use", true);
    let mut thread = fixture.draft();
    thread.send("hello").unwrap();
    // Stop is pressed while claude is still starting up.
    fixture.wait_for_spawns(1);
    thread.stop().unwrap();
    assert_eq!(thread.status(), Status::Stopped, "stopped at once");
    assert!(!thread.has_session());
    // The process still says "already in use" and exits; nothing is started or sent again.
    settle(&mut thread);
    assert_eq!(fixture.spawns().len(), 1);
    assert!(fixture.messages_received().is_empty());
    assert_eq!(thread.status(), Status::Stopped);
    assert_eq!(kinds(&thread), ["user"]);
    assert!(errors(&thread).is_empty());
    assert_eq!(thread.turn_started_at(), None);

    // The next message repairs as usual.
    thread.send("again").unwrap();
    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Done);
    assert_eq!(fixture.messages_received(), ["again"]);
}

#[test]
fn stopping_while_claude_resumes_cancels_the_fresh_start() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    let session = thread.session_id().to_string();
    fixture.switch("resume_missing", true);
    thread.send("second").unwrap();
    fixture.wait_for_spawns(2);
    thread.stop().unwrap();
    assert_eq!(thread.status(), Status::Stopped);
    settle(&mut thread);
    assert_eq!(fixture.spawns().len(), 2, "first turn, the stopped resume");
    assert_eq!(fixture.messages_received(), ["first"]);
    assert_eq!(thread.status(), Status::Stopped);
    assert_eq!(kinds(&thread), ["user", "assistant", "turn_end", "user"]);
    assert_eq!(thread.session_id(), session, "nothing was learned about it");
    assert!(errors(&thread).is_empty());
}

#[test]
fn a_repair_that_cannot_start_claude_marks_a_thread_off_screen_unread() {
    let fixture = Fixture::new();
    fixture.switch("new_in_use", true);
    fixture.switch("vanish", true);
    let mut thread = fixture.draft();
    thread.send("hello").unwrap();
    finish_turn(&mut thread);

    assert_eq!(fixture.spawns().len(), 1);
    assert_eq!(thread.status(), Status::Failed);
    let errors = errors(&thread);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "Could not start Claude Code");
    assert!(
        thread.is_unread(),
        "the turn ended while the thread was not on screen"
    );
    assert_eq!(thread.turn_started_at(), None);
    assert!(!thread.has_session());
}

#[test]
fn another_already_in_use_error_is_shown_as_it_is() {
    let fixture = Fixture::new();
    fixture.switch("other_in_use", true);
    let mut thread = fixture.draft();
    thread.send("hello").unwrap();
    finish_turn(&mut thread);

    assert_eq!(fixture.spawns().len(), 1, "no repair");
    assert!(!thread.session_started());
    assert_eq!(thread.status(), Status::Failed);
    let errors = errors(&thread);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, "Claude exited");
    assert!(errors[0].1.contains("EADDRINUSE"), "{}", errors[0].1);
    assert!(!kinds(&thread).contains(&"fresh_start"));
}

#[test]
fn quitting_while_claude_works_saves_the_thread_as_stopped() {
    let fixture = Fixture::new();
    fixture.switch("hang", true);
    let mut thread = fixture.draft();
    thread.send("go").unwrap();
    pump_until(
        &mut thread,
        "the tool call and the streamed reply",
        |thread| kinds(thread) == ["user", "tool", "assistant"],
    );
    assert_eq!(thread.status(), Status::Working);
    assert!(thread.transcript().pending_approval().is_none());

    thread.shutdown_for_quit();
    assert!(!thread.has_session());
    assert_eq!(thread.status(), Status::Stopped);
    assert_eq!(kinds(&thread), ["user", "tool", "assistant", "stopped"]);
    let items = thread.transcript().items();
    assert!(matches!(
        &items[1],
        Item::Tool { state: ToolState::Failed, output: Some(note), .. } if note == STOPPED_TOOL_NOTE
    ));
    assert!(matches!(
        &items[2],
        Item::Assistant { live: false, interrupted: true, text, .. } if text == "Still going"
    ));

    // Already on disk (shutdown_for_quit waited for it), without another flush.
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.status, Status::Stopped);
    let relaunched = fixture.reload(thread.id());
    assert_eq!(relaunched.transcript().items(), thread.transcript().items());
    assert_eq!(relaunched.status(), Status::Stopped);
}

#[test]
fn quitting_many_threads_stops_their_claudes_together() {
    let fixture = Fixture::new();
    fixture.switch("hang", true);
    // Each claude takes the whole grace period to stop.
    fixture.switch("slow_stop", true);
    let mut threads: Vec<Thread> = (0..5).map(|_| fixture.draft()).collect();
    for thread in &mut threads {
        thread.send("go").unwrap();
    }
    fixture.wait_for_messages(5);

    let started = Instant::now();
    let quitting: Vec<_> = threads.iter_mut().map(Thread::begin_quit).collect();
    finish_quit(&fixture.store, quitting);
    let took = started.elapsed();
    // One after another this would take at least 5 × 140 ms.
    assert!(took < Duration::from_millis(450), "took {took:?}");
    for thread in &threads {
        assert!(!thread.has_session());
        let row = fixture.store.thread(thread.id()).unwrap().unwrap();
        assert_eq!(row.status, Status::Stopped);
    }
}

#[test]
fn quitting_waits_for_a_write_queued_just_before() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    assert!(thread.is_unread(), "the turn ended off screen");
    assert!(fixture.store.flush(FLUSH));

    // Hold the database, so the next write is still queued at quit.
    let database = fixture.database();
    database.execute_batch("BEGIN IMMEDIATE;").unwrap();
    thread.set_active(true);
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        database.execute_batch("COMMIT;").unwrap();
    });
    // Nothing new to save at quit, but it still waits for what is queued.
    thread.shutdown_for_quit();
    release.join().unwrap();
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert!(!row.unread);
}

#[test]
fn a_failed_write_is_written_again_and_loses_nothing_else() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    assert!(fixture.store.flush(FLUSH));

    // The disk refuses the write with message "two" (as a full disk or a long lock would).
    let database = fixture.database();
    database
        .execute_batch(
            r#"CREATE TRIGGER refuse BEFORE INSERT ON thread_items
                   WHEN NEW.json LIKE '%"text":"two"%'
                   BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;"#,
        )
        .unwrap();
    thread.send("two").unwrap();
    assert!(fixture.store.flush(FLUSH));
    let error = fixture
        .store
        .write_errors()
        .try_recv()
        .expect("the failed write is reported");
    assert!(error.to_string().contains("disk I/O error"), "{error}");
    database.execute_batch("DROP TRIGGER refuse;").unwrap();

    // The next checkpoint writes it again, so the saved rows have no gap.
    finish_turn(&mut thread);
    assert!(fixture.store.flush(FLUSH));
    let positions: Vec<usize> = fixture
        .store
        .items(thread.id())
        .unwrap()
        .iter()
        .map(|item| item.ix)
        .collect();
    assert_eq!(
        positions,
        (0..thread.transcript().items().len()).collect::<Vec<_>>()
    );
    let mut reloaded = fixture.reload(thread.id());
    assert_eq!(reloaded.transcript().items(), thread.transcript().items());
    drop(thread);

    // Later saves still replace the right rows.
    reloaded.send("three").unwrap();
    finish_turn(&mut reloaded);
    let again = fixture.reload(reloaded.id());
    assert_eq!(again.transcript().items(), reloaded.transcript().items());
    assert_eq!(user_texts(&again), ["one", "two", "three"]);
}

#[test]
fn a_gap_left_in_the_saved_rows_is_closed_when_the_thread_loads() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.send("one").unwrap();
    finish_turn(&mut thread);
    thread.send("two").unwrap();
    finish_turn(&mut thread);
    let id = thread.id().to_string();
    drop(thread);
    // An earlier run lost the row with message "two" to a failed write.
    assert!(fixture.store.flush(FLUSH));
    fixture
        .database()
        .execute(
            "DELETE FROM thread_items WHERE thread_id = ?1 AND ix = 3",
            [&id],
        )
        .unwrap();

    let mut loaded = fixture.reload(&id);
    assert_eq!(
        kinds(&loaded),
        ["user", "assistant", "turn_end", "assistant", "turn_end"]
    );
    // Saved again where the rows now are.
    assert!(fixture.store.flush(FLUSH));
    let positions: Vec<usize> = fixture
        .store
        .items(&id)
        .unwrap()
        .iter()
        .map(|item| item.ix)
        .collect();
    assert_eq!(positions, [0, 1, 2, 3, 4]);

    // A later save replaces the right rows: nothing on screen disappears.
    loaded.send("three").unwrap();
    finish_turn(&mut loaded);
    let again = fixture.reload(&id);
    assert_eq!(again.transcript().items(), loaded.transcript().items());
    assert_eq!(user_texts(&again), ["one", "three"]);
}

#[test]
fn a_thread_always_restarts_from_its_own_folder() {
    // Claude ties `--resume` to the folder a conversation started in, so a thread whose
    // folder differs from the project's (its own Rift copy, later) must come back to it.
    let home = tempfile::tempdir().unwrap();
    let project_dir = tempfile::tempdir().unwrap();
    let copy_dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(home.path()).unwrap();
    let project = store.open_project(project_dir.path()).unwrap();
    store.save_thread(newjen_core::store::ThreadRow {
        id: "t1".to_string(),
        project_id: project.id,
        workdir: copy_dir.path().to_path_buf(),
        title: "In a copy".to_string(),
        provider: "claude".to_string(),
        session_id: "6f1c1f4e-2b1e-4c8e-9a51-0d9a7c2f3b10".to_string(),
        session_started: true,
        status: Status::Done,
        unread: false,
        created_at: 1,
        updated_at: 1,
        archived_at: None,
    });
    assert!(store.flush(Duration::from_secs(5)));

    let row = store.thread("t1").unwrap().expect("saved thread");
    assert_eq!(row.workdir, copy_dir.path());
    let thread = Thread::load(
        store.clone(),
        &project,
        row,
        SessionConfig::new(project_dir.path()),
    )
    .unwrap();
    assert_eq!(thread.cwd(), copy_dir.path());
    assert_eq!(thread.session_config().cwd, copy_dir.path());
}
