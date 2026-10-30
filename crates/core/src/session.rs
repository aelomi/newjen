//! Spawns and owns one `claude` CLI process speaking the stream-json protocol.
//!
//! Two background threads read the process's output and turn it into [`SessionEvent`]s on an
//! `async_channel`, which the UI can await (or a test can poll). A third thread waits for the
//! process to end, cleans up after it and sends the final `Exited` event. A fourth writes to
//! the process: [`Session`]'s methods (prompts, approval answers, interrupts) only put a
//! finished line on a queue and return. So a stalled `claude` that stops reading its stdin can
//! never freeze the caller (the UI thread) or block [`Session::shutdown`].
//!
//! Why threads and not an async runtime: there is exactly one process per conversation and its
//! input and output are line-based, so plain threads are the simplest thing that works, and
//! `core` stays independent of any UI framework's executor.
//!
//! claude is started through a tiny shell launcher (see `LAUNCHER`) that also starts a
//! watcher, so claude and its tools die as soon as Newjen dies, even after a crash.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, PipeWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
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
    // Names the app driving Claude. An SDK app that launched Newjen would otherwise have its
    // name sent on Newjen's requests. Newjen doesn't set its own, like T3 Code.
    "CLAUDE_AGENT_SDK_CLIENT_APP",
];

/// How long `shutdown` waits after SIGTERM before it sends SIGKILL. Kept short because GPUI
/// gives the whole app about 200 ms to quit.
const GRACE_PERIOD: Duration = Duration::from_millis(140);

/// When claude exits on its own (a crash), how long the tools it left running get after
/// SIGTERM before they are sent SIGKILL.
const CRASH_GRACE_PERIOD: Duration = Duration::from_millis(150);

/// After the process exits, how long to wait for the stdout and stderr threads to read the
/// last lines before `Exited` is sent anyway. Bounded, because a leftover process outside the
/// group could keep a pipe open forever.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// How long [`Session::interrupt`] waits for claude to confirm a stop request (or end the turn)
/// before it ends the session. Synara also uses 10 s.
pub const INTERRUPT_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// The shell script that starts claude: `/bin/sh -c LAUNCHER sh <claude> <args…>`.
///
/// It starts a background *watcher*, then `exec`s claude, so claude keeps the shell's pid (and
/// `Session::pid` and the orphan registry still name claude). The watcher waits on fd 3, the
/// read end of a pipe whose write end only Newjen holds. When Newjen dies, even by a crash or
/// SIGKILL, the OS closes that write end, `read` returns, and the watcher stops claude's whole
/// process group: SIGTERM, then SIGKILL 2 s later. Without it, the orphan registry would only
/// clean up at the next launch. (T3 Code does the same.)
///
/// - `trap '' TERM`: the watcher survives its own SIGTERM to the group, so it can follow up
///   with SIGKILL. Because it stays in the group until then, the group id cannot be handed to
///   another process while it waits.
/// - `</dev/null >/dev/null 2>&1`: the watcher must not hold claude's pipes, or our stdout
///   reader would not see EOF and a failed write to claude's stdin would go unnoticed.
/// - `3<&-`: claude itself does not inherit fd 3.
///
/// Newjen closes the write end whenever the session ends, and its SIGKILL to the group also
/// kills the watcher, so no watcher outlives a session.
const LAUNCHER: &str = "(trap '' TERM; read _ <&3; kill -TERM 0; sleep 2; kill -KILL 0) \
                        </dev/null >/dev/null 2>&1 & exec \"$@\" 3<&-";

/// The fd number the watcher reads (see `LAUNCHER`).
const WATCH_FD: libc::c_int = 3;

/// Which Claude conversation a new `claude` process works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conversation {
    /// `--session-id <id>`: start a new conversation with this id. Newjen picks the id itself
    /// (a v4 UUID), so it is known before claude starts. Fails if the id is already in use.
    New(String),
    /// `--resume <id>`: continue a saved conversation. Fails if claude has no conversation
    /// with that id. (claude does not replay the earlier messages on stdout.)
    Resume(String),
}

/// How to start the CLI.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    /// The `claude` executable. Defaults to [`find_claude`].
    pub program: PathBuf,
    /// The project directory the agent works in.
    pub cwd: PathBuf,
    /// The conversation to start or continue. `None` lets claude pick a new id itself.
    pub conversation: Option<Conversation>,
    /// Rules claude may use without asking, each passed as its own `--allowedTools <rule>`
    /// pair, e.g. `Bash(touch one.txt)` or `mcp__pencil__get_style` (see
    /// `protocol::SessionRule::cli_rule`). This is how a thread's "Always allow" grants
    /// outlive the claude process that first heard them.
    pub allowed_tools: Vec<String>,
    /// Extra command-line arguments, appended after Newjen's own.
    pub extra_args: Vec<String>,
    /// Extra environment variables for the child. Mostly for tests (to drive the fake CLI).
    pub env: Vec<(String, String)>,
    /// How long an interrupt may go unanswered before the session is ended. Defaults to
    /// [`INTERRUPT_ACK_TIMEOUT`]; tests use a shorter one.
    pub interrupt_ack_timeout: Duration,
}

impl SessionConfig {
    /// A config that runs the `claude` found by [`find_claude`] in `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        SessionConfig {
            program: find_claude(),
            cwd: cwd.into(),
            conversation: None,
            allowed_tools: Vec::new(),
            extra_args: Vec::new(),
            env: Vec::new(),
            interrupt_ack_timeout: INTERRUPT_ACK_TIMEOUT,
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
        match &self.conversation {
            Some(Conversation::New(session_id)) => {
                args.push("--session-id".to_string());
                args.push(session_id.clone());
            }
            Some(Conversation::Resume(session_id)) => {
                args.push("--resume".to_string());
                args.push(session_id.clone());
            }
            None => {}
        }
        // One flag per rule. The CLI accepts several rules after one flag too, but then splits
        // them on spaces and commas, which would break rules such as `Bash(echo a,b)`.
        for rule in &self.allowed_tools {
            args.push("--allowedTools".to_string());
            args.push(rule.clone());
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
// `Line` is much bigger than the other variants. That is fine: each event is built once and
// moved through the channel once, and boxing it would make every `match` on a line clumsier.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// One decoded stdout line.
    Line(Incoming),
    /// One stderr line (warnings, crash output). Useful in error messages.
    Stderr(String),
    /// Writing a prompt or answer to claude's stdin failed (claude closed it or is gone), so
    /// it never arrived. Holds the OS error text. Never sent after [`Session::shutdown`], and
    /// it can arrive just before or just after the `Exited` of a crashed claude.
    WriteFailed(String),
    /// claude did not confirm an interrupt within the deadline (see
    /// [`SessionConfig::interrupt_ack_timeout`]), so the session was shut down. Sent just
    /// before the `Exited` of that shutdown.
    InterruptTimedOut,
    /// The process ended. `code` is `None` if it was killed by a signal. `requested` is true
    /// when [`Session::shutdown`] (or dropping the `Session`) was called before it ended, so
    /// the exit was expected rather than a crash.
    ///
    /// Normally the last event. It waits (briefly) for stdout and stderr to reach the end, so
    /// only a leftover process outside claude's group that keeps a pipe open, or a
    /// `WriteFailed`, can follow it.
    Exited { code: Option<i32>, requested: bool },
}

/// A running `claude` process. Dropping it shuts the process down.
pub struct Session {
    control: Arc<Control>,
    /// Counter for the ids of our own control requests (interrupts).
    next_request_number: AtomicU64,
    interrupt_ack_timeout: Duration,
}

/// The parts of a session that its background threads need too: to stop the process, to
/// answer control requests, and to track an unanswered interrupt. Shared through an `Arc` (a
/// reference-counted pointer: several owners, one object, freed when the last one is gone).
struct Control {
    pid: u32,
    /// Lines waiting to be written to the process's stdin by the writer thread. Closing this
    /// queue makes the writer thread finish and drop stdin, which closes the pipe. The CLI
    /// needs stdin open for the whole session (when it closes, the CLI rejects pending
    /// approvals and exits), so it is closed only by `shutdown`, when claude has exited, or
    /// when a write fails. After that, `send_prompt`, `answer` and `interrupt` return an error.
    to_stdin: Sender<String>,
    /// Set by the first `shutdown` call so later calls return at once. Read by the waiter
    /// thread, which reports it in `Exited { requested }`, and the writer thread, which then
    /// stays quiet about failed writes.
    shutting_down: Arc<AtomicBool>,
    /// Set by the waiter thread once the process has exited, been reaped and (after a crash)
    /// the tools it left behind were stopped.
    exited: AtomicBool,
    /// Newjen's end of the pipe the watcher waits on (see `LAUNCHER`). Dropping it tells the
    /// watcher to stop claude, so it is dropped only when the session ends.
    owner_pipe: Mutex<Option<PipeWriter>>,
    /// The id of an interrupt claude has not confirmed yet.
    pending_interrupt: Mutex<Option<String>>,
    /// Set when an interrupt went unanswered and the session was shut down because of it.
    interrupt_timed_out: AtomicBool,
}

impl Session {
    /// Starts the CLI. Returns the session and the channel its events arrive on.
    pub fn spawn(config: SessionConfig) -> Result<(Session, Receiver<SessionEvent>)> {
        let could_not_start = || {
            format!(
                "could not start `{}` in {}",
                config.program.display(),
                config.cwd.display()
            )
        };
        // The shell would only report a missing claude after it started (as exit code 127),
        // so check first and keep a clear error.
        check_program(&config).with_context(could_not_start)?;

        // The owner pipe (see `LAUNCHER`). std opens both ends with close-on-exec.
        let (watch_read, watch_write) = std::io::pipe().context("could not create a pipe")?;
        // SAFETY: fcntl on a file descriptor we own; it only sets a flag. Close-on-exec on our
        // end means claude and its tools never inherit it (std already sets it; this makes the
        // requirement explicit).
        unsafe { libc::fcntl(watch_write.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        let read_fd = watch_read.as_raw_fd();

        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(LAUNCHER)
            .arg("sh") // `$0` inside the script
            .arg(&config.program)
            .args(config.args())
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A new process group, so `killpg` also reaches the tools claude starts.
            .process_group(0);
        // SAFETY: this runs in the child between fork and exec, where only async-signal-safe
        // calls are allowed; `dup2`, `fcntl` and reading errno are. It puts the pipe's read end
        // on fd 3 for the watcher. The copy `dup2` makes does not have close-on-exec; if the
        // read end already is fd 3, clear the flag instead.
        unsafe {
            command.pre_exec(move || {
                let result = if read_fd == WATCH_FD {
                    libc::fcntl(WATCH_FD, libc::F_SETFD, 0)
                } else {
                    libc::dup2(read_fd, WATCH_FD)
                };
                if result == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        for name in NESTED_SESSION_ENV_VARS {
            command.env_remove(name);
        }
        for (name, value) in &config.env {
            command.env(OsString::from(name), OsString::from(value));
        }

        let mut child = command.spawn().with_context(could_not_start)?;
        // Only the watcher needs the read end.
        drop(watch_read);
        let pid = child.id();
        // Best effort: if this fails, the only loss is orphan cleanup after a crash.
        let _ = orphans::register(pid);

        let stdin = child.stdin.take().context("child has no stdin")?;
        let stdout = child.stdout.take().context("child has no stdout")?;
        let stderr = child.stderr.take().context("child has no stderr")?;

        let (sender, receiver) = async_channel::unbounded();
        let shutting_down = Arc::new(AtomicBool::new(false));

        let to_stdin = spawn_stdin_writer(stdin, sender.clone(), shutting_down.clone());
        let control = Arc::new(Control {
            pid,
            to_stdin,
            shutting_down,
            exited: AtomicBool::new(false),
            owner_pipe: Mutex::new(Some(watch_write)),
            pending_interrupt: Mutex::new(None),
            interrupt_timed_out: AtomicBool::new(false),
        });
        let stdout_done = spawn_stdout_reader(stdout, sender.clone(), control.clone());
        let stderr_done = spawn_stderr_reader(stderr, sender.clone());
        spawn_waiter(child, sender, [stdout_done, stderr_done], control.clone());

        let session = Session {
            control,
            next_request_number: AtomicU64::new(1),
            interrupt_ack_timeout: config.interrupt_ack_timeout,
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
    ///
    /// If claude neither confirms the request nor ends the turn within
    /// [`SessionConfig::interrupt_ack_timeout`], the session is shut down and
    /// [`SessionEvent::InterruptTimedOut`] is sent, so a stuck claude can always be stopped.
    pub fn interrupt(&self) -> Result<()> {
        let number = self.next_request_number.fetch_add(1, Ordering::SeqCst);
        let request_id = format!("newjen_interrupt_{number}");
        // Remember it before sending, so even a very quick answer finds it.
        *lock(&self.control.pending_interrupt) = Some(request_id.clone());
        if let Err(error) = self.write_line(&protocol::interrupt_request(&request_id)) {
            lock(&self.control.pending_interrupt).take();
            return Err(error);
        }
        let control = self.control.clone();
        let timeout = self.interrupt_ack_timeout;
        thread::spawn(move || {
            thread::sleep(timeout);
            control.expire_interrupt(&request_id);
        });
        Ok(())
    }

    /// Stops the process: closes stdin, sends SIGTERM to its process group, and SIGKILL if it
    /// is still alive after a short grace period. Safe to call more than once; later calls
    /// return immediately. Takes at most about 150 ms, inside GPUI's quit budget, and never
    /// waits for the writer thread (which may be stuck writing to a claude that stopped
    /// reading; the SIGKILL below unsticks it).
    pub fn shutdown(&self) {
        self.control.shutdown();
    }

    /// The process id. Also the process group id, since the child leads its own group.
    pub fn pid(&self) -> u32 {
        self.control.pid
    }

    /// TEST SUPPORT ONLY: closes Newjen's end of the owner pipe and nothing else, as if Newjen
    /// had died. The watcher should then stop claude and its tools on its own.
    #[doc(hidden)]
    pub fn simulate_owner_death(&self) {
        self.control.close_owner_pipe();
    }

    /// Serializes `message` and queues it for the writer thread (see [`queue_line`]).
    fn write_line(&self, message: &Value) -> Result<()> {
        queue_line(&self.control.to_stdin, message)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Control {
    /// See [`Session::shutdown`].
    fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return;
        }
        // Closing the queue never blocks. The writer thread then drops stdin, which closes the
        // pipe, as soon as it is not in the middle of a write.
        self.to_stdin.close();
        // The session is over, so the watcher may go (the SIGKILL below also stops it).
        self.close_owner_pipe();
        if self.exited.load(Ordering::SeqCst) {
            // claude was reaped earlier, so its pid (which is also the group id) may already
            // belong to an unrelated process; signalling the group now could hit that. Tools a
            // crashed claude left in its group were already stopped by the waiter thread.
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
        // Even if claude itself exited, tools it started (and the watcher, which ignores
        // SIGTERM) may still be running in its group.
        // SAFETY: as above.
        unsafe { libc::killpg(group, libc::SIGKILL) };
        // The process is gone now. Unregister here too (not only in the waiter thread), because
        // the app may exit right after this call, before that thread gets to run.
        orphans::unregister(self.pid);
    }

    /// Drops Newjen's end of the owner pipe, which makes the watcher stop claude's group.
    fn close_owner_pipe(&self) {
        lock(&self.owner_pipe).take();
    }

    /// Called by an interrupt's timer when its deadline has passed. If claude still has not
    /// answered that interrupt, ends the session; the waiter thread then reports
    /// `InterruptTimedOut` just before `Exited`.
    fn expire_interrupt(&self, request_id: &str) {
        let unanswered = lock(&self.pending_interrupt).as_deref() == Some(request_id);
        let ended = self.shutting_down.load(Ordering::SeqCst) || self.exited.load(Ordering::SeqCst);
        if unanswered && !ended {
            // Set before shutting down, so the waiter sees it when claude's exit wakes it.
            self.interrupt_timed_out.store(true, Ordering::SeqCst);
            self.shutdown();
        }
    }

    /// Handles the stdout lines the session itself must react to, before they go to the UI.
    fn note_incoming(&self, incoming: &Incoming) {
        match incoming {
            // A control request Newjen does not handle (a valid `can_use_tool` became
            // `PermissionRequest` instead). The CLI would wait for an answer forever, so say no.
            Incoming::ControlRequest(request) => {
                let error = match request.subtype() {
                    "can_use_tool" => "Newjen could not read this approval request".to_string(),
                    subtype => format!("Newjen does not support {subtype} yet"),
                };
                let reply = protocol::control_error_response(&request.request_id, &error);
                // A closed queue means the session is ending; nobody waits for the reply then.
                let _ = queue_line(&self.to_stdin, &reply);
            }
            // claude confirmed our interrupt.
            Incoming::ControlResponse(response) => {
                let mut pending = lock(&self.pending_interrupt);
                if pending.is_some() && pending.as_deref() == response.request_id() {
                    *pending = None;
                }
            }
            // The turn ended, so there is nothing left to stop.
            Incoming::Result(_) => {
                lock(&self.pending_interrupt).take();
            }
            _ => {}
        }
    }
}

/// Locks a mutex. A mutex is "poisoned" if a thread panicked while holding it; the data here
/// (an `Option`) is still valid then, so carry on instead of panicking too.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Serializes `message` with a JSON encoder and queues it as one line for the writer thread.
/// The CLI exits on a line that is not valid JSON, so lines are never built by hand.
///
/// Returns at once. Fails if the session was shut down, claude exited, or an earlier write
/// failed (each of these closes the queue).
fn queue_line(to_stdin: &Sender<String>, message: &Value) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    // The queue is unbounded, so the only possible error is "closed".
    to_stdin
        .try_send(line)
        .map_err(|_| anyhow!("the claude session has ended (shut down, or claude exited)"))
}

/// Checks that `config.program` can be started, looking for it the way the shell's `exec`
/// will: a path is taken relative to `config.cwd`, a bare name is searched on `PATH` (the
/// child's, if `config.env` sets one).
fn check_program(config: &SessionConfig) -> Result<()> {
    let program = &config.program;
    let found = if program.components().count() > 1 {
        is_executable(&config.cwd.join(program))
    } else {
        let path = config
            .env
            .iter()
            .rev()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| OsString::from(value))
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
    };
    if found {
        Ok(())
    } else {
        Err(anyhow!(
            "`{}` was not found or is not executable",
            program.display()
        ))
    }
}

/// Writes queued lines to claude's stdin, in order, until the queue is closed (by `shutdown`
/// or when claude exits) or a write fails. Then it drops stdin, which closes the pipe.
fn spawn_stdin_writer(
    mut stdin: ChildStdin,
    events: Sender<SessionEvent>,
    shutting_down: Arc<AtomicBool>,
) -> Sender<String> {
    let (sender, receiver) = async_channel::unbounded::<String>();
    thread::spawn(move || {
        // `recv_blocking` still hands out lines queued before the close, then returns an error.
        while let Ok(line) = receiver.recv_blocking() {
            let written = stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush());
            if let Err(error) = written {
                // Closing the queue makes later `send_prompt`/`answer` calls report an error
                // instead of silently queueing lines nobody will write.
                receiver.close();
                // Tell the UI, so a lost prompt or answer never fails silently. After
                // `shutdown` a failed write is expected (we killed claude), so stay quiet.
                if !shutting_down.load(Ordering::SeqCst) {
                    let _ = events.send_blocking(SessionEvent::WriteFailed(error.to_string()));
                }
                break;
            }
        }
        drop(stdin);
    });
    sender
}

/// Reads stdout line by line until EOF. Signals the returned channel at EOF.
fn spawn_stdout_reader(
    stdout: impl Read + Send + 'static,
    sender: Sender<SessionEvent>,
    control: Arc<Control>,
) -> mpsc::Receiver<()> {
    let (done_sender, done) = mpsc::channel();
    thread::spawn(move || {
        for_each_line(stdout, |line| {
            if line.trim().is_empty() {
                return;
            }
            let incoming = Incoming::parse(line);
            control.note_incoming(&incoming);
            // If nobody is listening any more, keep draining so the child never blocks on a
            // full pipe.
            let _ = sender.send_blocking(SessionEvent::Line(incoming));
        });
        let _ = done_sender.send(());
    });
    done
}

/// Waits for claude to exit, cleans up, and sends `Exited` last.
///
/// This does not wait for stdout to close first: a tool claude started can inherit stdout and
/// keep it open long after claude itself is gone, and a crash must still be reported.
fn spawn_waiter(
    mut child: Child,
    sender: Sender<SessionEvent>,
    readers_done: [mpsc::Receiver<()>; 2],
    control: Arc<Control>,
) {
    thread::spawn(move || {
        let code = child.wait().ok().and_then(|status| status.code());
        let pid = child.id();
        // Read after the process is gone: true only if shutdown started before it ended.
        let requested = control.shutting_down.load(Ordering::SeqCst);
        // The session is over, so the watcher may go. (Its SIGTERM is harmless; the SIGKILL
        // from `shutdown` or `stop_leftover_tools` removes the watcher itself.)
        control.close_owner_pipe();
        // claude is reaped, so the orphan registry has nothing left to clean up. Unregister
        // before setting `exited`: once `exited` is set, `shutdown` returns early and skips its
        // own unregister, and the app may quit right after.
        orphans::unregister(pid);
        if !requested {
            stop_leftover_tools(pid);
        }
        // Nobody reads stdin any more: make `send_prompt`, `answer` and `interrupt` fail at
        // once instead of queueing lines into a dead pipe. Done before `Exited` is sent, so a
        // caller that has seen `Exited` can rely on it.
        control.to_stdin.close();
        // Set only after the cleanup above. Until then a `shutdown` call (say, the app
        // quitting) still signals the group itself, so a stubborn tool cannot outlive the app.
        control.exited.store(true, Ordering::SeqCst);
        // Let the last stdout and stderr lines arrive before `Exited`, but don't hang if a
        // leftover process keeps a pipe open.
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        for done in readers_done {
            let _ = done.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
        if control.interrupt_timed_out.load(Ordering::SeqCst) {
            let _ = sender.send_blocking(SessionEvent::InterruptTimedOut);
        }
        let _ = sender.send_blocking(SessionEvent::Exited { code, requested });
    });
}

/// claude ended on its own (often a crash). Stops the tools it started, which share its process
/// group, so they don't run on unattended: SIGTERM, up to [`CRASH_GRACE_PERIOD`] for them to
/// exit, then SIGKILL for any that ignored it (including the watcher, which ignores SIGTERM).
///
/// Safe even though claude is already reaped: a group id is not handed out again while any
/// process is still in that group, and if the group is already empty the signals just fail
/// with ESRCH. The only risk is the pid being reused as a new group leader in the moment since
/// `wait()` returned, which is vanishingly unlikely.
fn stop_leftover_tools(pid: u32) {
    let group = pid as libc::pid_t;
    // SAFETY (all three calls): killpg only sends a signal.
    unsafe { libc::killpg(group, libc::SIGTERM) };
    let deadline = Instant::now() + CRASH_GRACE_PERIOD;
    while Instant::now() < deadline {
        // Signal 0 sends nothing; it fails once no process is left in the group.
        if unsafe { libc::killpg(group, 0) } != 0 {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    unsafe { libc::killpg(group, libc::SIGKILL) };
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
        config.conversation = Some(Conversation::Resume("abc".to_string()));
        config.extra_args = vec!["--x".to_string()];
        let args = config.args();
        assert_eq!(args[0], "-p");
        assert!(args.contains(&"--include-partial-messages".to_string()));
        assert!(!args.contains(&"--model".to_string()));
        let tail: Vec<&str> = args[args.len() - 3..].iter().map(String::as_str).collect();
        assert_eq!(tail, ["--resume", "abc", "--x"]);
    }

    #[test]
    fn args_start_a_new_conversation_with_one_allowed_tools_flag_per_rule() {
        let mut config = SessionConfig::new("/tmp");
        config.conversation = Some(Conversation::New("id-1".to_string()));
        config.allowed_tools = vec![
            "Bash(touch one.txt)".to_string(),
            "Bash(echo a,b)".to_string(),
            "mcp__pencil__get_style".to_string(),
        ];
        let args = config.args();
        assert!(!args.contains(&"--resume".to_string()));
        let tail: Vec<&str> = args[args.len() - 8..].iter().map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "--session-id",
                "id-1",
                "--allowedTools",
                "Bash(touch one.txt)",
                "--allowedTools",
                "Bash(echo a,b)",
                "--allowedTools",
                "mcp__pencil__get_style",
            ]
        );
    }

    #[test]
    fn args_without_a_conversation_let_claude_pick_one() {
        let args = SessionConfig::new("/tmp").args();
        assert!(!args.contains(&"--resume".to_string()));
        assert!(!args.contains(&"--session-id".to_string()));
        assert!(!args.contains(&"--allowedTools".to_string()));
    }
}
