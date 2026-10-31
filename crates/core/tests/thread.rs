//! Runs real `Thread`s against a shell-script stand-in for claude, with a real store in a temp
//! folder. No network, no Claude login.
//!
//! The stand-in (`STAND_IN`) records the arguments of every start in `spawns.log`, the folder
//! it started in with its `RIFT_ID` and `RIFT_SOURCE` in `cwd.log`, and every stdin line in
//! `stdin.log`. Empty files in its folder switch on odd behaviour, so a test can
//! change it between turns:
//! - `new_in_use` / `resume_in_use`: with `--session-id` / `--resume`, print claude's
//!   "already in use" error on stderr and exit 1, without reading stdin.
//! - `new_missing` / `resume_missing`: print claude's "No conversation found" result and exit
//!   1, without reading stdin. claude 2.1.314 checks `--resume` as it starts, before it reads
//!   a message (checked live), so this also happens with no message at all.
//! - `resume_missing_other`: like `resume_missing`, but the error names another session id.
//! - `other_in_use`: print another error that says "already in use" (a busy port) on stderr
//!   and exit 1, without reading stdin.
//! - `crash`: read a message, then exit 1 without answering.
//! - `never_init`: read a message, then hang without ever sending `init`, as a claude still
//!   starting up would.
//! - `no_init`: never send `init`, but otherwise run turns as usual.
//! - `hang_after_init`: read a message, send `init`, then hang without any reply.
//! - `missing_after_init`: end each turn right after its `init` with the "No conversation
//!   found" result for this session, and keep running.
//! - `vanish`: delete the stand-in as it starts, so the next start fails.
//! - `ask`: ask to approve `touch one.txt` (with the CLI's usual suggestion) before replying.
//!   With `exit_on_ask` too, exit 1 right after asking.
//! - `hang`: start the turn (a running `sleep` tool call and a reply being streamed) and never
//!   finish it.
//! - `slow_stop`: ignore SIGTERM, as a claude busy with its exit hooks might, so stopping it
//!   takes the whole grace period before SIGKILL.
//! - `background`: run `npm run dev` in the background before replying, as claude 2.1.314
//!   does (see the `background_task` fixture). The task runs until a `finish_task` file
//!   appears; then claude reports it finished and starts a turn by itself, with no message.
//!   With `own_turn_missing`, that turn ends with the "No conversation found" result for this
//!   session.
//! - `queued_own_turn`: on reading a message, first run the turn claude starts by itself when
//!   that task finishes, as if the message arrived just after the task did. That turn's
//!   result says the message waits for the next turn (`queued_turn_count: 1`).
//! - `fail_turn`: end each turn with an error result instead of success.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use newjen_core::ApprovalIntent;
use newjen_core::rift::{Copy, CopyError, Tools};
use newjen_core::session::{SessionConfig, SessionEvent};
use newjen_core::store::{Project, Store};
use newjen_core::thread::{
    CONTINUE_MESSAGE, IDLE_TIMEOUT, NEW_THREAD_TITLE, Removal, Sent, Thread, WorksIn, finish_quit,
};
use newjen_core::transcript::{
    BACKGROUND_STOPPED_NOTE, Item, STOPPED_TOOL_NOTE, Status, ToolState,
};
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(10);
const FLUSH: Duration = Duration::from_secs(5);

const STAND_IN: &str = r##"#!/bin/sh
dir="$FAKE_DIR"
printf '%s|%s|%s\n' "$(pwd -P)" "${RIFT_ID:-}" "${RIFT_SOURCE:-}" >> "$dir/cwd.log"
{ for arg in "$@"; do printf '%s\n' "$arg"; done; echo "---"; } >> "$dir/spawns.log"
if [ -f "$dir/close_stdin" ]; then
  exec 0<&-
  while [ ! -f "$dir/flush_write_failure" ]; do sleep 0.01; done
  if [ -f "$dir/flush_with_init" ]; then
    echo '{"type":"system","subtype":"init"}'
  else
    echo "warning: slow start" >&2
  fi
  while :; do sleep 1; done
fi
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
missing='{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: '"$sid"'"]}'
if [ -f "$dir/${mode}_missing" ]; then
  sleep 0.2
  printf '%s\n' "$missing"
  exit 1
fi
if [ -f "$dir/${mode}_missing_other" ]; then
  sleep 0.2
  printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: 11111111-0000-4000-8000-000000000000"]}'
  exit 1
fi
send_init() {
  if [ ! -f "$dir/no_init" ]; then
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"'"$sid"'"}'
  fi
}
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/stdin.log"
  if [ -f "$dir/crash" ]; then
    exit 1
  fi
  if [ -f "$dir/never_init" ]; then
    exec sleep 60
  fi
  if [ -f "$dir/queued_own_turn" ]; then
    printf '%s\n' '{"type":"system","subtype":"task_notification","task_id":"b1","run_id":"r1","tool_use_id":"tb","status":"completed","summary":"Background command completed (exit code 0)"}'
    printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"'"$sid"'"}'
    printf '%s\n' '{"type":"assistant","message":{"id":"mc","content":[{"type":"text","text":"The dev server stopped."}]}}'
    printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":5,"queued_turn_count":1,"origin":{"kind":"task-notification","producer":"session-task","runId":"r1"}}'
  fi
  send_init
  if [ -f "$dir/hang_after_init" ]; then
    exec sleep 60
  fi
  if [ -f "$dir/missing_after_init" ]; then
    printf '%s\n' "$missing"
    continue
  fi
  if [ -f "$dir/hang" ]; then
    printf '%s\n' '{"type":"assistant","message":{"id":"m0","content":[{"type":"tool_use","id":"t0","name":"Bash","input":{"command":"sleep 60"}}]}}'
    printf '%s\n' '{"type":"stream_event","api_message_id":"m1","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Still going"}}}'
    exec sleep 60
  fi
  if [ -f "$dir/background" ]; then
    printf '%s\n' '{"type":"assistant","message":{"id":"mb","content":[{"type":"tool_use","id":"tb","name":"Bash","input":{"command":"npm run dev","run_in_background":true}}]}}'
    printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[{"task_id":"b1","run_id":"r1","task_type":"local_bash","description":"Start the dev server"}]}'
    printf '%s\n' '{"type":"system","subtype":"task_started","task_id":"b1","run_id":"r1","tool_use_id":"tb","description":"Start the dev server","is_backgrounded":true,"task_type":"local_bash"}'
    printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tb","content":"Command running in background with ID: b1.","is_error":false}]}}'
    (
      while [ ! -f "$dir/finish_task" ]; do sleep 0.05; done
      rm -f "$dir/finish_task"
      printf '%s\n' '{"type":"system","subtype":"task_updated","task_id":"b1","run_id":"r1","patch":{"status":"completed"}}'
      printf '%s\n' '{"type":"system","subtype":"task_notification","task_id":"b1","run_id":"r1","tool_use_id":"tb","status":"completed","summary":"Background command completed (exit code 0)"}'
      printf '%s\n' '{"type":"system","subtype":"background_tasks_changed","tasks":[]}'
      send_init
      printf '%s\n' '{"type":"assistant","message":{"id":"mc","content":[{"type":"text","text":"The dev server stopped."}]}}'
      if [ -f "$dir/own_turn_missing" ]; then
        printf '%s\n' "$missing"
      else
        printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":5}'
      fi
    ) &
  fi
  if [ -f "$dir/ask" ]; then
    printf '%s\n' '{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"t1","input":{"command":"touch one.txt"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"touch one.txt"}],"behavior":"allow","destination":"localSettings"}]}}'
    if [ -f "$dir/exit_on_ask" ]; then
      exit 1
    fi
    IFS= read -r answer
    printf '%s\n' "$answer" >> "$dir/stdin.log"
    printf '%s\n' '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"","is_error":false}]}}'
  fi
  printf '%s\n' '{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"done"}]}}'
  if [ -f "$dir/fail_turn" ]; then
    printf '%s\n' '{"type":"result","subtype":"success","is_error":true,"result":"Overloaded","duration_ms":5}'
  else
    printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"duration_ms":5}'
  fi
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

    /// Where each start of the stand-in ran, with the `RIFT_ID` and `RIFT_SOURCE` it saw, as
    /// `folder|id|source`.
    fn cwds(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("cwd.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A folder standing in for a Rift copy of the project, as `rift::create` would report it.
    fn fake_copy(&self) -> Copy {
        let path = self.path("copies/newjen-test");
        std::fs::create_dir_all(&path).unwrap();
        let path = std::fs::canonicalize(path).unwrap();
        Copy {
            workdir: path.clone(),
            path,
            source: std::fs::canonicalize(&self.project.path).unwrap(),
            rift_id: "01TESTCOPY".to_string(),
            base: None,
        }
    }

    /// A draft that wants a Rift copy, with its first message sent and held.
    fn preparing(&self, text: &str) -> (Thread, newjen_core::thread::CopyJob) {
        let mut thread = self.draft();
        thread.set_works_in(WorksIn::RiftCopy).unwrap();
        let Sent::MakingCopy(job) = thread.send(text).unwrap() else {
            panic!("the message should wait for the copy");
        };
        (thread, job)
    }

    /// The installed `rift` and `jj` with a `HOME` of their own (so a Rift registry of their
    /// own), or `None`, with the reason printed, when they are not installed.
    fn rift_tools(&self) -> Option<Tools> {
        let mut tools = Tools::find();
        if !tools.rift.is_absolute() || !tools.jj.is_absolute() {
            eprintln!("skipped: `rift` or `jj` is not installed");
            return None;
        }
        let home = self.path("rift-home");
        std::fs::create_dir_all(&home).unwrap();
        tools.env = vec![("HOME".to_string(), home.display().to_string())];
        Some(tools)
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
    assert!(matches!(
        rows[0].conversation,
        newjen_core::agent::ConversationState::Established(_)
    ));
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
    // The divider comes before the message that starts the new conversation.
    assert_eq!(
        kinds(&thread),
        [
            "user",
            "assistant",
            "turn_end",
            "fresh_start",
            "user",
            "assistant",
            "turn_end"
        ]
    );
    assert_eq!(user_texts(&thread), ["first", "second"]);
    assert!(errors(&thread).is_empty());
    let reloaded = fixture.reload(thread.id());
    assert_eq!(reloaded.transcript().items(), thread.transcript().items());

    // The new conversation is saved, so the next launch resumes it.
    assert!(fixture.store.flush(FLUSH));
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.conversation.id(), Some(new_session));
    assert!(matches!(
        row.conversation,
        newjen_core::agent::ConversationState::Established(_)
    ));
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
fn a_missing_conversation_error_after_init_is_shown_and_nothing_is_resent() {
    let fixture = Fixture::new();
    fixture.switch("missing_after_init", true);
    let mut thread = fixture.draft();
    thread.send("hello").unwrap();
    finish_turn(&mut thread);
    settle(&mut thread);

    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 1, "no new claude");
    assert_eq!(flag(&spawns[0], "--session-id"), Some(thread.session_id()));
    assert_eq!(fixture.messages_received(), ["hello"], "nothing resent");
    assert!(thread.session_started());
    assert_eq!(thread.status(), Status::Failed);
    assert!(errors(&thread)[0].1.contains("No conversation found"));
    assert!(!kinds(&thread).contains(&"fresh_start"));
}

/// A started thread whose next turn leaves `npm run dev` running, and whose turn claude starts
/// by itself when it finishes ends with "No conversation found" for this conversation. With
/// `init` false the stand-in never sends `system/init`.
fn missing_conversation_in_claudes_own_turn(fixture: &Fixture, init: bool) -> Thread {
    let mut thread = started_thread(fixture);
    fixture.switch("no_init", !init);
    fixture.switch("background", true);
    fixture.switch("own_turn_missing", true);
    thread.send("Start the dev server").unwrap();
    finish_turn(&mut thread);
    fixture.switch("background", false);
    assert_eq!(thread.status(), Status::Done);
    let before = kinds(&thread).len();

    fixture.switch("finish_task", true);
    pump_until(&mut thread, "claude's own turn to end", |thread| {
        kinds(thread).len() > before + 1 && !thread.status().is_busy()
    });
    settle(&mut thread);
    assert_eq!(fixture.spawns().len(), 2, "no new claude");
    assert_eq!(
        fixture.messages_received(),
        ["first", "Start the dev server"],
        "nothing resent"
    );
    assert_eq!(user_texts(&thread), ["first", "Start the dev server"]);
    thread
}

#[test]
fn a_missing_conversation_error_in_claudes_own_turn_is_shown_and_nothing_is_resent() {
    let fixture = Fixture::new();
    let thread = missing_conversation_in_claudes_own_turn(&fixture, true);
    assert_eq!(
        flag(&fixture.spawns()[1], "--resume"),
        Some(thread.session_id())
    );
    assert!(thread.session_started());
    assert_eq!(thread.status(), Status::Failed);
    assert!(!kinds(&thread).contains(&"fresh_start"));
}

#[test]
fn a_finished_turns_message_is_never_resent_even_without_init() {
    let fixture = Fixture::new();
    missing_conversation_in_claudes_own_turn(&fixture, false);
}

#[test]
fn a_missing_conversation_error_for_another_id_is_shown_as_it_is() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    let session = thread.session_id().to_string();
    fixture.switch("resume_missing_other", true);
    thread.send("second").unwrap();
    finish_turn(&mut thread);
    settle(&mut thread);

    assert_eq!(fixture.spawns().len(), 2, "no repair");
    assert_eq!(fixture.messages_received(), ["first"]);
    assert_eq!(thread.session_id(), session);
    assert!(thread.session_started());
    assert_eq!(thread.status(), Status::Failed);
    assert!(
        errors(&thread)[0]
            .1
            .contains("11111111-0000-4000-8000-000000000000")
    );
    assert!(!kinds(&thread).contains(&"fresh_start"));
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
        .answer(None, ApprovalIntent::AllowForSession)
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
    thread.revoke(
        &thread
            .grant_views()
            .into_iter()
            .find(|grant| grant.shown == "touch one.txt")
            .unwrap()
            .id,
    );
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
        .answer(None, ApprovalIntent::AllowForSession)
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
    thread.answer(None, ApprovalIntent::AllowOnce).unwrap();
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
        .answer(None, ApprovalIntent::AllowForSession)
        .unwrap();
    finish_turn(&mut thread);

    send_and_wait_for_approval(&mut thread, "two");
    thread.revoke(
        &thread
            .grant_views()
            .into_iter()
            .find(|grant| grant.shown == "touch one.txt")
            .unwrap()
            .id,
    );
    assert!(thread.has_session(), "never mid-turn");
    thread.answer(None, ApprovalIntent::AllowOnce).unwrap();
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
    thread.answer(None, ApprovalIntent::AllowOnce).unwrap();
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
    thread.answer(None, ApprovalIntent::AllowOnce).unwrap();
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
    assert!(!relaunched.continue_resends());
    assert_eq!(relaunched.continue_text(), CONTINUE_MESSAGE);
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
    assert_eq!(thread.status(), Status::Stopped, "usable, not failed");
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
fn a_flushed_write_failure_ends_the_session_before_the_next_send() {
    for flush_with_init in [false, true] {
        let fixture = Fixture::new();
        fixture.switch("close_stdin", true);
        fixture.switch("flush_with_init", flush_with_init);
        let mut thread = fixture.draft();
        thread.set_active(true);
        thread.send(&"lost message".repeat(100_000)).unwrap();

        let events = thread.events();
        let deadline = Instant::now() + TIMEOUT;
        let failure = loop {
            match events.try_recv() {
                Ok(event) => break event,
                Err(async_channel::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "stdin write did not fail");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(async_channel::TryRecvError::Closed) => panic!("event channel closed"),
            }
        };
        assert!(matches!(failure.event(), SessionEvent::WriteFailed(_)));
        thread.apply(failure);
        assert!(thread.status().is_busy(), "the startup failure is held");
        assert!(thread.has_session());

        fixture.switch("close_stdin", false);
        fixture.switch("flush_write_failure", true);
        finish_turn(&mut thread);
        assert_eq!(thread.status(), Status::Failed);
        assert!(!thread.has_session());
        assert_eq!(errors(&thread).len(), 1);
        assert_eq!(errors(&thread)[0].0, "Could not reach Claude");
        assert_eq!(thread.turn_started_at(), None);
        assert!(fixture.store.flush(FLUSH));
        assert_eq!(
            fixture.store.thread(thread.id()).unwrap().unwrap().status,
            Status::Failed
        );
        settle(&mut thread);
        assert_eq!(fixture.spawns().len(), 1, "no automatic restart");
        assert!(fixture.messages_received().is_empty(), "nothing was resent");

        thread.send("next explicit message").unwrap();
        finish_turn(&mut thread);
        assert_eq!(thread.status(), Status::Done);
        assert_eq!(fixture.spawns().len(), 2);
        assert_eq!(fixture.messages_received(), ["next explicit message"]);
        assert_eq!(errors(&thread).len(), 1);
    }
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
        conversation: newjen_core::agent::ConversationState::Established(
            "6f1c1f4e-2b1e-4c8e-9a51-0d9a7c2f3b10".into(),
        ),
        model: newjen_core::providers::ModelSelection::CliDefault,
        effort: newjen_core::agent::EffortSelection::CliDefault,
        speed: newjen_core::agent::Speed::Normal,
        status: Status::Done,
        unread: false,
        created_at: 1,
        updated_at: 1,
        archived_at: None,
        copy: None,
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

/// A thread whose first turn left `npm run dev` running in the background (the stand-in's
/// `background` switch), with that turn over.
fn thread_with_a_background_task(fixture: &Fixture) -> Thread {
    fixture.switch("background", true);
    let mut thread = fixture.draft();
    thread.send("Start the dev server").unwrap();
    finish_turn(&mut thread);
    fixture.switch("background", false);
    assert_eq!(thread.status(), Status::Done);
    let descriptions: Vec<&str> = thread
        .background_tasks()
        .iter()
        .map(|task| task.description.as_str())
        .collect();
    assert_eq!(descriptions, ["Start the dev server"]);
    thread
}

/// Ends the stand-in's background task and feeds the thread everything up to the end of the
/// turn claude then starts by itself.
fn finish_the_background_task(thread: &mut Thread, fixture: &Fixture) {
    fixture.switch("finish_task", true);
    pump_until(thread, "claude's own turn to end", |thread| {
        kinds(thread)
            .iter()
            .filter(|kind| **kind == "turn_end")
            .count()
            == 2
    });
}

#[test]
fn idle_shutdown_waits_until_background_tasks_are_done() {
    let fixture = Fixture::new();
    let mut thread = thread_with_a_background_task(&fixture);
    // A dev server can run for hours with nothing to say: stopping claude would stop it.
    assert!(!thread.idle_check(Instant::now() + IDLE_TIMEOUT * 4));
    assert!(thread.has_session());

    finish_the_background_task(&mut thread, &fixture);
    assert!(thread.background_tasks().is_empty());
    assert!(!thread.idle_check(Instant::now() + IDLE_TIMEOUT - Duration::from_secs(60)));
    assert!(thread.idle_check(Instant::now() + IDLE_TIMEOUT));
    assert!(!thread.has_session());
    assert_eq!(thread.status(), Status::Done);
}

#[test]
fn a_turn_claude_starts_by_itself_is_a_turn_like_any_other() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.set_active(true);
    fixture.switch("background", true);
    thread.send("Start the dev server").unwrap();
    finish_turn(&mut thread);
    fixture.switch("background", false);
    assert!(!thread.is_unread());
    // The user moves on to another thread while the task runs.
    thread.set_active(false);

    fixture.switch("finish_task", true);
    pump_until(&mut thread, "claude's own turn to start", |thread| {
        thread.status() == Status::Working
    });
    assert!(thread.turn_started_at().is_some());
    assert!(fixture.store.flush(FLUSH));
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.status, Status::Working, "saved when the turn started");

    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Done);
    assert_eq!(thread.turn_started_at(), None);
    assert!(thread.is_unread(), "its turn ended off screen");
    assert_eq!(
        kinds(&thread),
        [
            "user",
            "tool",
            "assistant",
            "turn_end",
            "assistant",
            "turn_end"
        ]
    );
    assert_eq!(
        user_texts(&thread),
        ["Start the dev server"],
        "no message made up"
    );
    assert_eq!(fixture.messages_received(), ["Start the dev server"]);

    let reloaded = fixture.reload(thread.id());
    assert_eq!(reloaded.transcript().items(), thread.transcript().items());
    assert_eq!(reloaded.status(), Status::Done);
    assert!(reloaded.is_unread());
}

#[test]
fn quitting_while_a_background_task_runs_says_it_was_stopped() {
    let fixture = Fixture::new();
    let mut thread = thread_with_a_background_task(&fixture);
    thread.shutdown_for_quit();
    assert!(!thread.has_session());
    assert!(thread.background_tasks().is_empty());
    assert_eq!(thread.status(), Status::Stopped);
    assert_eq!(
        kinds(&thread),
        ["user", "tool", "assistant", "turn_end", "stopped"]
    );

    // Already on disk (shutdown_for_quit waited for it), and the same after a relaunch.
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.status, Status::Stopped);
    let mut relaunched = fixture.reload(thread.id());
    assert_eq!(relaunched.transcript().items(), thread.transcript().items());
    assert_eq!(relaunched.status(), Status::Stopped);

    // Continue resumes the conversation, as after any other quit.
    relaunched.send(CONTINUE_MESSAGE).unwrap();
    finish_turn(&mut relaunched);
    assert_eq!(relaunched.status(), Status::Done);
    assert_eq!(
        flag(fixture.spawns().last().unwrap(), "--resume"),
        Some(relaunched.session_id())
    );
}

#[test]
fn a_crash_while_a_background_task_runs_is_stopped_at_the_relaunch() {
    let fixture = Fixture::new();
    let thread = thread_with_a_background_task(&fixture);
    // Newjen is killed here: no quit handling, only what the checkpoints saved.
    let relaunched = fixture.reload(thread.id());
    drop(thread);
    assert_eq!(relaunched.status(), Status::Stopped);
    assert_eq!(
        kinds(&relaunched),
        ["user", "tool", "assistant", "turn_end", "stopped"]
    );
    assert!(relaunched.background_tasks().is_empty());
    assert!(!relaunched.has_session(), "nothing restarts on its own");

    // Settled and saved: a second launch shows one notice, not two.
    let again = fixture.reload(relaunched.id());
    assert_eq!(again.transcript().items(), relaunched.transcript().items());
}

#[test]
fn a_finished_background_task_leaves_nothing_to_stop_at_quit() {
    let fixture = Fixture::new();
    let mut thread = thread_with_a_background_task(&fixture);
    finish_the_background_task(&mut thread, &fixture);
    thread.shutdown_for_quit();
    assert_eq!(thread.status(), Status::Done);
    let relaunched = fixture.reload(thread.id());
    assert_eq!(relaunched.status(), Status::Done);
    assert!(!kinds(&relaunched).contains(&"stopped"));
}

#[test]
fn a_message_sent_just_as_claude_starts_a_turn_by_itself_waits_for_its_own_turn() {
    let fixture = Fixture::new();
    let mut thread = thread_with_a_background_task(&fixture);
    // Seen, and then the user moved on to another thread.
    thread.set_active(true);
    thread.set_active(false);
    // The task has just finished, but the thread has not seen claude's own turn start yet.
    fixture.switch("queued_own_turn", true);
    thread.send("What next?").unwrap();

    // claude answers about the task first; the user's message is still waiting, so the turn
    // is not over: no Done, no unread, the composer stays locked.
    let events = thread.events();
    let deadline = Instant::now() + TIMEOUT;
    while turn_ends(&thread) < 3 {
        assert!(
            thread.status().is_busy(),
            "{:#?}",
            thread.transcript().items()
        );
        assert!(!thread.is_unread());
        assert!(thread.turn_started_at().is_some());
        match events.try_recv() {
            Ok(event) => thread.apply(event),
            Err(_) => {
                assert!(Instant::now() < deadline, "timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    assert_eq!(thread.status(), Status::Done);
    assert!(thread.is_unread(), "the user's turn ended off screen");
    assert_eq!(
        kinds(&thread),
        [
            "user",
            "tool",
            "assistant",
            "turn_end",
            "user",
            "assistant",
            "turn_end",
            "assistant",
            "turn_end"
        ]
    );
    assert_eq!(user_texts(&thread), ["Start the dev server", "What next?"]);
    assert_eq!(
        fixture.messages_received(),
        ["Start the dev server", "What next?"]
    );
    let reloaded = fixture.reload(thread.id());
    assert_eq!(reloaded.status(), Status::Done);
}

fn turn_ends(thread: &Thread) -> usize {
    kinds(thread)
        .iter()
        .filter(|kind| **kind == "turn_end")
        .count()
}

/// The card of the stand-in's `npm run dev` call: its state, output and background flag.
fn background_card(thread: &Thread) -> (ToolState, Option<String>, bool) {
    thread
        .transcript()
        .items()
        .iter()
        .find_map(|item| match item {
            Item::Tool {
                tool_use_id,
                state,
                output,
                background,
                ..
            } if tool_use_id == "tb" => Some((*state, output.clone(), *background)),
            _ => None,
        })
        .expect("the npm run dev card")
}

/// Its card says Newjen stopped the command, and so does the saved thread, with no "Stopped
/// when Newjen quit" notice: quitting did not stop it.
fn assert_background_stopped(fixture: &Fixture, thread: &Thread, status: Status) {
    assert!(thread.background_tasks().is_empty());
    assert!(!thread.has_session());
    assert_eq!(
        background_card(thread),
        (
            ToolState::Failed,
            Some(BACKGROUND_STOPPED_NOTE.to_string()),
            false
        )
    );
    assert_eq!(thread.status(), status);
    // Newjen is killed here: only what was saved so far counts.
    let relaunched = fixture.reload(thread.id());
    assert_eq!(relaunched.transcript().items(), thread.transcript().items());
    assert_eq!(relaunched.status(), status);
    assert!(!kinds(&relaunched).contains(&"stopped"));
}

/// A thread with an "Always allow" grant whose last turn left `npm run dev` running.
fn thread_with_a_grant_and_a_background_task(fixture: &Fixture) -> Thread {
    fixture.switch("ask", true);
    fixture.switch("background", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "Start the dev server and make a file");
    thread
        .answer(None, ApprovalIntent::AllowForSession)
        .unwrap();
    finish_turn(&mut thread);
    fixture.switch("ask", false);
    fixture.switch("background", false);
    assert_eq!(thread.grants().len(), 1);
    assert_eq!(thread.background_tasks().len(), 1);
    thread
}

#[test]
fn revoking_a_grant_says_it_stopped_the_background_task() {
    let fixture = Fixture::new();
    let mut thread = thread_with_a_grant_and_a_background_task(&fixture);
    thread.revoke(
        &thread
            .grant_views()
            .into_iter()
            .find(|grant| grant.shown == "touch one.txt")
            .unwrap()
            .id,
    );
    assert_background_stopped(&fixture, &thread, Status::Done);
}

#[test]
fn revoking_mid_turn_says_it_stopped_the_background_task_when_the_turn_ends() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "make a file");
    thread
        .answer(None, ApprovalIntent::AllowForSession)
        .unwrap();
    finish_turn(&mut thread);

    fixture.switch("background", true);
    send_and_wait_for_approval(&mut thread, "Start the dev server and make a file");
    assert_eq!(thread.background_tasks().len(), 1);
    thread.revoke(
        &thread
            .grant_views()
            .into_iter()
            .find(|grant| grant.shown == "touch one.txt")
            .unwrap()
            .id,
    );
    thread.answer(None, ApprovalIntent::AllowOnce).unwrap();
    finish_turn(&mut thread);
    assert_background_stopped(&fixture, &thread, Status::Done);
}

#[test]
fn try_again_says_it_stopped_the_background_task() {
    let fixture = Fixture::new();
    fixture.switch("background", true);
    fixture.switch("fail_turn", true);
    let mut thread = fixture.draft();
    thread.send("Start the dev server").unwrap();
    finish_turn(&mut thread);
    fixture.switch("background", false);
    fixture.switch("fail_turn", false);
    assert_eq!(thread.status(), Status::Failed);
    assert_eq!(thread.background_tasks().len(), 1);

    thread.restart().unwrap();
    assert!(thread.has_session(), "a fresh claude");
    assert!(thread.background_tasks().is_empty());
    assert_eq!(
        background_card(&thread),
        (
            ToolState::Failed,
            Some(BACKGROUND_STOPPED_NOTE.to_string()),
            false
        )
    );
    assert_eq!(thread.status(), Status::Failed);
    let relaunched = fixture.reload(thread.id());
    assert_eq!(relaunched.transcript().items(), thread.transcript().items());
    assert!(!kinds(&relaunched).contains(&"stopped"));
}

#[test]
fn continuing_a_thread_quit_before_claude_started_sends_the_message_again() {
    let fixture = Fixture::new();
    fixture.switch("never_init", true);
    let mut thread = fixture.draft();
    thread.send("Fix the login bug").unwrap();
    fixture.wait_for_messages(1);
    thread.shutdown_for_quit();
    let mut relaunched = fixture.reload(thread.id());
    drop(thread);
    assert_eq!(kinds(&relaunched), ["user", "stopped"]);
    assert!(!relaunched.session_started());

    // Claude never started the conversation, so "Continue where you left off." would mean
    // nothing to it: Continue sends the message again instead.
    fixture.switch("never_init", false);
    assert!(relaunched.continue_resends());
    let text = relaunched.continue_text();
    assert_eq!(text, "Fix the login bug");
    relaunched.send(&text).unwrap();
    finish_turn(&mut relaunched);
    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 2);
    assert_eq!(
        flag(&spawns[1], "--session-id"),
        Some(relaunched.session_id())
    );
    assert_eq!(
        fixture.messages_received(),
        ["Fix the login bug", "Fix the login bug"]
    );
    assert_eq!(relaunched.status(), Status::Done);
}

#[test]
fn continuing_a_started_thread_quit_before_claude_started_the_turn_sends_the_message_again() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    fixture.switch("never_init", true);
    thread.send("second").unwrap();
    fixture.wait_for_messages(2);
    thread.shutdown_for_quit();
    let mut relaunched = fixture.reload(thread.id());
    drop(thread);
    assert_eq!(
        kinds(&relaunched),
        ["user", "assistant", "turn_end", "user", "stopped"]
    );
    assert!(relaunched.session_started());
    assert!(relaunched.continue_resends());

    fixture.switch("never_init", false);
    let text = relaunched.continue_text();
    assert_eq!(text, "second");
    relaunched.send(&text).unwrap();
    finish_turn(&mut relaunched);
    let spawns = fixture.spawns();
    assert_eq!(spawns.len(), 3);
    assert_eq!(flag(&spawns[2], "--resume"), Some(relaunched.session_id()));
    assert_eq!(fixture.messages_received(), ["first", "second", "second"]);
    assert_eq!(relaunched.status(), Status::Done);
    assert!(!relaunched.continue_resends());
}

#[test]
fn after_a_crash_continue_resends_only_a_message_whose_turn_never_started() {
    let fixture = Fixture::new();
    let mut thread = started_thread(&fixture);
    fixture.switch("never_init", true);
    thread.send("second").unwrap();
    fixture.wait_for_messages(2);
    // Newjen is killed here: only what was saved at the last checkpoint (the message).
    let mut relaunched = fixture.reload(thread.id());
    drop(thread);
    assert_eq!(
        kinds(&relaunched),
        ["user", "assistant", "turn_end", "user", "stopped"]
    );
    assert_eq!(relaunched.continue_text(), "second");

    // This time claude starts the turn (`init`) before the kill, so it has the message.
    fixture.switch("never_init", false);
    fixture.switch("hang_after_init", true);
    relaunched.send("third").unwrap();
    pump_until(&mut relaunched, "the turn to start", |thread| {
        !thread.continue_resends()
    });
    let again = fixture.reload(relaunched.id());
    drop(relaunched);
    assert_eq!(kinds(&again).last(), Some(&"stopped"));
    assert!(!again.continue_resends());
    assert_eq!(again.continue_text(), CONTINUE_MESSAGE);
}

#[test]
fn archiving_mid_turn_saves_the_thread_as_stopped() {
    let fixture = Fixture::new();
    fixture.switch("hang", true);
    let mut thread = fixture.draft();
    thread.send("go").unwrap();
    pump_until(&mut thread, "the turn to start", |thread| {
        kinds(thread) == ["user", "tool", "assistant"]
    });
    thread.archive();
    assert!(!thread.has_session());
    assert_eq!(thread.status(), Status::Stopped);
    assert!(fixture.store.flush(FLUSH));
    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert_eq!(row.status, Status::Stopped);
}

#[test]
fn always_allow_that_never_reaches_claude_saves_no_grant() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    fixture.switch("exit_on_ask", true);
    let mut thread = fixture.draft();
    send_and_wait_for_approval(&mut thread, "make a file");
    // claude exits right after asking. Its exit is not passed on yet, so the approval is still
    // on screen when the user answers it.
    let events = thread.events();
    let deadline = Instant::now() + TIMEOUT;
    let mut exited = false;
    while !exited {
        match events.try_recv() {
            Ok(event) => exited = matches!(event.event(), SessionEvent::Exited { .. }),
            Err(_) => {
                assert!(Instant::now() < deadline, "timed out waiting for the exit");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    assert!(
        thread
            .answer(None, ApprovalIntent::AllowForSession)
            .is_err()
    );
    assert!(thread.grants().is_empty());
    assert!(fixture.store.flush(FLUSH));
    assert!(fixture.store.grants(thread.id()).unwrap().is_empty());
}

#[test]
fn a_rift_draft_holds_its_first_message_until_its_copy_is_ready_then_sends_it_once() {
    let fixture = Fixture::new();
    let (mut thread, _job) = fixture.preparing("Fix the login bug");
    assert!(!thread.is_draft(), "kept when the user switches away");
    assert_eq!(thread.works_in(), WorksIn::RiftCopy);
    assert_eq!(thread.title(), "Fix the login bug");
    let held = thread.preparing().expect("preparing");
    assert_eq!(held.message(), "Fix the login bug");
    assert!(held.started_at() <= Instant::now());
    assert!(
        thread.send("another").is_err(),
        "busy while the copy is made"
    );
    assert!(thread.set_works_in(WorksIn::ProjectFolder).is_err());
    thread.note_copy_progress(newjen_core::rift::Progress::Line(
        "installing deps".to_string(),
    ));
    assert_eq!(
        thread.preparing().unwrap().last_line(),
        Some("installing deps")
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        fixture.spawns().is_empty(),
        "no claude runs while preparing"
    );
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );

    let copy = fixture.fake_copy();
    thread.finish_copy(Ok(copy.clone())).unwrap();
    finish_turn(&mut thread);
    assert_eq!(thread.status(), Status::Done);
    assert_eq!(thread.copy(), Some(&copy));
    assert_eq!(thread.cwd(), copy.workdir);
    assert_eq!(fixture.messages_received(), ["Fix the login bug"]);
    assert_eq!(
        fixture.cwds(),
        [format!(
            "{}|{}|{}",
            copy.workdir.display(),
            copy.rift_id,
            copy.source.display()
        )]
    );

    assert!(thread.finish_copy(Ok(copy.clone())).is_err());
    settle(&mut thread);
    assert_eq!(
        fixture.messages_received(),
        ["Fix the login bug"],
        "never twice"
    );
    assert_eq!(fixture.spawns().len(), 1);

    let row = fixture.store.thread(thread.id()).unwrap().unwrap();
    assert!(
        fixture
            .store
            .pending_copies(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(row.workdir, copy.workdir);
    assert_eq!(row.copy.as_ref(), Some(&copy));
    let loaded = fixture.reload(thread.id());
    assert_eq!(loaded.copy(), Some(&copy));
    assert_eq!(loaded.session_config().cwd, copy.workdir);
    assert!(
        loaded
            .session_config()
            .env
            .contains(&("RIFT_ID".to_string(), copy.rift_id.clone()))
    );
}

#[test]
fn a_copy_that_could_not_be_made_hands_the_message_back_to_a_draft_that_still_wants_one() {
    let fixture = Fixture::new();
    let (mut thread, _job) = fixture.preparing("Fix the login bug");
    let failure = CopyError::Failed {
        command: "rift create".to_string(),
        detail: "postcreate hook failed".to_string(),
        output: vec!["npm ERR! missing script: setup".to_string()],
    };

    let not_sent = thread.finish_copy(Err(failure.clone())).unwrap_err();
    assert_eq!(not_sent.message, "Fix the login bug");
    assert_eq!(not_sent.error.downcast_ref::<CopyError>(), Some(&failure));
    assert!(thread.is_draft());
    assert!(thread.preparing().is_none());
    assert_eq!(thread.works_in(), WorksIn::RiftCopy);
    assert_eq!(thread.copy(), None);
    assert_eq!(thread.title(), NEW_THREAD_TITLE);
    settle(&mut thread);
    assert!(fixture.spawns().is_empty(), "nothing tried again by itself");
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );

    assert!(matches!(
        thread.send("Fix the login bug").unwrap(),
        Sent::MakingCopy(_)
    ));
}

#[test]
fn a_claude_left_by_try_again_is_stopped_before_the_draft_moves_into_its_copy() {
    let fixture = Fixture::new();
    let mut thread = fixture.draft();
    thread.restart().unwrap();
    fixture.wait_for_spawns(1);
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    let Sent::MakingCopy(_job) = thread.send("hello").unwrap() else {
        panic!("the message should wait for the copy");
    };

    let copy = fixture.fake_copy();
    thread.finish_copy(Ok(copy.clone())).unwrap();
    finish_turn(&mut thread);
    let cwds = fixture.cwds();
    assert_eq!(cwds.len(), 2);
    assert!(cwds[1].starts_with(&format!("{}|", copy.workdir.display())));
    assert_eq!(fixture.messages_received(), ["hello"]);
}

#[test]
fn archiving_a_rift_thread_removes_its_copy_once_claude_has_stopped() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    std::fs::write(fixture.project.path.join("a.txt"), "a\n").unwrap();
    let (mut thread, job) = fixture.preparing("Look around");
    let job_tools = tools.clone();
    let made = std::thread::spawn(move || job.run(&job_tools, |_| {}))
        .join()
        .unwrap();
    let copy = made.unwrap();
    assert_eq!(
        std::fs::read_to_string(copy.workdir.join("a.txt")).unwrap(),
        "a\n"
    );
    thread.finish_copy(Ok(copy.clone())).unwrap();
    finish_turn(&mut thread);
    assert!(thread.has_session());

    let removal = thread.archive().expect("a thread with a copy");
    assert!(!thread.has_session());
    assert_eq!(removal.copy(), &copy);
    removal.run(&tools).unwrap();
    assert!(!copy.path.exists());
    assert!(fixture.store.flush(FLUSH));
    assert_eq!(fixture.store.copy_owner(&copy.path).unwrap(), None);
    assert!(Removal::pending(&fixture.store).unwrap().is_empty());
}

#[test]
fn copies_of_threads_archived_before_a_relaunch_are_removed_at_startup() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let (mut thread, job) = fixture.preparing("Look around");
    let copy = job.run(&tools, |_| {}).unwrap();
    thread.finish_copy(Ok(copy.clone())).unwrap();
    finish_turn(&mut thread);
    drop(thread.archive());
    assert!(fixture.store.flush(FLUSH));
    assert!(copy.path.exists(), "Newjen quit before removing it");

    let pending = Removal::pending(&fixture.store).unwrap();
    assert_eq!(pending.len(), 1);
    for removal in pending {
        removal.run(&tools).unwrap();
    }
    assert!(!copy.path.exists());
    assert!(fixture.store.flush(FLUSH));
    assert!(Removal::pending(&fixture.store).unwrap().is_empty());
}

#[test]
fn a_new_copy_waits_for_agents_busy_in_the_project_folder_and_for_another_copy() {
    let fixture = Fixture::new();
    let mut idle = fixture.draft();
    let _ = idle.send("one").unwrap();
    finish_turn(&mut idle);
    assert!(!idle.blocks_copy_creation());

    fixture.switch("hang", true);
    let mut busy = fixture.draft();
    let _ = busy.send("two").unwrap();
    assert!(
        busy.blocks_copy_creation(),
        "mid-turn in the project folder"
    );

    let (preparing, _job) = fixture.preparing("three");
    assert!(
        preparing.blocks_copy_creation(),
        "another copy is being made"
    );

    let (mut in_copy, _job) = fixture.preparing("four");
    in_copy.finish_copy(Ok(fixture.fake_copy())).unwrap();
    assert!(in_copy.status().is_busy());
    assert!(!in_copy.blocks_copy_creation(), "it works in its own copy");
    busy.shutdown_for_quit();
    in_copy.shutdown_for_quit();
    assert!(!busy.blocks_copy_creation(), "stopped");
    fixture.switch("hang", false);

    let background = thread_with_a_background_task(&fixture);
    assert!(background.blocks_copy_creation(), "a background task runs");
}

#[test]
fn a_held_first_message_is_durable_before_the_copy_job_runs() {
    let fixture = Fixture::new();
    let (thread, job) = fixture.preparing("keep my message");
    let saved: String = fixture
        .database()
        .query_row(
            "SELECT message FROM pending_copies WHERE thread_id = ?1",
            [thread.id()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, "keep my message");
    drop(job);
}

#[test]
fn quit_before_copy_completion_recovers_the_message_without_a_send() {
    enum Exit {
        Quit,
        Crash,
    }
    for exit in [Exit::Quit, Exit::Crash] {
        let fixture = Fixture::new();
        let Some(tools) = fixture.rift_tools() else {
            return;
        };
        let (mut thread, job) = fixture.preparing("Fix the login bug");
        let copy = job.run(&tools, |_| {}).unwrap();
        match exit {
            Exit::Quit => thread.shutdown_for_quit(),
            Exit::Crash => {}
        }
        drop(thread);
        let store = Store::open_in(fixture.store.path().parent().unwrap()).unwrap();
        let pending = store.pending_copies(fixture.project.id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].intent,
            newjen_core::store::PendingIntent::Restore("Fix the login bug".to_string())
        );
        Thread::recover_pending_copy(&store, &fixture.project, &pending[0], &tools).unwrap();
        let recovered = Thread::recovered_draft(
            store.clone(),
            &fixture.project,
            fixture.config.clone(),
            &pending[0],
        );
        assert!(!copy.path.exists());
        assert!(recovered.is_draft());
        assert_eq!(recovered.works_in(), WorksIn::RiftCopy);
        assert!(!recovered.has_session());
        assert!(recovered.transcript().items().is_empty());
        assert!(fixture.spawns().is_empty());
        assert!(store.threads(fixture.project.id).unwrap().is_empty());
        assert!(store.flush(FLUSH));
        assert_eq!(
            store.pending_copies(fixture.project.id).unwrap().len(),
            1,
            "another quit preserves the composer message"
        );
    }
}

#[test]
fn quit_after_a_failed_first_send_cleans_the_copy_and_restores_the_message() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let mut config = fixture.config.clone();
    config.program = fixture.path("no-such-claude");
    let mut thread = Thread::new_draft(fixture.store.clone(), &fixture.project, config);
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    let Sent::MakingCopy(job) = thread.send("hello").unwrap() else {
        panic!()
    };
    let copy = job.run(&tools, |_| {}).unwrap();
    assert_eq!(
        thread.finish_copy(Ok(copy.clone())).unwrap_err().message,
        "hello"
    );
    assert!(thread.is_draft());
    assert!(thread.send("edited retry").is_err());
    thread.shutdown_for_quit();
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    assert_eq!(
        pending[0].intent,
        newjen_core::store::PendingIntent::Restore("edited retry".to_string())
    );
    assert_eq!(pending.len(), 1);
    Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending[0], &tools).unwrap();
    assert!(!copy.path.exists());
    assert!(fixture.spawns().is_empty());
}

#[test]
fn an_archived_copy_job_returns_its_copy_for_removal_and_never_sends() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let (mut thread, job) = fixture.preparing("hello");
    let copy = job.run(&tools, |_| {}).unwrap();
    assert!(thread.archive().is_none());
    let not_sent = thread.finish_copy(Ok(copy.clone())).unwrap_err();
    assert_eq!(not_sent.copy_to_remove.as_deref(), Some(&copy));
    newjen_core::rift::remove(&tools, &copy.source, &copy.path, &copy.rift_id).unwrap();
    assert!(!copy.path.exists());
    assert!(not_sent.message.is_empty());
    assert!(fixture.spawns().is_empty());
    assert!(fixture.store.flush(FLUSH));
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    assert_eq!(
        pending[0].intent,
        newjen_core::store::PendingIntent::Discard
    );
    Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending[0], &tools).unwrap();
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .pending_copies(fixture.project.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_failed_pending_write_does_not_start_a_copy_or_hold_the_message() {
    let fixture = Fixture::new();
    fixture.database().execute_batch("CREATE TRIGGER refuse_pending BEFORE INSERT ON pending_copies BEGIN SELECT RAISE(ABORT, 'disk full'); END;").unwrap();
    let mut thread = fixture.draft();
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    assert!(thread.send("keep this").is_err());
    assert!(thread.is_draft());
    assert!(thread.preparing().is_none());
    assert!(fixture.spawns().is_empty());
}

#[test]
fn sending_a_recovered_draft_in_the_project_folder_clears_its_pending_record() {
    let fixture = Fixture::new();
    let (_thread, job) = fixture.preparing("first message");
    drop(job);
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    let mut recovered = Thread::recovered_draft(
        fixture.store.clone(),
        &fixture.project,
        fixture.config.clone(),
        &pending[0],
    );
    recovered.set_works_in(WorksIn::ProjectFolder).unwrap();
    recovered.send("edited message").unwrap();
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .pending_copies(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    recovered.shutdown_for_quit();
}

#[test]
fn recovering_again_never_removes_a_replacement_after_the_original_was_cleaned() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let (thread, job) = fixture.preparing("keep my message");
    let copy = job.run(&tools, |_| {}).unwrap();
    drop(thread);
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending[0], &tools).unwrap();
    let replacement = newjen_core::rift::create(
        &tools,
        &fixture.project.path,
        copy.path.file_name().unwrap().to_str().unwrap(),
        |_| {},
    )
    .unwrap();
    std::fs::write(replacement.path.join("work.txt"), "unrelated work").unwrap();
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending[0], &tools).unwrap();
    assert_eq!(
        std::fs::read_to_string(replacement.path.join("work.txt")).unwrap(),
        "unrelated work"
    );
}

#[test]
fn recovery_preserves_a_replacement_before_the_first_cleanup() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let status = std::process::Command::new(&tools.jj)
        .args(["git", "init", "--colocate"])
        .arg(&fixture.project.path)
        .envs(tools.env.iter().cloned())
        .status()
        .unwrap();
    assert!(status.success());
    let (thread, job) = fixture.preparing("keep my message");
    let own = job.run(&tools, |_| {}).unwrap();
    drop(thread);
    newjen_core::rift::remove(&tools, &own.source, &own.path, &own.rift_id).unwrap();
    let replacement = newjen_core::rift::create(
        &tools,
        &fixture.project.path,
        own.path.file_name().unwrap().to_str().unwrap(),
        |_| {},
    )
    .unwrap();
    std::fs::write(replacement.path.join("valuable.txt"), "replacement work").unwrap();
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    assert_eq!(pending[0].rift_id.as_deref(), Some(own.rift_id.as_str()));
    assert_eq!(
        Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending[0], &tools)
            .unwrap(),
        newjen_core::thread::CopyRecovery::Draft
    );
    assert!(
        replacement.path.join("valuable.txt").exists(),
        "recovery deleted a different Rift id at the pending destination"
    );
}

#[test]
fn a_failed_first_thread_write_never_sweeps_a_copy_with_work() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    std::fs::write(fixture.project.path.join("a.txt"), "a\n").unwrap();
    fixture
        .database()
        .execute_batch("CREATE TRIGGER refuse_threads BEFORE INSERT ON threads BEGIN SELECT RAISE(ABORT, 'disk full'); END;")
        .unwrap();
    let (mut thread, job) = fixture.preparing("Refactor the parser");
    let copy = job.run(&tools, |_| {}).unwrap();
    thread.finish_copy(Ok(copy.clone())).unwrap();
    finish_turn(&mut thread);
    assert_eq!(fixture.messages_received(), ["Refactor the parser"]);
    std::fs::write(copy.workdir.join("parser.rs"), "fn parse() {}\n").unwrap();
    thread.shutdown_for_quit();
    drop(thread);
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].state, newjen_core::store::PendingState::Sent);
    for row in &pending {
        assert_eq!(
            Thread::recover_pending_copy(&fixture.store, &fixture.project, row, &tools).unwrap(),
            newjen_core::thread::CopyRecovery::Kept(copy.path.clone())
        );
    }
    assert!(fixture.store.flush(FLUSH));
    assert!(
        fixture
            .store
            .pending_copies(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    assert!(
        copy.workdir.join("parser.rs").exists(),
        "a copy in which claude already worked was deleted at relaunch"
    );
}

#[test]
fn a_failed_sent_checkpoint_returns_the_message_without_sending_it() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let (mut thread, job) = fixture.preparing("keep this message");
    let copy = job.run(&tools, |_| {}).unwrap();
    fixture.database().execute_batch("CREATE TRIGGER refuse_sent BEFORE INSERT ON pending_copies WHEN NEW.state = 'sent' BEGIN SELECT RAISE(ABORT, 'disk full'); END;").unwrap();
    let failed = thread.finish_copy(Ok(copy.clone())).unwrap_err();
    assert_eq!(failed.message, "keep this message");
    thread.shutdown_for_quit();
    assert!(fixture.messages_received().is_empty());
    assert!(
        fixture
            .store
            .threads(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    assert_eq!(
        pending[0].state,
        newjen_core::store::PendingState::MakingCopy
    );
    Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending[0], &tools).unwrap();
    assert!(!copy.path.exists());
}

#[test]
fn quitting_stops_rift_and_postcreate_hooks_before_recovery() {
    for provider in [
        newjen_core::providers::Provider::Claude,
        newjen_core::providers::Provider::Codex,
    ] {
        quitting_stops_rift_and_postcreate_hooks_before_recovery_for(provider);
    }
}

fn quitting_stops_rift_and_postcreate_hooks_before_recovery_for(
    provider: newjen_core::providers::Provider,
) {
    let mut fixture = Fixture::new();
    fixture.config.agent = newjen_core::agent::AgentConfig::cli_default(provider);
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let pid_file = fixture.path("hook-pid");
    let hook = format!(
        "echo $$ > '{}'; trap '' TERM; while :; do sleep 1; done",
        pid_file.display()
    );
    std::fs::write(
        fixture.project.path.join(".rift.toml"),
        format!("version = 1\n[[hooks.postcreate]]\nrun = {hook:?}\n"),
    )
    .unwrap();
    let (mut thread, job) = fixture.preparing("wait for setup");
    let job_thread = std::thread::spawn(move || job.run(&tools, |_| {}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pid_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let group = std::process::Command::new("/bin/ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let group = String::from_utf8(group.stdout).unwrap().trim().to_string();
    assert!(newjen_core::orphans::registry_dir().join(&group).exists());
    thread.shutdown_for_quit();
    assert!(!newjen_core::orphans::registry_dir().join(&group).exists());
    let deadline = Instant::now() + Duration::from_secs(2);
    while newjen_core::orphans::start_time(pid as u32).is_some() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let alive = newjen_core::orphans::start_time(pid as u32).is_some();
    if alive {
        // SAFETY: clean up the hook if the assertion fails.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }
    }
    let result = job_thread.join().unwrap();
    assert!(!alive, "postcreate survived quit");
    assert!(result.is_err());
    assert!(fixture.messages_received().is_empty());
}

#[test]
fn a_copy_job_not_yet_started_never_runs_rift_after_quit() {
    for provider in [
        newjen_core::providers::Provider::Claude,
        newjen_core::providers::Provider::Codex,
    ] {
        a_copy_job_not_yet_started_never_runs_rift_after_quit_for(provider);
    }
}

fn a_copy_job_not_yet_started_never_runs_rift_after_quit_for(
    provider: newjen_core::providers::Provider,
) {
    let mut fixture = Fixture::new();
    fixture.config.agent = newjen_core::agent::AgentConfig::cli_default(provider);
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let (mut thread, job) = fixture.preparing("wait for setup");
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    thread.shutdown_for_quit();
    assert!(job.run(&tools, |_| {}).is_err());
    assert!(!pending[0].path.exists());
}

#[test]
fn quitting_lets_a_running_jj_finish_and_prevents_the_copy_start() {
    for provider in [
        newjen_core::providers::Provider::Claude,
        newjen_core::providers::Provider::Codex,
    ] {
        quitting_lets_a_running_jj_finish_and_prevents_the_copy_start_for(provider);
    }
}

fn quitting_lets_a_running_jj_finish_and_prevents_the_copy_start_for(
    provider: newjen_core::providers::Provider,
) {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    fixture.config.agent = newjen_core::agent::AgentConfig::cli_default(provider);
    let Some(mut tools) = fixture.rift_tools() else {
        return;
    };
    let status = std::process::Command::new(&tools.jj)
        .args(["git", "init", "--colocate"])
        .arg(&fixture.project.path)
        .envs(tools.env.iter().cloned())
        .output()
        .unwrap();
    assert!(status.status.success());
    let started = fixture.path("jj-started");
    let release = fixture.path("jj-release");
    let wrapper = fixture.path("jj-wait");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep .02; done\nexec '{}' \"$@\"\n",
            started.display(),
            release.display(),
            tools.jj.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    tools.jj = wrapper;
    let (mut thread, job) = fixture.preparing("snapshot first");
    let pending = fixture.store.pending_copies(fixture.project.id).unwrap();
    let running = std::thread::spawn(move || job.run(&tools, |_| {}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(started.exists());
    thread.shutdown_for_quit();
    assert!(!running.is_finished());
    std::fs::write(release, "finish").unwrap();
    assert!(running.join().unwrap().is_err());
    assert!(!pending[0].path.exists());
}

#[test]
fn recovery_refuses_symlinked_copy_parents_and_preserves_unrelated_work() {
    for parent_depth in [1, 2] {
        let fixture = Fixture::new();
        let Some(tools) = fixture.rift_tools() else {
            return;
        };
        let (thread, job) = fixture.preparing("keep my message");
        drop(job);
        let pending = fixture
            .store
            .pending_copies(fixture.project.id)
            .unwrap()
            .remove(0);
        let parent = pending.path.ancestors().nth(parent_depth).unwrap();
        std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
        let outside = fixture.path("unrelated");
        std::fs::create_dir(&outside).unwrap();
        let victim = outside.join(pending.path.strip_prefix(parent).unwrap());
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("valuable.txt"), "unrelated work").unwrap();
        std::os::unix::fs::symlink(&outside, parent).unwrap();
        let result =
            Thread::recover_pending_copy(&fixture.store, &fixture.project, &pending, &tools);
        assert!(
            victim.join("valuable.txt").exists(),
            "recovery deleted unrelated work through a symlinked parent"
        );
        assert!(result.is_err());
        assert_eq!(
            fixture.store.pending_copies(fixture.project.id).unwrap()[0],
            pending
        );
        assert!(fixture.messages_received().is_empty());
        drop(thread);
    }
}

#[test]
fn retrying_a_draft_keeps_its_failed_attempt_for_cleanup() {
    failed_attempt_stays_tracked(WorksIn::RiftCopy, CleanupFailure::RiftRemove);
}

#[test]
fn sending_a_draft_in_the_project_keeps_its_failed_attempt_for_cleanup() {
    failed_attempt_stays_tracked(WorksIn::ProjectFolder, CleanupFailure::RiftRemove);
}

#[test]
fn retrying_after_a_hook_blocks_trash_keeps_the_leftover_for_cleanup() {
    failed_attempt_stays_tracked(WorksIn::RiftCopy, CleanupFailure::TrashSymlink);
}

enum CleanupFailure {
    RiftRemove,
    TrashSymlink,
}

fn failed_attempt_stays_tracked(works_in: WorksIn, failure: CleanupFailure) {
    let fixture = Fixture::new();
    let Some(mut tools) = fixture.rift_tools() else {
        return;
    };
    let real_tools = tools.clone();
    let wrapper = fixture.path("rift-without-remove");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = remove ]; then exit 1; fi\nexec '{}' \"$@\"\n",
            tools.rift.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        std::process::Command::new(&tools.jj)
            .args(["git", "init", "--colocate"])
            .arg(&fixture.project.path)
            .envs(tools.env.iter().cloned())
            .output()
            .unwrap()
            .status
            .success()
    );
    let hook = match failure {
        CleanupFailure::RiftRemove => {
            tools.rift = wrapper;
            "exit 3"
        }
        CleanupFailure::TrashSymlink => {
            r#"mkdir -p "$RIFT_DESTINATION/../safe"; ln -s safe "$RIFT_DESTINATION/../.trash"; exit 7"#
        }
    };
    std::fs::write(
        fixture.project.path.join(".rift.toml"),
        format!("version = 1\n[[hooks.postcreate]]\nrun = '{hook}'\n"),
    )
    .unwrap();
    let (mut thread, job) = fixture.preparing("first try");
    let first = fixture
        .store
        .pending_copies(fixture.project.id)
        .unwrap()
        .remove(0);
    let error = job.run(&tools, |_| {}).unwrap_err();
    assert!(first.path.exists());
    assert_eq!(
        thread.finish_copy(Err(error)).unwrap_err().message,
        "first try"
    );
    std::fs::remove_file(fixture.project.path.join(".rift.toml")).unwrap();
    thread.set_works_in(works_in).unwrap();
    match thread.send("second try").unwrap() {
        Sent::MakingCopy(job) => drop(job),
        Sent::Started => finish_turn(&mut thread),
    }
    assert!(fixture.store.flush(FLUSH));
    let tracked: i64 = fixture
        .database()
        .query_row(
            "SELECT count(*) FROM copy_cleanup WHERE path = ?1",
            [first.path.to_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tracked, 1, "the failed attempt was forgotten");
    assert!(first.path.exists());
    let cleanup = fixture.store.copy_cleanup(fixture.project.id).unwrap();
    assert_eq!(cleanup.len(), 1);
    assert!(cleanup[0].rift_id.is_some());
    assert!(Thread::recover_copy_cleanup(&fixture.store, &fixture.project, &tools).is_err());
    assert_eq!(
        fixture.store.copy_cleanup(fixture.project.id).unwrap(),
        cleanup
    );
    if matches!(failure, CleanupFailure::TrashSymlink) {
        std::fs::remove_file(first.path.parent().unwrap().join(".trash")).unwrap();
    }
    Thread::recover_copy_cleanup(&fixture.store, &fixture.project, &real_tools).unwrap();
    assert!(fixture.store.flush(FLUSH));
    assert!(!first.path.exists());
    assert!(
        fixture
            .store
            .copy_cleanup(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    Thread::recover_copy_cleanup(&fixture.store, &fixture.project, &real_tools).unwrap();
    thread.shutdown_for_quit();
}

#[test]
fn opaque_approval_keys_reject_cross_thread_stale_and_previous_process_answers() {
    let fixture = Fixture::new();
    fixture.switch("ask", true);
    let mut first = fixture.draft();
    let mut second = fixture.draft();
    send_and_wait_for_approval(&mut first, "first");
    send_and_wait_for_approval(&mut second, "second");
    let first_key = first.transcript().pending_approval().unwrap().key();
    let second_key = second.transcript().pending_approval().unwrap().key();
    assert_ne!(first_key, second_key);
    first
        .answer(Some(second_key), ApprovalIntent::AllowForSession)
        .unwrap();
    assert_eq!(
        first.transcript().pending_approval().unwrap().key(),
        first_key
    );
    assert!(first.grants().is_empty());
    first
        .answer(Some(first_key), ApprovalIntent::AllowOnce)
        .unwrap();
    first
        .answer(Some(first_key), ApprovalIntent::AllowForSession)
        .unwrap();
    assert!(
        first.grants().is_empty(),
        "stale remember never grants anything"
    );
    finish_turn(&mut first);
    first.restart().unwrap();
    send_and_wait_for_approval(&mut first, "after restart");
    let current = first.transcript().pending_approval().unwrap().key();
    assert_ne!(first_key, current);
    first
        .answer(Some(first_key), ApprovalIntent::DenyAndStop)
        .unwrap();
    assert_eq!(
        first.transcript().pending_approval().unwrap().key(),
        current
    );
    first
        .answer(Some(current), ApprovalIntent::AllowOnce)
        .unwrap();
    second
        .answer(Some(second_key), ApprovalIntent::AllowOnce)
        .unwrap();
    finish_turn(&mut first);
    finish_turn(&mut second);
    assert_eq!(first.status(), Status::Done);
    assert_eq!(second.status(), Status::Done);
}

#[test]
fn cancelling_before_the_copy_is_made_never_starts_an_agent() {
    let fixture = Fixture::new();
    let (mut thread, job) = fixture.preparing("Do not send this");
    thread.stop().unwrap();
    assert!(thread.preparing().unwrap().cancelled());
    assert!(thread.send("too early").is_err());
    let made = job.run(&Tools::find(), |_| {});
    assert_eq!(made, Err(CopyError::Cancelled));
    let failed = thread.finish_copy(made).unwrap_err();
    assert_eq!(failed.message, "Do not send this");
    assert!(failed.copy_to_remove.is_none());
    assert!(thread.is_draft());
    assert!(fixture.spawns().is_empty());
    assert!(fixture.messages_received().is_empty());
}

#[test]
fn cancelling_during_a_rift_hook_stops_it_and_removes_the_copy() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    std::fs::write(
        fixture.project.path.join(".rift.toml"),
        "version = 1\n[[hooks.postcreate]]\nrun = \"echo hook-ready >&2; sleep 30; echo finished > hook-finished\"\n",
    )
    .unwrap();
    let (mut thread, job) = fixture.preparing("Held until ready");
    let (ready, started) = std::sync::mpsc::channel();
    let worker_tools = tools.clone();
    let worker = std::thread::spawn(move || {
        job.run(&worker_tools, |progress| {
            if let newjen_core::rift::Progress::Line(line) = progress
                && line.contains("hook-ready")
            {
                let _ = ready.send(());
            }
        })
    });
    started.recv_timeout(Duration::from_secs(20)).unwrap();
    let path = thread.preparing().unwrap().path().to_path_buf();
    let stopped = Instant::now();
    thread.stop().unwrap();
    assert!(thread.send("not yet").is_err());
    let made = worker.join().unwrap();
    assert!(
        stopped.elapsed() < Duration::from_secs(20),
        "the hook was stopped rather than waited for"
    );
    assert_eq!(made, Err(CopyError::Cancelled));
    assert_eq!(
        thread.finish_copy(made).unwrap_err().message,
        "Held until ready"
    );
    assert!(!path.exists(), "the half-made copy is removed");
    assert!(fixture.store.flush(Duration::from_secs(5)));
    assert!(
        fixture
            .store
            .pending_copies(fixture.project.id)
            .unwrap()
            .is_empty()
    );
    assert!(fixture.spawns().is_empty());
    assert!(fixture.messages_received().is_empty());
}

#[test]
fn a_copy_finished_after_cancelling_is_removed_instead_of_used() {
    let fixture = Fixture::new();
    let Some(tools) = fixture.rift_tools() else {
        return;
    };
    let (mut thread, job) = fixture.preparing("Never deliver");
    let copy = job.run(&tools, |_| {}).unwrap();
    thread.stop().unwrap();
    let failed = thread.finish_copy(Ok(copy.clone())).unwrap_err();
    assert_eq!(failed.message, "Never deliver");
    assert_eq!(failed.copy_to_remove.as_deref(), Some(&copy));
    assert!(thread.is_draft());
    assert!(fixture.messages_received().is_empty());
    assert!(fixture.spawns().is_empty());
}
