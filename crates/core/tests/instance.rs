use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use newjen_core::instance::InstanceLock;
use newjen_core::store;

struct Owner(Child);

impl Owner {
    fn start(directory: &Path) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_process_child", "--nocapture"])
            .env("NEWJEN_LOCK_TEST_HELPER", "hold")
            .env("NEWJEN_HOME", directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let owner = Self(child);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.unwrap().contains("LOCK_ACQUIRED") {
                    let _ = sender.send(());
                }
            }
        });
        receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("child acquired the lock");
        owner
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn reacquire(directory: &Path) -> InstanceLock {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(instance) = InstanceLock::try_acquire(directory).unwrap() {
            return instance;
        }
        assert!(Instant::now() < deadline, "ownership was not released");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn lock_process_child() {
    if std::env::var("NEWJEN_LOCK_TEST_HELPER").as_deref() != Ok("hold") {
        return;
    }
    let directory = store::default_dir().unwrap();
    let instance = InstanceLock::try_acquire(&directory).unwrap().unwrap();
    println!("LOCK_ACQUIRED");
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read(&mut [0]);
    drop(instance);
}

#[test]
fn one_owner_per_folder_until_drop() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("nested/home");
    let first = InstanceLock::try_acquire(&directory).unwrap().unwrap();
    assert_eq!(first.directory(), directory.canonicalize().unwrap());
    assert!(InstanceLock::try_acquire(&directory).unwrap().is_none());
    drop(first);
    assert!(directory.join("instance.lock").exists());
    drop(reacquire(&directory));
}

#[test]
fn stale_lock_file_is_reused_without_erasing_it() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("instance.lock");
    std::fs::write(&path, "previous owner").unwrap();
    let instance = InstanceLock::try_acquire(temp.path()).unwrap().unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "previous owner");
    assert!(InstanceLock::try_acquire(temp.path()).unwrap().is_none());
    drop(instance);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "previous owner");
}

#[test]
fn aliases_of_a_folder_share_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("home");
    let instance = InstanceLock::try_acquire(&directory).unwrap().unwrap();
    let alias = temp.path().join("alias");
    symlink(&directory, &alias).unwrap();
    assert!(InstanceLock::try_acquire(&alias).unwrap().is_none());
    drop(instance);
    drop(reacquire(&alias));
}

#[test]
fn different_newjen_homes_can_run_in_parallel() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let _first_owner = Owner::start(first.path());
    let _second_owner = Owner::start(second.path());
    assert!(InstanceLock::try_acquire(first.path()).unwrap().is_none());
    assert!(InstanceLock::try_acquire(second.path()).unwrap().is_none());
}

#[test]
fn process_death_releases_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = Owner::start(temp.path());
    assert!(InstanceLock::try_acquire(temp.path()).unwrap().is_none());
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert!(temp.path().join("instance.lock").exists());
    drop(reacquire(temp.path()));
}

#[test]
fn normal_process_exit_releases_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = Owner::start(temp.path());
    drop(owner.0.stdin.take());
    assert!(owner.0.wait().unwrap().success());
    drop(reacquire(temp.path()));
}

#[test]
fn spawned_program_does_not_keep_the_lock() {
    let temp = tempfile::tempdir().unwrap();
    let instance = InstanceLock::try_acquire(temp.path()).unwrap().unwrap();
    let mut child = Owner(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    drop(instance);
    assert!(child.0.try_wait().unwrap().is_none());
    drop(reacquire(temp.path()));
}

#[test]
fn filesystem_failures_are_errors_instead_of_contention() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("file");
    std::fs::write(&file, "not a folder").unwrap();
    assert!(InstanceLock::try_acquire(&file).is_err());
    std::fs::create_dir(temp.path().join("instance.lock")).unwrap();
    assert!(InstanceLock::try_acquire(temp.path()).is_err());
}
