//! A small registry of the `claude` processes Newjen started, so that processes left behind by
//! a crashed Newjen can be cleaned up the next time it starts.
//!
//! Each running child gets a file in [`registry_dir`] named after the child's pid; the file
//! holds the pid of the Newjen process that owns it. On start-up, [`reap`] kills children whose
//! owner is gone. Processes owned by a Newjen that is still running are never touched, so
//! several Newjen windows or instances can share the registry safely.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

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
    fs::create_dir_all(dir)?;
    fs::write(
        dir.join(child_pid.to_string()),
        std::process::id().to_string(),
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
        let owner_pid = fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok());
        let (Some(child_pid), Some(owner_pid)) = (child_pid, owner_pid) else {
            let _ = fs::remove_file(&path); // Not one of our records, or corrupt.
            continue;
        };
        if is_alive(owner_pid) {
            continue; // Owned by a running Newjen: leave it alone.
        }
        if is_alive(child_pid) && is_orphaned_claude(child_pid) {
            kill_group(child_pid);
            killed.push(child_pid);
        }
        let _ = fs::remove_file(&path);
    }
    killed
}

/// True if a process with this pid exists (including ones we may not signal).
fn is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks whether the process exists; nothing is sent.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// True if the process looks like the Claude Code CLI *and* is an orphan. Guards against
/// killing an unrelated process that happens to reuse an old pid, including the user's own
/// `claude` in a terminal.
///
/// When Newjen dies, macOS hands its children to launchd, so a real leftover has parent pid 1.
/// A `claude` started any other way (from a shell, or by a running Newjen) has a live parent.
fn is_orphaned_claude(pid: u32) -> bool {
    let Ok(output) = Command::new("ps")
        .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    // The output looks like `    1 /path/to/claude`. The path may contain spaces, so split
    // off only the first field.
    let text = String::from_utf8_lossy(&output.stdout);
    let Some((parent, command)) = text.trim().split_once(char::is_whitespace) else {
        return false;
    };
    if parent.parse::<u32>() != Ok(1) {
        return false;
    }
    let command = command.trim();
    let name = Path::new(command)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // The installer's `claude` is a symlink to `…/claude/versions/<version>`, which shows up
    // under that path when started through the real file.
    name == "claude" || command.contains("/claude/versions/")
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
