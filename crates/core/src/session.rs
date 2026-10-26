//! Spawns and owns one `claude` CLI process speaking the stream-json protocol.
//!
//! Two background threads read the process's output and turn it into [`SessionEvent`]s on an
//! `async_channel`, which the UI can await (or a test can poll). A third thread writes to the
//! process: [`Session`]'s methods (prompts, approval answers, interrupts) only put a finished
//! line on a queue and return. So a stalled `claude` that stops reading its stdin can never
//! freeze the caller (the UI thread) or block [`Session::shutdown`].
//!
//! Why threads and not an async runtime: there is exactly one process per conversation and its
//! input and output are line-based, so plain threads are the simplest thing that works, and
//! `core` stays independent of any UI framework's executor.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use async_channel::{Receiver, Sender};
use serde_json::Value;

use crate::orphans;
use crate::protocol::{self, Incoming, PermissionAnswer, PermissionRequest};

/// Environment variables that mark "this process runs inside Claude Code". They leak in when
/// Newjen itself is started from a Claude Code terminal and would make the child think it is a
/// nested session, so they are removed. Everything else (API keys, `CLAUDE_CONFIG_DIR`, cloud
/// provider settings) is passed through unchanged.
const NESTED_SESSION_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
];

/// How long `shutdown` waits after SIGTERM before it sends SIGKILL. Kept short because GPUI
/// gives the whole app about 200 ms to quit.
const GRACE_PERIOD: Duration = Duration::from_millis(140);

/// How long the stdout thread waits for the stderr thread to drain after the process exits.
const STDERR_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// How to start the CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    /// The `claude` executable. Defaults to [`find_claude`].
    pub program: PathBuf,
    /// The project directory the agent works in.
    pub cwd: PathBuf,
    /// A Claude session id to continue (`--resume <id>`).
    pub resume: Option<String>,
    /// Extra command-line arguments, appended after Newjen's own.
    pub extra_args: Vec<String>,
    /// Extra environment variables for the child. Mostly for tests (to drive the fake CLI).
    pub env: Vec<(String, String)>,
}

impl SessionConfig {
    /// A config that runs the `claude` found by [`find_claude`] in `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        SessionConfig {
            program: find_claude(),
            cwd: cwd.into(),
            resume: None,
            extra_args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// The full argument list passed to the CLI.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--permission-prompt-tool",
            "stdio",
            "--permission-mode",
            "default",
            "--include-partial-messages",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
        if let Some(session_id) = &self.resume {
            args.push("--resume".to_string());
            args.push(session_id.clone());
        }
        args.extend(self.extra_args.iter().cloned());
        args
    }
}

/// Finds the `claude` executable.
///
/// Order: `$NEWJEN_CLAUDE`, then every directory on `$PATH`, then the usual install locations.
/// The fallbacks matter because apps started from Finder get a minimal `PATH` that usually
/// does not include where `claude` was installed. If nothing is found, returns plain `claude`
/// so the spawn error message names it.
pub fn find_claude() -> PathBuf {
    if let Some(path) = std::env::var_os("NEWJEN_CLAUDE").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("claude");
            if is_executable(&candidate) {
                return candidate;
            }
        }
    }
    let mut fallbacks = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        fallbacks.push(home.join(".local/bin/claude"));
        fallbacks.push(home.join(".claude/local/claude"));
    }
    fallbacks.push(PathBuf::from("/opt/homebrew/bin/claude"));
    fallbacks.push(PathBuf::from("/usr/local/bin/claude"));
    fallbacks
        .into_iter()
        .find(|candidate| is_executable(candidate))
        .unwrap_or_else(|| PathBuf::from("claude"))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // `metadata` follows symlinks, so a symlink to the real binary counts.
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Something that happened in the CLI process.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// One decoded stdout line.
    Line(Incoming),
    /// One stderr line (warnings, crash output). Useful in error messages.
    Stderr(String),
    /// The process ended. `code` is `None` if it was killed by a signal. `requested` is true
    /// when [`Session::shutdown`] (or dropping the `Session`) was called before it ended, so
    /// the exit was expected rather than a crash.
    ///
    /// The last stdout-derived event. A few `Stderr` lines can still follow it if a leftover
    /// grandchild keeps stderr open.
    Exited { code: Option<i32>, requested: bool },
}

/// A running `claude` process. Dropping it shuts the process down.
pub struct Session {
    pid: u32,
    /// Lines waiting to be written to the process's stdin by the writer thread. Closing this
    /// queue makes the writer thread finish and drop stdin, which closes the pipe. The CLI
    /// needs stdin open for the whole session (when it closes, the CLI rejects pending
    /// approvals and exits), so it is closed only by `shutdown`.
    to_stdin: Sender<String>,
    /// Set by the first `shutdown` call so later calls return at once. Shared with the stdout
    /// thread, which reports it in `Exited { requested }`.
    shutting_down: Arc<AtomicBool>,
    /// Set by the reader thread once the process has exited and been reaped.
    exited: Arc<AtomicBool>,
    /// Counter for the ids of our own control requests (interrupts).
    next_request_number: AtomicU64,
}

impl Session {
    /// Starts the CLI. Returns the session and the channel its events arrive on.
    pub fn spawn(config: SessionConfig) -> Result<(Session, Receiver<SessionEvent>)> {
        let mut command = Command::new(&config.program);
        command
            .args(config.args())
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A new process group, so `killpg` also reaches the tools claude starts.
            .process_group(0);
        for name in NESTED_SESSION_ENV_VARS {
            command.env_remove(name);
        }
        for (name, value) in &config.env {
            command.env(OsString::from(name), OsString::from(value));
        }

        let mut child = command.spawn().with_context(|| {
            format!(
                "could not start `{}` in {}",
                config.program.display(),
                config.cwd.display()
            )
        })?;
        let pid = child.id();
        // Best effort: if this fails, the only loss is orphan cleanup after a crash.
        let _ = orphans::register(pid);

        let stdin = child.stdin.take().context("child has no stdin")?;
        let stdout = child.stdout.take().context("child has no stdout")?;
        let stderr = child.stderr.take().context("child has no stderr")?;

        let to_stdin = spawn_stdin_writer(stdin);
        let (sender, receiver) = async_channel::unbounded();
        let exited = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));

        let stderr_done = spawn_stderr_reader(stderr, sender.clone());
        spawn_stdout_reader(
            child,
            stdout,
            sender,
            stderr_done,
            exited.clone(),
            shutting_down.clone(),
        );

        let session = Session {
            pid,
            to_stdin,
            shutting_down,
            exited,
            next_request_number: AtomicU64::new(1),
        };
        Ok((session, receiver))
    }

    /// Sends a user prompt.
    pub fn send_prompt(&self, text: &str) -> Result<()> {
        self.write_line(&protocol::user_message(text))
    }

    /// Answers an approval request.
    pub fn answer(&self, request: &PermissionRequest, answer: PermissionAnswer) -> Result<()> {
        self.write_line(&protocol::permission_response(request, answer))
    }

    /// Asks the CLI to stop the current turn. The turn then ends with a "stopped" result.
    pub fn interrupt(&self) -> Result<()> {
        let number = self.next_request_number.fetch_add(1, Ordering::SeqCst);
        let request_id = format!("newjen_interrupt_{number}");
        self.write_line(&protocol::interrupt_request(&request_id))
    }

    /// Stops the process: closes stdin, sends SIGTERM to its process group, and SIGKILL if it
    /// is still alive after a short grace period. Safe to call more than once; later calls
    /// return immediately. Takes at most about 150 ms, inside GPUI's quit budget, and never
    /// waits for the writer thread (which may be stuck writing to a claude that stopped
    /// reading; the SIGKILL below unsticks it).
    pub fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        // Closing the queue never blocks. The writer thread then drops stdin, which closes the
        // pipe, as soon as it is not in the middle of a write.
        self.to_stdin.close();
        if self.exited.load(Ordering::SeqCst) {
            // claude was reaped earlier, so its pid (which is also the group id) may already
            // belong to an unrelated process; signalling the group now could hit that. Tools a
            // crashed claude left in its group were already sent SIGTERM by the reader thread.
            return;
        }
        let group = self.pid as libc::pid_t;
        // SAFETY: killpg only sends a signal; errors (e.g. the group is already gone) are fine.
        unsafe { libc::killpg(group, libc::SIGTERM) };
        let deadline = Instant::now() + GRACE_PERIOD;
        while Instant::now() < deadline {
            if self.exited.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        // Even if claude itself exited, tools it started may still be running in its group.
        // SAFETY: as above.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        // The process is gone now. Unregister here too (not only in the reader thread), because
        // the app may exit right after this call, before that thread gets to run.
        orphans::unregister(self.pid);
    }

    /// The process id. Also the process group id, since the child leads its own group.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Serializes `message` with a JSON encoder and queues it as one line for the writer
    /// thread. The CLI exits on a line that is not valid JSON, so lines are never built by hand.
    ///
    /// Returns at once. Fails if the session was shut down or claude stopped accepting input
    /// (the writer thread closes the queue when a write fails).
    fn write_line(&self, message: &Value) -> Result<()> {
        let mut line = serde_json::to_string(message)?;
        line.push('\n');
        // The queue is unbounded, so the only possible error is "closed".
        self.to_stdin
            .try_send(line)
            .map_err(|_| anyhow!("the claude session has ended (shut down, or claude exited)"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Writes queued lines to claude's stdin, in order, until the queue is closed (by `shutdown`)
/// or a write fails (claude is gone). Then it drops stdin, which closes the pipe.
fn spawn_stdin_writer(mut stdin: ChildStdin) -> Sender<String> {
    let (sender, receiver) = async_channel::unbounded::<String>();
    thread::spawn(move || {
        // `recv_blocking` still hands out lines queued before the close, then returns an error.
        while let Ok(line) = receiver.recv_blocking() {
            if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
                // Closing the queue makes later `send_prompt`/`answer` calls report an error
                // instead of silently queueing lines nobody will write.
                receiver.close();
                break;
            }
        }
        drop(stdin);
    });
    sender
}

/// Reads stdout line by line until EOF, then reaps the process and sends `Exited` last.
fn spawn_stdout_reader(
    mut child: Child,
    stdout: impl Read + Send + 'static,
    sender: Sender<SessionEvent>,
    stderr_done: mpsc::Receiver<()>,
    exited: Arc<AtomicBool>,
    shutting_down: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        for_each_line(stdout, |line| {
            if line.trim().is_empty() {
                return;
            }
            // If nobody is listening any more, keep draining so the child never blocks on a
            // full pipe.
            let _ = sender.send_blocking(SessionEvent::Line(Incoming::parse(line)));
        });
        let code = child.wait().ok().and_then(|status| status.code());
        // Read after the process is gone: true only if shutdown started before it ended.
        let requested = shutting_down.load(Ordering::SeqCst);
        if !requested {
            // claude ended on its own (often a crash). Stop the tools it started, which share
            // its process group, so they don't run on unattended. Safe even though claude is
            // already reaped: a group id is not handed out again while any process is still
            // in that group, and if the group is already empty this just fails with ESRCH.
            // The only risk is the pid being reused as a new group leader in the moment since
            // `wait()` returned, which is vanishingly unlikely.
            // SAFETY: killpg only sends a signal.
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGTERM) };
        }
        // Unregister before setting `exited`: once `exited` is set, `shutdown` returns early
        // and skips its own unregister, and the app may quit right after.
        orphans::unregister(child.id());
        exited.store(true, Ordering::SeqCst);
        // Let the last stderr lines arrive before `Exited`, but don't hang if a leftover
        // grandchild keeps stderr open.
        let _ = stderr_done.recv_timeout(STDERR_DRAIN_TIMEOUT);
        let _ = sender.send_blocking(SessionEvent::Exited { code, requested });
    });
}

/// Forwards stderr lines. Signals `done` at EOF.
fn spawn_stderr_reader(
    stderr: impl Read + Send + 'static,
    sender: Sender<SessionEvent>,
) -> mpsc::Receiver<()> {
    let (done_sender, done) = mpsc::channel();
    thread::spawn(move || {
        for_each_line(stderr, |line| {
            let _ = sender.send_blocking(SessionEvent::Stderr(line.to_string()));
        });
        let _ = done_sender.send(());
    });
    done
}

/// Calls `handle` for every line (without the newline). Invalid UTF-8 is replaced rather than
/// treated as an error, so one odd byte never stops the reader.
fn for_each_line(source: impl Read, mut handle: impl FnMut(&str)) {
    let mut reader = BufReader::new(source);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buffer);
                handle(line.trim_end_matches(['\n', '\r']));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_include_the_protocol_flags_and_resume() {
        let mut config = SessionConfig::new("/tmp");
        config.resume = Some("abc".to_string());
        config.extra_args = vec!["--x".to_string()];
        let args = config.args();
        assert_eq!(args[0], "-p");
        assert!(args.contains(&"--include-partial-messages".to_string()));
        assert!(!args.contains(&"--model".to_string()));
        let tail: Vec<&str> = args[args.len() - 3..].iter().map(String::as_str).collect();
        assert_eq!(tail, ["--resume", "abc", "--x"]);
    }
}
