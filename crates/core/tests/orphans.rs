//! The orphan registry kills `claude` processes whose Newjen owner is gone, and nothing else.
//! A process is recognised by its pid *and* its start time, never by its name, so `claude`
//! installs that run under `node` or `bun` are cleaned up too (these tests use plain `sleep`).

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use newjen_core::orphans;

/// A pid that cannot belong to a running process (above macOS's pid limit of 99998).
const DEAD_OWNER_PID: u32 = 999_999;

/// Starts `sleep 60` in its own process group like a real session. It is a child of this test
/// process, so it is *not* an orphan.
fn spawn_child() -> Child {
    Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn sleep")
}

/// Starts `sleep 60` as a real orphan: a shell starts it in the background and exits, so
/// launchd (pid 1) becomes its parent, as after a Newjen crash.
fn spawn_orphan() -> u32 {
    let output = Command::new("/bin/sh")
        .args(["-c", "/bin/sleep 60 >/dev/null 2>&1 & echo $!"])
        .output()
        .expect("start orphan");
    let pid = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("orphan pid");
    // Give launchd a moment to adopt it.
    let deadline = Instant::now() + Duration::from_secs(2);
    while parent_of(pid) != Some(1) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    pid
}

fn parent_of(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

/// Writes a registry record by hand: `<owner pid> <owner start> <child start>`.
fn write_record(dir: &Path, child_pid: u32, owner_pid: u32, owner_start: u64, child_start: u64) {
    std::fs::write(
        dir.join(child_pid.to_string()),
        format!("{owner_pid} {owner_start} {child_start}"),
    )
    .unwrap();
}

fn start_of(pid: u32) -> u64 {
    orphans::start_time(pid).expect("process start time")
}

/// True if a process with this pid exists (a zombie counts).
fn is_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Waits until `pid` is gone; returns false on timeout.
fn dies_within(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

fn kill_orphan(pid: u32) {
    // SAFETY: only sends a signal to a process this test started.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
}

#[test]
fn reap_kills_orphans_of_dead_owners_and_spares_live_ones() {
    let registry = tempfile::tempdir().unwrap();

    // Not named `claude`: the name does not matter, only pid + start time.
    let orphan = spawn_orphan();
    let mut owned = spawn_child();

    // The orphan's owner is a pid that is not running.
    write_record(registry.path(), orphan, DEAD_OWNER_PID, 1, start_of(orphan));
    // The other one is owned by this (live) test process.
    orphans::register_in(registry.path(), owned.id()).unwrap();
    // A corrupt record, and one in the old pid-only format, are removed without killing.
    std::fs::write(registry.path().join("not-a-pid"), "x").unwrap();
    std::fs::write(registry.path().join("12345"), DEAD_OWNER_PID.to_string()).unwrap();

    let killed = orphans::reap_in(registry.path());

    assert_eq!(killed, [orphan]);
    assert!(
        dies_within(orphan, Duration::from_secs(2)),
        "orphan still running"
    );
    assert!(!registry.path().join(orphan.to_string()).exists());
    assert!(!registry.path().join("not-a-pid").exists());
    assert!(!registry.path().join("12345").exists());

    // The live owner's process is untouched and still registered.
    assert!(
        owned.try_wait().unwrap().is_none(),
        "owned process was killed"
    );
    assert!(registry.path().join(owned.id().to_string()).exists());

    orphans::unregister_in(registry.path(), owned.id());
    assert!(!registry.path().join(owned.id().to_string()).exists());
    owned.kill().unwrap();
    owned.wait().unwrap();
}

#[test]
fn reap_treats_a_reused_owner_pid_as_a_dead_owner() {
    // The owner pid is running (it is this test), but it started at another time than the
    // record says: the pid was reused, so the real owner is gone and the orphan is reaped.
    let registry = tempfile::tempdir().unwrap();
    let orphan = spawn_orphan();
    let me = std::process::id();
    write_record(
        registry.path(),
        orphan,
        me,
        start_of(me) - 1,
        start_of(orphan),
    );

    assert_eq!(orphans::reap_in(registry.path()), [orphan]);
    assert!(dies_within(orphan, Duration::from_secs(2)));
}

#[test]
fn reap_never_kills_a_process_that_reused_the_child_pid() {
    // The record's pid is now a different process (other start time): leave it alone.
    let registry = tempfile::tempdir().unwrap();
    let stranger = spawn_orphan();
    write_record(
        registry.path(),
        stranger,
        DEAD_OWNER_PID,
        1,
        start_of(stranger) - 1,
    );

    assert!(orphans::reap_in(registry.path()).is_empty());
    assert!(
        is_alive(stranger),
        "a process that reused the pid was killed"
    );
    // The stale record is still removed.
    assert!(!registry.path().join(stranger.to_string()).exists());
    kill_orphan(stranger);
}

#[test]
fn reap_spares_a_child_that_still_has_a_parent() {
    // Pid and start time match, but the process has a live parent, so it is not an orphan
    // (for example it was handed to another process). It must survive.
    let registry = tempfile::tempdir().unwrap();
    let mut not_orphan = spawn_child();
    write_record(
        registry.path(),
        not_orphan.id(),
        DEAD_OWNER_PID,
        1,
        start_of(not_orphan.id()),
    );

    assert!(orphans::reap_in(registry.path()).is_empty());
    assert!(
        not_orphan.try_wait().unwrap().is_none(),
        "a process with a live parent was killed"
    );
    assert!(!registry.path().join(not_orphan.id().to_string()).exists());

    not_orphan.kill().unwrap();
    not_orphan.wait().unwrap();
}

#[test]
fn reap_with_no_registry_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(orphans::reap_in(&dir.path().join("missing")).is_empty());
}
