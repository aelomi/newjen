//! TEST SUPPORT ONLY: a stand-in for the `claude` CLI.
//!
//! It replays a recorded fixture (a `.jsonl` file of `{"dir": "out"|"in", "msg": {...}}`
//! lines) so integration tests can drive a real `Session` without network or a Claude login.
//!
//! - The fixture path comes from the `NEWJEN_FAKE_FIXTURE` environment variable.
//! - `out` lines are printed to stdout, one JSON object per line, like the real CLI.
//! - For each `in` line it reads one line from stdin and checks it is valid JSON with the same
//!   `type` (and, for a `control_response`, the same `subtype`, `request_id` and `behavior`).
//!   On a mismatch it prints why to stderr and exits with code 3.
//! - When the fixture is fully replayed it exits 0. (The real CLI would instead wait for stdin
//!   to close.) If stdin closes early, it exits 0 too, like the real CLI.
//!
//! Command-line arguments (the real CLI's flags) are ignored.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::Value;

const MISMATCH: u8 = 3;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--version"] {
        println!("2.1.316 (Claude)");
        return ExitCode::SUCCESS;
    }
    if args == ["auth", "status", "--json"] {
        println!(
            "{}",
            serde_json::json!({"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max","email":"user@example.com"})
        );
        return ExitCode::SUCCESS;
    }
    let Some(path) = std::env::var_os("NEWJEN_FAKE_FIXTURE") else {
        eprintln!("newjen-fake-claude: set NEWJEN_FAKE_FIXTURE to a fixture .jsonl file");
        return ExitCode::from(2);
    };
    let fixture = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("newjen-fake-claude: cannot read fixture: {error}");
            return ExitCode::from(2);
        }
    };

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    for (number, line) in fixture.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) => {
                eprintln!(
                    "newjen-fake-claude: fixture line {} is not JSON: {error}",
                    number + 1
                );
                return ExitCode::from(2);
            }
        };
        let message = &record["msg"];
        match record["dir"].as_str() {
            Some("out") => {
                // Ignore write errors: the reader may have gone away, which is not our problem.
                let _ = writeln!(stdout, "{message}");
                let _ = stdout.flush();
            }
            Some("in") => {
                let mut received = String::new();
                match stdin.read_line(&mut received) {
                    Ok(0) | Err(_) => return ExitCode::SUCCESS, // stdin closed: exit like claude.
                    Ok(_) => {}
                }
                if let Err(reason) = check_input(message, &received) {
                    eprintln!(
                        "newjen-fake-claude: unexpected input on fixture line {}: {reason}",
                        number + 1
                    );
                    return ExitCode::from(MISMATCH);
                }
            }
            _ => {}
        }
    }
    ExitCode::SUCCESS
}

/// Compares what Newjen sent with what the fixture expected at this point.
fn check_input(expected: &Value, received: &str) -> Result<(), String> {
    let received: Value = serde_json::from_str(received.trim())
        .map_err(|error| format!("not valid JSON ({error}): {received}"))?;
    // Recorder wrapper for non-JSON input (probe fixtures): nothing to compare.
    if expected.get("type").is_none() {
        return Ok(());
    }
    if received["type"] != expected["type"] {
        return Err(format!(
            "expected type {}, got {}",
            expected["type"], received["type"]
        ));
    }
    if expected["type"] == "control_response" {
        for pointer in [
            "/response/subtype",
            "/response/request_id",
            "/response/response/behavior",
        ] {
            if received.pointer(pointer) != expected.pointer(pointer) {
                return Err(format!(
                    "{pointer}: expected {:?}, got {:?}",
                    expected.pointer(pointer),
                    received.pointer(pointer)
                ));
            }
        }
    }
    Ok(())
}
