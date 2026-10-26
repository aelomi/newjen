//! A small registry of the `claude` processes Newjen started, so that processes left behind by
//! a crashed Newjen can be cleaned up the next time it starts.
//!
//! Each running child gets a file in [`registry_dir`] named after the child's pid. The file
//! holds one line, `<owner pid> <owner start time> <child start time>`: the Newjen process that
//! owns the child, and when each of the two processes started. On start-up, [`reap`] kills
//! children whose owner is gone. Processes owned by a Newjen that is still running are never
//! touched, so several Newjen windows or instances can share the registry safely.
//!
//! Why start times: macOS reuses pids. A pid alone could, by the time we look, belong to a
//! different process. A pid plus the moment that process started identifies it for certain.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Where the registry files live: a `newjen-sessions` folder in the per-user temp directory.
pub fn registry_dir() -> PathBuf {
    std::env::temp_dir().join("newjen-sessions")
}

/// Records that this Newjen process owns `child_pid`.
pub fn register(child_pid: u32) -> io::Result<()> {
    register_in(&registry_dir(), child_pid)
}

/// Removes the record for `child_pid` (after it exited). Errors are ignored.
pub fn unregister(child_pid: u32) {
    unregister_in(&registry_dir(), child_pid);
}

/// Kills `claude` processes left over from a Newjen that is no longer running, and removes
/// stale records. Returns the pids it killed. Call once at app start.
pub fn reap() -> Vec<u32> {
    reap_in(&registry_dir())
}

/// [`register`] with an explicit registry folder (used by tests).
pub fn register_in(dir: &Path, child_pid: u32) -> io::Result<()> {
    let owner_pid = std::process::id();
    let owner_start = start_time(owner_pid).ok_or_else(|| missing_process(owner_pid))?;
    let child_start = start_time(child_pid).ok_or_else(|| missing_process(child_pid))?;
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(child_pid.to_string()),
        format!("{owner_pid} {owner_start} {child_start}"),
    )
}

/// [`unregister`] with an explicit registry folder (used by tests).
pub fn unregister_in(dir: &Path, child_pid: u32) {
    let _ = fs::remove_file(dir.join(child_pid.to_string()));
}

/// [`reap`] with an explicit registry folder (used by tests).
pub fn reap_in(dir: &Path) -> Vec<u32> {
    let mut killed = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return killed; // No registry yet: nothing to do.
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let child_pid = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok());
        let record = fs::read_to_string(&path)
            .ok()
            .and_then(|text| Record::parse(&text));
        let (Some(child_pid), Some(record)) = (child_pid, record) else {
            // Not one of our records, corrupt, or written by an older Newjen without start
            // times. Without start times we cannot be sure who the pid belongs to: never kill.
            let _ = fs::remove_file(&path);
            continue;
        };
        if is_same_process(record.owner_pid, record.owner_start) {
            continue; // Owned by a running Newjen: leave it alone.
        }
        // The owner is gone (or its pid now belongs to another process). Kill the child only if
        // it is still the exact process we started and it is an orphan.
        if let Some(child) = process_info(child_pid)
            && child.start == record.child_start
            && child.parent_pid == 1
        {
            kill_group(child_pid);
            killed.push(child_pid);
        }
        let _ = fs::remove_file(&path);
    }
    killed
}

/// When the process started, in microseconds since 1970, or `None` if there is no such
/// process (or we may not inspect it). Public so tests can write registry records.
pub fn start_time(pid: u32) -> Option<u64> {
    process_info(pid).map(|info| info.start)
}

/// One registry file's contents.
struct Record {
    owner_pid: u32,
    owner_start: u64,
    child_start: u64,
}

impl Record {
    fn parse(text: &str) -> Option<Record> {
        let mut fields = text.split_whitespace();
        let record = Record {
            owner_pid: fields.next()?.parse().ok()?,
            owner_start: fields.next()?.parse().ok()?,
            child_start: fields.next()?.parse().ok()?,
        };
        // Anything extra means this is not a file we wrote.
        fields.next().is_none().then_some(record)
    }
}

/// What the registry needs to know about a running process.
struct ProcessInfo {
    /// Start time in microseconds since 1970.
    start: u64,
    /// When Newjen dies, macOS hands its children to launchd, so a real leftover has parent
    /// pid 1. A `claude` started any other way (from a shell, or by a running Newjen) has a
    /// live parent.
    parent_pid: u32,
}

/// Asks the kernel about `pid` (macOS `proc_pidinfo`, the call `ps` itself uses). `None` if
/// the process does not exist, is a zombie, or belongs to another user we may not inspect.
fn process_info(pid: u32) -> Option<ProcessInfo> {
    let pid = libc::c_int::try_from(pid).ok().filter(|pid| *pid > 0)?;
    // SAFETY: `proc_bsdinfo` is a plain C struct of integers and byte arrays, so all-zero
    // bytes are a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: the kernel writes at most `size` bytes into `info`, which is exactly that big.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<libc::c_void>(),
            size,
        )
    };
    if written != size {
        return None;
    }
    Some(ProcessInfo {
        start: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        parent_pid: info.pbi_ppid,
    })
}

/// True if `pid` is running and is the same process that started at `start`. A pid that is
/// gone, or that now belongs to a process started at another time (pid reuse), is not.
fn is_same_process(pid: u32, start: u64) -> bool {
    process_info(pid).is_some_and(|info| info.start == start)
}

fn missing_process(pid: u32) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, format!("process {pid} not found"))
}

/// Terminates the process group led by `pid` (claude and the tools it started).
fn kill_group(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: only sends signals. SIGKILL because the owner is gone and nobody will answer
    // this process's questions any more.
    unsafe {
        if libc::killpg(pid, libc::SIGKILL) != 0 {
            // Not a group leader (should not happen for our children): kill just the process.
            libc::kill(pid, libc::SIGKILL);
        }
    }
}
