//! Test-only stdio fixture replayer. No network or provider login.
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};

fn substitute(value: &mut Value, cwd: &str, ids: &HashMap<String, Value>) {
    match value {
        Value::String(text) if text == "$CWD" => *text = cwd.into(),
        Value::Array(values) => {
            for value in values {
                substitute(value, cwd, ids);
            }
        }
        Value::Object(fields) => {
            if let Some(id) = fields.get_mut("id")
                && let Some(actual) = ids.get(&id.to_string())
            {
                *id = actual.clone();
            }
            for value in fields.values_mut() {
                substitute(value, cwd, ids);
            }
        }
        _ => (),
    }
}
fn subset(expected: &Value, actual: &Value) -> bool {
    match expected {
        Value::Object(fields) => fields.iter().all(|(key, value)| {
            actual.get(key).is_some_and(|actual| {
                if key == "cwd"
                    && let (Some(expected), Some(actual)) = (value.as_str(), actual.as_str())
                {
                    std::path::Path::new(expected) == std::path::Path::new(actual)
                } else {
                    subset(value, actual)
                }
            })
        }),
        _ => expected == actual,
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var("NEWJEN_FAKE_FIXTURE")?;
    let fixture = std::fs::read_to_string(path)?;
    let cwd = std::env::current_dir()?.display().to_string();
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut stdout = std::io::stdout().lock();
    let mut ids = HashMap::new();
    for record in fixture.lines().filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(record)?;
        let mut message = record["msg"].clone();
        substitute(&mut message, &cwd, &ids);
        if record["dir"] == "in" {
            let mut line = String::new();
            if stdin.read_line(&mut line)? == 0 {
                return Ok(());
            }
            if let Ok(path) = std::env::var("NEWJEN_FAKE_INPUT_LOG") {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)?;
                file.write_all(line.as_bytes())?;
            }
            let actual: Value = serde_json::from_str(&line)?;
            if message["method"] == "thread/open" {
                if actual["method"] != "thread/start" && actual["method"] != "thread/resume" {
                    return Err("expected thread open".into());
                }
                message["method"] = actual["method"].clone();
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                ids.insert(record["msg"]["id"].to_string(), actual["id"].clone());
                message["id"] = actual["id"].clone();
            }
            if !subset(&message, &actual) {
                return Err(format!("expected {message}, got {actual}").into());
            }
        } else if record["dir"] == "out" {
            writeln!(stdout, "{message}")?;
            stdout.flush()?;
        }
    }
    let mut line = String::new();
    while stdin.read_line(&mut line)? != 0 {
        if let Ok(path) = std::env::var("NEWJEN_FAKE_INPUT_LOG") {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            file.write_all(line.as_bytes())?;
        }
        line.clear();
    }
    Ok(())
}
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("newjen-fake-codex: {error}");
            std::process::ExitCode::from(3)
        }
    }
}
