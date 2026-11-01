use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use newjen_core::instance::{Instance, InstanceLock, LaunchRequest, Startup};
use newjen_core::store;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};

struct Owner(Child);

impl Owner {
    fn start(directory: &Path) -> Self {
        Self::start_mode(directory, "hold")
    }

    fn start_mode(directory: &Path, mode: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock_process_child", "--nocapture"])
            .env("NEWJEN_LOCK_TEST_HELPER", mode)
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
    let mode = std::env::var("NEWJEN_LOCK_TEST_HELPER").unwrap_or_default();
    if mode != "hold" && mode != "serve" {
        return;
    }
    let directory = store::default_dir().unwrap();
    let lock = InstanceLock::try_acquire(&directory).unwrap().unwrap();
    let (server, hold) = if mode == "serve" {
        (Some(Instance::bind(lock).unwrap()), None)
    } else {
        (None, Some(lock))
    };
    println!("LOCK_ACQUIRED");
    std::io::stdout().flush().unwrap();
    if let Some(server) = server {
        loop {
            if let Some(request) = server.receive().unwrap() {
                std::fs::write(
                    directory.join("received.txt"),
                    format!("{:?}", request.request()),
                )
                .unwrap();
                request.accept().unwrap();
                break;
            }
        }
    } else {
        let _ = std::io::stdin().read(&mut [0]);
    }
    drop(hold);
}

#[test]
fn one_owner_per_folder_until_drop() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("nested/home");
    let first = InstanceLock::try_acquire(&directory).unwrap().unwrap();
    assert_eq!(first.directory(), directory.canonicalize().unwrap());
    assert!(InstanceLock::try_acquire(&directory).unwrap().is_none());
    drop(first);
    assert!(!directory.join("instance.lock").exists());
    drop(reacquire(&directory));
}

#[test]
fn stale_lock_file_is_left_alone() {
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
fn deleting_a_lock_file_cannot_admit_a_second_owner() {
    let temp = tempfile::tempdir().unwrap();
    let owner = Owner::start(temp.path());
    let path = temp.path().join("instance.lock");
    std::fs::write(&path, "stale lock").unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(InstanceLock::try_acquire(temp.path()).unwrap().is_none());
    drop(owner);
    drop(reacquire(temp.path()));
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
}

fn own(directory: &Path) -> Instance {
    match Instance::start(directory, &LaunchRequest::Activate).unwrap() {
        Startup::Owner(owner) => owner,
        Startup::Forwarded => panic!("expected a new owner"),
    }
}

fn forward(
    directory: &Path,
    request: LaunchRequest,
) -> std::thread::JoinHandle<anyhow::Result<Startup>> {
    let directory = directory.to_path_buf();
    std::thread::spawn(move || Instance::start(&directory, &request))
}

fn receive(owner: &Instance) -> newjen_core::instance::PendingLaunch {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if let Some(request) = owner.receive().unwrap() {
            return request;
        }
        assert!(Instant::now() < deadline, "launch was not received");
    }
}

#[test]
fn another_process_receives_activation_and_folder_launches() {
    for request in [
        LaunchRequest::Activate,
        LaunchRequest::OpenProject(std::env::temp_dir().join("project with spaces 日本語")),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let mut owner = Owner::start_mode(temp.path(), "serve");
        assert!(matches!(
            Instance::start(temp.path(), &request).unwrap(),
            Startup::Forwarded
        ));
        assert!(owner.0.wait().unwrap().success());
        assert_eq!(
            std::fs::read_to_string(temp.path().join("received.txt")).unwrap(),
            format!("{request:?}")
        );
    }
}

#[test]
fn acknowledgement_waits_for_the_app_to_accept_the_launch() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let sender = forward(temp.path(), LaunchRequest::Activate);
    let pending = receive(&owner);
    assert!(!sender.is_finished());
    pending.accept().unwrap();
    assert!(matches!(
        sender.join().unwrap().unwrap(),
        Startup::Forwarded
    ));
    assert!(owner.receive().unwrap().is_none());
}

#[test]
fn a_rejected_launch_is_not_retried_or_reported_as_forwarded() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let sender = forward(temp.path(), LaunchRequest::Activate);
    drop(receive(&owner));
    assert!(sender.join().unwrap().is_err());
    assert!(owner.receive().unwrap().is_none());
}

#[test]
fn a_sender_waits_through_the_lock_before_bind_race() {
    let temp = tempfile::tempdir().unwrap();
    let lock = InstanceLock::try_acquire(temp.path()).unwrap().unwrap();
    let sender = forward(temp.path(), LaunchRequest::Activate);
    std::thread::sleep(Duration::from_millis(50));
    let owner = Instance::bind(lock).unwrap();
    receive(&owner).accept().unwrap();
    assert!(matches!(
        sender.join().unwrap().unwrap(),
        Startup::Forwarded
    ));
}

#[test]
fn death_before_bind_allows_the_waiting_launcher_to_become_owner() {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = Owner::start(temp.path());
    let sender = forward(temp.path(), LaunchRequest::Activate);
    std::thread::sleep(Duration::from_millis(50));
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert!(matches!(sender.join().unwrap().unwrap(), Startup::Owner(_)));
}

#[test]
fn a_crashed_socket_is_replaced_by_the_next_owner() {
    let temp = tempfile::tempdir().unwrap();
    let mut previous = Owner::start_mode(temp.path(), "serve");
    previous.0.kill().unwrap();
    previous.0.wait().unwrap();
    assert!(temp.path().join("instance.sock").exists());
    let owner = own(temp.path());
    assert_eq!(
        std::fs::metadata(temp.path().join("instance.sock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(owner);
    assert!(!temp.path().join("instance.sock").exists());
}

#[test]
fn non_socket_files_and_symlinks_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("instance.sock");
    std::fs::write(&path, "keep this").unwrap();
    assert!(Instance::start(temp.path(), &LaunchRequest::Activate).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep this");
    std::fs::remove_file(&path).unwrap();
    let target = temp.path().join("target");
    std::fs::write(&target, "keep target").unwrap();
    symlink(&target, &path).unwrap();
    assert!(Instance::start(temp.path(), &LaunchRequest::Activate).is_err());
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn cleanup_preserves_a_replacement_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let path = temp.path().join("instance.sock");
    std::fs::remove_file(&path).unwrap();
    let replacement = UnixListener::bind(&path).unwrap();
    drop(owner);
    assert!(path.exists());
    drop(replacement);
}

#[test]
fn separate_homes_have_independent_launch_endpoints() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_owner = own(first.path());
    let second_owner = own(second.path());
    let sender = forward(second.path(), LaunchRequest::Activate);
    receive(&second_owner).accept().unwrap();
    assert!(matches!(
        sender.join().unwrap().unwrap(),
        Startup::Forwarded
    ));
    assert!(first_owner.receive().unwrap().is_none());
}

fn raw_frame(directory: &Path, bytes: &[u8]) -> UnixStream {
    let mut stream = UnixStream::connect(directory.join("instance.sock")).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    if bytes.len() <= 16 * 1024 {
        stream.write_all(bytes).unwrap();
    }
    stream
}

#[test]
fn malformed_oversized_versioned_and_invalid_path_requests_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    for payload in [
        b"invalid JSON".to_vec(),
        br#"{"version":2,"request":{"kind":"Activate"}}"#.to_vec(),
        br#"{"version":1,"request":{"kind":"Unknown"}}"#.to_vec(),
        br#"{"version":1,"request":{"kind":"OpenProject","path":[97]}}"#.to_vec(),
        br#"{"version":1,"request":{"kind":"OpenProject","path":[47,0]}}"#.to_vec(),
        vec![b'a'; 16 * 1024 + 1],
    ] {
        let _stream = raw_frame(temp.path(), &payload);
        assert!(owner.receive().is_err(), "accepted invalid payload");
    }
}

#[test]
fn native_non_unicode_path_bytes_survive_the_handoff() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let path =
        std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/project-\xff".to_vec()));
    let sender = forward(temp.path(), LaunchRequest::OpenProject(path.clone()));
    let pending = receive(&owner);
    assert_eq!(pending.request(), &LaunchRequest::OpenProject(path));
    pending.accept().unwrap();
    assert!(matches!(
        sender.join().unwrap().unwrap(),
        Startup::Forwarded
    ));
}

#[test]
fn a_slow_dribble_cannot_extend_the_total_receive_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let mut stream = UnixStream::connect(temp.path().join("instance.sock")).unwrap();
    stream.write_all(&10_u32.to_be_bytes()).unwrap();
    let start = Instant::now();
    let writer = std::thread::spawn(move || {
        for _ in 0..8 {
            if stream.write_all(b" ").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
    assert!(owner.receive().is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    writer.join().unwrap();
}

#[test]
fn a_hung_owner_cannot_make_the_second_launch_wait_forever() {
    let temp = tempfile::tempdir().unwrap();
    let _owner = own(temp.path());
    let start = Instant::now();
    assert!(Instance::start(temp.path(), &LaunchRequest::Activate).is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_relative_sender_path_is_rejected_before_delivery() {
    let temp = tempfile::tempdir().unwrap();
    assert!(Instance::start(temp.path(), &LaunchRequest::OpenProject("relative".into())).is_err());
    assert!(!temp.path().join("instance.sock").exists());
}

#[test]
fn expired_pending_launches_cannot_be_acknowledged() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let sender = forward(temp.path(), LaunchRequest::Activate);
    let pending = receive(&owner);
    std::thread::sleep(Duration::from_millis(1100));
    assert!(pending.is_expired());
    assert!(pending.accept().is_err());
    assert!(sender.join().unwrap().is_err());
}

#[test]
fn long_data_folder_paths_keep_the_socket_inside_the_owned_folder() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("project-home-".repeat(12));
    let owner = own(&directory);
    assert!(directory.join("instance.sock").exists());
    let sender = forward(&directory, LaunchRequest::Activate);
    receive(&owner).accept().unwrap();
    assert!(matches!(
        sender.join().unwrap().unwrap(),
        Startup::Forwarded
    ));
    drop(owner);
    assert!(!directory.join("instance.sock").exists());
}

#[test]
fn a_crashed_long_home_alias_is_reused_by_the_next_owner() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("long-crashed-home-".repeat(10));
    let mut previous = Owner::start_mode(&directory, "serve");
    previous.0.kill().unwrap();
    previous.0.wait().unwrap();
    let owner = own(&directory);
    let sender = forward(&directory, LaunchRequest::Activate);
    receive(&owner).accept().unwrap();
    assert!(matches!(
        sender.join().unwrap().unwrap(),
        Startup::Forwarded
    ));
}

#[test]
fn an_owner_that_quits_after_receiving_allows_the_launcher_to_take_over() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let sender = forward(temp.path(), LaunchRequest::OpenProject("/tmp".into()));
    drop(receive(&owner));
    std::thread::sleep(Duration::from_millis(50));
    drop(owner);
    assert!(matches!(sender.join().unwrap().unwrap(), Startup::Owner(_)));
}

#[test]
fn an_owner_that_quits_with_a_backlogged_launch_allows_takeover() {
    let temp = tempfile::tempdir().unwrap();
    let owner = own(temp.path());
    let sender = forward(temp.path(), LaunchRequest::Activate);
    std::thread::sleep(Duration::from_millis(100));
    drop(owner);
    assert!(matches!(sender.join().unwrap().unwrap(), Startup::Owner(_)));
}

#[test]
fn handoff_failures_are_distinct_from_first_owner_startup_errors() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("not-a-folder");
    std::fs::write(&file, "keep").unwrap();
    let error = Instance::start(&file, &LaunchRequest::Activate)
        .err()
        .unwrap();
    assert!(!error.is::<newjen_core::instance::HandoffError>());
    let owner = own(temp.path());
    let sender = forward(temp.path(), LaunchRequest::Activate);
    drop(receive(&owner));
    assert!(
        sender
            .join()
            .unwrap()
            .err()
            .unwrap()
            .is::<newjen_core::instance::HandoffError>()
    );
}
