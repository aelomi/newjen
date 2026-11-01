use std::ffi::OsString;
use std::fs::{File, Permissions, TryLockError};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// Exclusive ownership of a Newjen data folder until this guard is dropped or its process exits.
/// Locks the folder itself, so deleting a file inside it cannot release ownership.
#[derive(Debug)]
#[must_use = "keep the guard alive while using the data folder"]
pub struct InstanceLock {
    file: File,
    directory: PathBuf,
}

impl InstanceLock {
    /// Creates the data folder if needed and tries to own it without waiting.
    /// Returns `None` when another owner holds it, or an error when it cannot be protected.
    pub fn try_acquire(directory: &Path) -> Result<Option<Self>> {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("could not create {}", directory.display()))?;
        let directory = directory
            .canonicalize()
            .with_context(|| format!("could not resolve {}", directory.display()))?;
        let file = File::open(&directory)
            .with_context(|| format!("could not open {}", directory.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file, directory })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("could not lock {}", directory.display()))
            }
        }
    }

    /// The absolute, symlink-free data folder protected by this guard.
    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // A child between fork and exec can still hold the same open file description.
        let _ = self.file.unlock();
    }
}

const SOCKET_NAME: &str = "instance.sock";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_FRAME: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchRequest {
    Activate,
    OpenProject(PathBuf),
}

pub enum Startup {
    Owner(Instance),
    Forwarded,
}

/// A running owner could not acknowledge this launch before the handoff ended.
#[derive(Debug)]
pub struct HandoffError(anyhow::Error);

impl std::fmt::Display for HandoffError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, formatter)
    }
}

impl std::error::Error for HandoffError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        let problem: &(dyn std::error::Error + Send + Sync) = self.0.as_ref();
        problem.source()
    }
}

/// Owns the data folder and its private launch endpoint together.
pub struct Instance {
    listener: UnixListener,
    lock: InstanceLock,
    socket_identity: (u64, u64),
    alias: Option<SocketAlias>,
}

/// A launch awaits acknowledgement until the app has accepted it into its UI.
pub struct PendingLaunch {
    request: LaunchRequest,
    stream: UnixStream,
    deadline: Instant,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    version: u8,
    request: WireRequest,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum WireRequest {
    Activate,
    OpenProject { path: Vec<u8> },
}

impl LaunchRequest {
    fn message(&self) -> Result<Message> {
        let request = match self {
            Self::Activate => WireRequest::Activate,
            Self::OpenProject(path) => {
                validate_path(path)?;
                WireRequest::OpenProject {
                    path: path.as_os_str().as_bytes().to_vec(),
                }
            }
        };
        Ok(Message {
            version: 1,
            request,
        })
    }
}

impl Instance {
    /// Owns the folder or forwards this launch. Only an acknowledged handoff returns Forwarded.
    pub fn start(directory: &Path, request: &LaunchRequest) -> Result<Startup> {
        let payload = serde_json::to_vec(&request.message()?)?;
        ensure!(payload.len() <= MAX_FRAME, "launch request is too large");
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(lock) = InstanceLock::try_acquire(directory)? {
                return Self::bind(lock).map(Startup::Owner);
            }
            let socket = socket_address(&directory.canonicalize()?)?;
            match connect(&socket, deadline) {
                Ok(mut stream) => {
                    let delivered = (|| {
                        write_frame(&mut stream, &payload, deadline)
                            .context("could not send launch to the running Newjen")?;
                        let response = read_frame(&mut stream, deadline)
                            .context("the running Newjen did not acknowledge the launch")?;
                        ensure!(
                            response == br#"{"version":1,"accepted":true}"#,
                            "the running Newjen rejected the launch"
                        );
                        Ok::<_, anyhow::Error>(())
                    })();
                    match delivered {
                        Ok(()) => return Ok(Startup::Forwarded),
                        Err(problem) => {
                            while Instant::now() < deadline {
                                if let Some(lock) = InstanceLock::try_acquire(directory)? {
                                    return Self::bind(lock).map(Startup::Owner);
                                }
                                std::thread::sleep(Duration::from_millis(10));
                            }
                            return Err(HandoffError(problem).into());
                        }
                    }
                }
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(HandoffError(
                            error.context("could not reach the running Newjen; try again"),
                        )
                        .into());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Binds after locking, including recovery of an endpoint left by a crashed owner.
    pub fn bind(lock: InstanceLock) -> Result<Self> {
        let path = lock.directory().join(SOCKET_NAME);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.file_type().is_socket(),
                    "launch endpoint is occupied by a non-socket file"
                );
                std::fs::remove_file(&path)
                    .context("could not remove the stale launch endpoint")?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("could not inspect the launch endpoint"),
        }
        let address = socket_address(lock.directory())?;
        let alias = if address != path {
            Some(SocketAlias::prepare(
                address.parent().unwrap(),
                lock.directory(),
            )?)
        } else {
            None
        };
        let listener =
            UnixListener::bind(&address).context("could not bind the Newjen launch endpoint")?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let instance = Self {
            listener,
            lock,
            socket_identity: (metadata.dev(), metadata.ino()),
            alias,
        };
        std::fs::set_permissions(&path, Permissions::from_mode(0o600))?;
        instance.listener.set_nonblocking(true)?;
        Ok(instance)
    }

    pub fn directory(&self) -> &Path {
        self.lock.directory()
    }

    /// Waits briefly for a launch, with a bounded read even for an unresponsive sender.
    /// Call on a background executor. Malformed requests return errors without acknowledgement.
    pub fn receive(&self) -> Result<Option<PendingLaunch>> {
        let waiting = Instant::now() + Duration::from_millis(100);
        let (mut stream, _) = loop {
            match self.listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if !wait_ready(self.listener.as_raw_fd(), libc::POLLIN, waiting)? {
                        return Ok(None);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error).context("could not receive a Newjen launch"),
            }
        };
        stream.set_nonblocking(true)?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let message: Message = serde_json::from_slice(&read_frame(&mut stream, deadline)?)?;
        ensure!(message.version == 1, "unsupported launch protocol version");
        let request = match message.request {
            WireRequest::Activate => LaunchRequest::Activate,
            WireRequest::OpenProject { path } => {
                let path = PathBuf::from(OsString::from_vec(path));
                validate_path(&path)?;
                LaunchRequest::OpenProject(path)
            }
        };
        Ok(Some(PendingLaunch {
            request,
            stream,
            deadline,
        }))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        let path = self.directory().join(SOCKET_NAME);
        if std::fs::symlink_metadata(&path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && (metadata.dev(), metadata.ino()) == self.socket_identity
        }) {
            let _ = std::fs::remove_file(path);
        }
        drop(self.alias.take());
    }
}

impl PendingLaunch {
    pub fn request(&self) -> &LaunchRequest {
        &self.request
    }

    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub fn accept(mut self) -> Result<()> {
        write_frame(
            &mut self.stream,
            br#"{"version":1,"accepted":true}"#,
            self.deadline,
        )
    }
}

fn socket_address(directory: &Path) -> Result<PathBuf> {
    let path = directory.join(SOCKET_NAME);
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path.as_os_str().as_bytes().len() < address.sun_path.len() {
        return Ok(path);
    }
    let metadata = std::fs::metadata(directory)?;
    let user = unsafe { libc::geteuid() };
    Ok(PathBuf::from(format!("/tmp/newjen-launch-{user}"))
        .join(format!("{}-{}", metadata.dev(), metadata.ino()))
        .join(SOCKET_NAME))
}

// Darwin bounds the address string, even when the socket's directory itself is valid.
struct SocketAlias {
    path: PathBuf,
    directory: PathBuf,
}

impl SocketAlias {
    fn prepare(path: &Path, directory: &Path) -> Result<Self> {
        let parent = path.parent().unwrap();
        match std::fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("could not create the launch address folder"),
        }
        let metadata = std::fs::symlink_metadata(parent)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.permissions().mode() & 0o777 == 0o700,
            "launch address folder must be private and owned by this user"
        );
        match symlink(directory, path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                ensure!(
                    std::fs::read_link(path)? == directory,
                    "launch address points to another folder"
                );
            }
            Err(error) => return Err(error).context("could not shorten the launch address"),
        }
        Ok(Self {
            path: path.to_path_buf(),
            directory: directory.to_path_buf(),
        })
    }
}

impl Drop for SocketAlias {
    fn drop(&mut self) {
        if std::fs::read_link(&self.path).is_ok_and(|target| target == self.directory) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn validate_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "launch project path must be absolute");
    ensure!(
        !path.as_os_str().as_bytes().contains(&0),
        "launch project path contains a null byte"
    );
    Ok(())
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Result<Vec<u8>> {
    let mut size = [0; 4];
    transfer(stream, &mut size, deadline, |stream, bytes| {
        stream.read(bytes)
    })?;
    let size = u32::from_be_bytes(size) as usize;
    ensure!(size > 0 && size <= MAX_FRAME, "invalid launch message size");
    let mut payload = vec![0; size];
    transfer(stream, &mut payload, deadline, |stream, bytes| {
        stream.read(bytes)
    })?;
    Ok(payload)
}

fn write_frame(stream: &mut UnixStream, payload: &[u8], deadline: Instant) -> Result<()> {
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(payload);
    transfer(stream, &mut frame, deadline, |stream, bytes| {
        stream.write(bytes)
    })
}

fn transfer(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    mut operation: impl FnMut(&mut UnixStream, &mut [u8]) -> io::Result<usize>,
) -> Result<()> {
    while !bytes.is_empty() {
        ensure!(Instant::now() < deadline, "launch handoff timed out");
        match operation(stream, bytes) {
            Ok(0) => bail!("launch connection closed before acknowledgement"),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn connect(path: &Path, deadline: Instant) -> Result<UnixStream> {
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    ensure!(
        bytes.len() < address.sun_path.len(),
        "Newjen data folder path is too long for its launch endpoint"
    );
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    address.sun_len = std::mem::size_of_val(&address) as u8;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    // std's UnixStream::connect has no deadline; a full socket backlog can block it forever.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "could not protect launch socket from child processes"
    );
    stream.set_nonblocking(true)?;
    let connected = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if connected == 0 {
        return Ok(stream);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EINPROGRESS) {
        return Err(error.into());
    }
    ensure!(
        wait_ready(stream.as_raw_fd(), libc::POLLOUT, deadline)?,
        "launch connection timed out"
    );
    if let Some(error) = stream.take_error()? {
        return Err(error.into());
    }
    Ok(stream)
}

fn wait_ready(
    fd: std::os::fd::RawFd,
    events: libc::c_short,
    deadline: Instant,
) -> io::Result<bool> {
    let mut poll = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    loop {
        if Instant::now() >= deadline {
            return Ok(false);
        }
        let milliseconds = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
        if ready > 0 {
            return Ok(true);
        }
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_owner_releases_lock_before_a_forked_child_execs() {
        use std::os::unix::process::CommandExt as _;

        let temp = tempfile::tempdir().unwrap();
        let owner = InstanceLock::try_acquire(temp.path()).unwrap().unwrap();
        let (mut ready, child_ready) = UnixStream::pair().unwrap();
        let (mut release, child_release) = UnixStream::pair().unwrap();
        ready
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let spawning = std::thread::spawn(move || {
            let ready_fd = child_ready.as_raw_fd();
            let release_fd = child_release.as_raw_fd();
            let mut command = std::process::Command::new("/usr/bin/true");
            unsafe {
                command.pre_exec(move || {
                    let mut byte = 0_u8;
                    if libc::write(ready_fd, (&byte as *const u8).cast(), 1) != 1 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::read(release_fd, (&mut byte as *mut u8).cast(), 1) != 1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command.spawn()?.wait()
        });
        let started = ready.read_exact(&mut [0]);
        let held = InstanceLock::try_acquire(temp.path());
        drop(owner);
        let reacquired = InstanceLock::try_acquire(temp.path());
        let released = release.write_all(&[1]);
        let status = spawning.join().unwrap();

        started.unwrap();
        released.unwrap();
        assert!(status.unwrap().success());
        assert!(held.unwrap().is_none());
        assert!(reacquired.unwrap().is_some());
    }

    #[test]
    fn long_address_alias_is_private_and_removed_before_ownership_ends() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("long-home-".repeat(14));
        let Startup::Owner(owner) = Instance::start(&directory, &LaunchRequest::Activate).unwrap()
        else {
            panic!("expected owner")
        };
        let alias = owner.alias.as_ref().unwrap().path.clone();
        assert_eq!(
            std::fs::read_link(&alias).unwrap(),
            directory.canonicalize().unwrap()
        );
        assert_eq!(
            std::fs::metadata(alias.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(owner);
        assert!(std::fs::symlink_metadata(&alias).is_err());
        assert!(InstanceLock::try_acquire(&directory).unwrap().is_some());
    }

    #[test]
    fn a_short_address_never_replaces_an_unrelated_alias() {
        let temp = tempfile::tempdir().unwrap();
        let alias = temp.path().join("private").join("alias");
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let guard = SocketAlias::prepare(&alias, &target).unwrap();
        assert!(SocketAlias::prepare(&alias, temp.path()).is_err());
        assert_eq!(std::fs::read_link(&alias).unwrap(), target);
        drop(guard);
        std::fs::write(&alias, "keep").unwrap();
        assert!(SocketAlias::prepare(&alias, temp.path()).is_err());
        assert_eq!(std::fs::read_to_string(alias).unwrap(), "keep");
    }

    #[test]
    fn a_full_backlog_cannot_block_a_second_launch_forever() {
        let temp = tempfile::tempdir().unwrap();
        let Startup::Owner(owner) = Instance::start(temp.path(), &LaunchRequest::Activate).unwrap()
        else {
            panic!("expected owner")
        };
        assert_eq!(unsafe { libc::listen(owner.listener.as_raw_fd(), 1) }, 0);
        let path = owner.directory().join(SOCKET_NAME);
        let mut connections = Vec::new();
        let mut full = false;
        for _ in 0..8 {
            match connect(&path, Instant::now() + Duration::from_millis(100)) {
                Ok(stream) => connections.push(stream),
                Err(_) => {
                    full = true;
                    break;
                }
            }
        }
        assert!(full, "test did not fill the socket backlog");
        let start = Instant::now();
        assert!(Instance::start(temp.path(), &LaunchRequest::Activate).is_err());
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
