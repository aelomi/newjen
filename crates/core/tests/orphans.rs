//! The orphan registry kills `claude` processes whose Newjen owner is gone, and nothing else.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use newjen_core::orphans;

/// A pid that cannot belong to a running process (above macOS's pid limit of 99998).
const DEAD_OWNER_PID: &str = "999999";

/// A `claude` symlink to `/bin/sleep`, so the registry's "is this claude?" check passes.
fn fake_claude_link(dir: &std::path::Path) -> std::path::PathBuf {
    let link = dir.join("claude");
    if !link.exists() {
        std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
    }
    link
}

/// Starts `sleep 60` under the name `claude`, in its own process group like a real session.
/// It is a child of this test process, so it is *not* an orphan.
fn spawn_fake_claude(dir: &std::path::Path) -> Child {
    Command::new(fake_claude_link(dir))
        .arg("60")
        .process_group(0)
        .spawn()
        .expect("spawn sleep as claude")
}

/// Starts `sleep 60` under the name `claude` as a real orphan: a shell starts it in the
/// background and exits, so launchd (pid 1) becomes its parent, as after a Newjen crash.
fn spawn_orphaned_fake_claude(dir: &std::path::Path) -> u32 {
    let output = Command::new("/bin/sh")
        .args(["-c", r#""$0" 60 >/dev/null 2>&1 & echo $!"#])
        .arg(fake_claude_link(dir))
        .output()
        .expect("start orphan");
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .expect("orphan pid")
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

#[test]
fn reap_kills_orphans_of_dead_owners_and_spares_live_ones() {
    let bin_dir = tempfile::tempdir().unwrap();
    let registry = tempfile::tempdir().unwrap();

    let orphan = spawn_orphaned_fake_claude(bin_dir.path());
    let mut owned = spawn_fake_claude(bin_dir.path());

    // The orphan's owner is a pid that is not running.
    std::fs::write(registry.path().join(orphan.to_string()), DEAD_OWNER_PID).unwrap();
    // The other one is owned by this (live) test process.
    orphans::register_in(registry.path(), owned.id()).unwrap();
    // A corrupt record is cleaned up.
    std::fs::write(registry.path().join("not-a-pid"), "x").unwrap();

    let killed = orphans::reap_in(registry.path());

    assert_eq!(killed, [orphan]);
    assert!(
        dies_within(orphan, Duration::from_secs(2)),
        "orphan still running"
    );
    assert!(!registry.path().join(orphan.to_string()).exists());
    assert!(!registry.path().join("not-a-pid").exists());

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
fn reap_never_kills_a_process_that_is_not_claude() {
    let registry = tempfile::tempdir().unwrap();
    let mut other = Command::new("/bin/sleep")
        .arg("60")
        .process_group(0)
        .spawn()
        .unwrap();
    std::fs::write(registry.path().join(other.id().to_string()), DEAD_OWNER_PID).unwrap();

    assert!(orphans::reap_in(registry.path()).is_empty());
    assert!(
        other.try_wait().unwrap().is_none(),
        "a non-claude process was killed"
    );
    // The stale record is still removed.
    assert!(!registry.path().join(other.id().to_string()).exists());

    other.kill().unwrap();
    other.wait().unwrap();
}

#[test]
fn reap_spares_a_claude_that_still_has_a_parent() {
    // A stale record whose pid now belongs to someone else's `claude` (for example one running
    // in a terminal): the name matches, but it is not an orphan, so it must survive.
    let bin_dir = tempfile::tempdir().unwrap();
    let registry = tempfile::tempdir().unwrap();
    let mut not_ours = spawn_fake_claude(bin_dir.path());
    std::fs::write(
        registry.path().join(not_ours.id().to_string()),
        DEAD_OWNER_PID,
    )
    .unwrap();

    assert!(orphans::reap_in(registry.path()).is_empty());
    assert!(
        not_ours.try_wait().unwrap().is_none(),
        "a claude with a live parent was killed"
    );
    assert!(!registry.path().join(not_ours.id().to_string()).exists());

    not_ours.kill().unwrap();
    not_ours.wait().unwrap();
}

#[test]
fn reap_with_no_registry_does_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(orphans::reap_in(&dir.path().join("missing")).is_empty());
}
