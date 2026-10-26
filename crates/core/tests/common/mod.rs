//! Helpers shared by the integration tests.

#![allow(dead_code)] // Each test file uses a different subset.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use async_channel::Receiver;
use newjen_core::protocol::Incoming;
use newjen_core::session::SessionEvent;
use serde_json::Value;

/// One fixture line: who wrote it, and the message.
pub struct FixtureLine {
    /// True for lines the CLI printed (`"dir": "out"`), false for lines we sent.
    pub out: bool,
    pub msg: Value,
}

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.jsonl"))
}

/// The names (without `.jsonl`) of every fixture.
pub fn all_fixture_names() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("fixtures folder")
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".jsonl").map(str::to_string)
        })
        .collect();
    names.sort();
    names
}

pub fn load(name: &str) -> Vec<FixtureLine> {
    let text = std::fs::read_to_string(fixture_path(name)).expect("fixture file");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let record: Value = serde_json::from_str(line).expect("fixture line is JSON");
            FixtureLine {
                out: record["dir"] == "out",
                msg: record["msg"].clone(),
            }
        })
        .collect()
}

/// Waits for the next event, failing the test after `timeout`.
pub fn next_event(events: &Receiver<SessionEvent>, timeout: Duration) -> SessionEvent {
    let deadline = Instant::now() + timeout;
    loop {
        match events.try_recv() {
            Ok(event) => return event,
            Err(async_channel::TryRecvError::Closed) => panic!("event channel closed"),
            Err(async_channel::TryRecvError::Empty) => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for a session event"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// True if the event is a `can_use_tool` approval request.
pub fn is_permission_request(event: &SessionEvent) -> bool {
    matches!(event, SessionEvent::Line(Incoming::PermissionRequest(_)))
}
