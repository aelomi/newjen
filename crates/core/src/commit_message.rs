use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};

const PROMPT: &str = "Write a git commit message for the diff on stdin. Use Conventional Commits \
(`type(scope): subject`), a subject of at most 72 characters in the imperative mood, then a blank \
line and a short body only if the change needs explaining. Reply with the message only, no code \
fences or commentary.";
const MAX_INPUT_BYTES: usize = 50_000;
const TIMEOUT: Duration = Duration::from_secs(90);

/// Asks the user's own `claude` CLI, on a small model with no tools, for a commit message for
/// `patch`. Runs only when the user asks for a commit, so it starts no turn on its own.
pub fn generate(claude: &Path, cwd: &Path, patch: &str) -> Result<String> {
    ensure!(!patch.trim().is_empty(), "There is nothing to commit");
    let mut input = patch.as_bytes();
    if input.len() > MAX_INPUT_BYTES {
        input = &input[..MAX_INPUT_BYTES];
    }
    let mut command = Command::new(claude);
    command
        .args([
            "-p",
            "--model",
            "haiku",
            "--tools",
            "",
            "--no-session-persistence",
            "--output-format",
            "text",
            PROMPT,
        ])
        .current_dir(cwd)
        .env("PATH", crate::env::resolved_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::env::strip_session_env(&mut command);
    let mut child = command.spawn().context("Cannot start claude")?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let err = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("claude took too long to write the commit message");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let _ = writer.join();
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    ensure!(
        status.success(),
        "claude could not write the commit message: {}",
        stderr.trim()
    );
    let message = clean(&stdout);
    ensure!(
        !message.is_empty(),
        "claude returned an empty commit message"
    );
    Ok(message)
}

fn clean(reply: &str) -> String {
    let trimmed = reply.trim();
    let unfenced = trimmed
        .strip_prefix("```")
        .and_then(|rest| rest.split_once('\n').map(|(_, body)| body))
        .and_then(|body| body.strip_suffix("```"))
        .unwrap_or(trimmed);
    unfenced.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn a_fenced_reply_loses_its_fence() {
        assert_eq!(clean("```text\nfeat: add notes\n```\n"), "feat: add notes");
        assert_eq!(clean("  fix: keep stdin open \n"), "fix: keep stdin open");
    }
}
