//! Rift copies: a thread's own copy-on-write clone of the project, so agents that work at the
//! same time never touch each other's files. No UI.
//!
//! **Making one** ([`create`]): the folder copied is the jj root around the project, else its
//! git root, else the project folder itself, and `rift init` runs there once. In a jj repo the
//! main repo and the copy then never share a working-copy change, so fetching the copy's work
//! back later makes no divergent change:
//! 1. One snapshot of the main repo. If its `@` has changes, `jj new` there, so they are
//!    committed and the main repo goes on in a new empty change.
//! 2. The copy's work starts on the main repo's `@-`, the copy's [`Copy::base`].
//! 3. `rift create --copy-all`, which also copies gitignored files (`node_modules`, `target/`).
//! 4. `jj new <base>` in the copy. The copy's old `@` (the main repo's working-copy change) is
//!    abandoned there when it is empty.
//!
//! A step that fails after the copy exists removes it again.
//!
//! **Removing one** ([`remove`]): `rift remove` moves the copy into the `.trash` folder beside
//! it, and that one entry is then deleted here. `rift gc` is never run: it empties every Rift
//! trash on the Mac.
//!
//! Every jj read that is not meant to snapshot passes `--ignore-working-copy`.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::session::{find_program, for_each_line, is_executable};

/// The file Rift keeps a folder's id in, at the root of the project and of each copy.
const MARKER: &str = ".rift";

/// The folder `rift remove` moves a copy into, beside the copy.
const TRASH: &str = ".trash";

/// How many of the last stderr lines an error keeps (hook output, then Rift's own message).
const KEPT_LINES: usize = 20;

/// After a tool exits, how long to wait for the last lines it printed. Bounded, because a
/// process a hook left running can keep the pipes open.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// The `rift` and `jj` programs, and extra environment for both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub rift: PathBuf,
    pub jj: PathBuf,
    /// Mostly for tests: their own `HOME` gives them their own Rift registry.
    pub env: Vec<(String, String)>,
}

impl Tools {
    /// The installed `rift` and `jj` ([`find_rift`], [`find_jj`]).
    pub fn find() -> Tools {
        Tools {
            rift: find_rift(),
            jj: find_jj(),
            env: Vec::new(),
        }
    }
}

/// Finds `rift`, overridden by `$NEWJEN_RIFT`. The `rift` npm installs is a Node script, and an
/// app started from Finder may have no `node` on its `PATH`, so the native binary shipped
/// beside the script is used when it is there.
pub fn find_rift() -> PathBuf {
    let found = find_program("NEWJEN_RIFT", "rift", &[".bun/bin", ".npm-global/bin"]);
    native_rift(&found).unwrap_or(found)
}

/// Finds `jj`, overridden by `$NEWJEN_JJ`.
pub fn find_jj() -> PathBuf {
    find_program("NEWJEN_JJ", "jj", &[".cargo/bin"])
}

/// `<package>/prebuilds/darwin-<arch>/rift` for the npm package's `bin/rift.js`.
fn native_rift(found: &Path) -> Option<PathBuf> {
    let script = std::fs::canonicalize(found).ok()?;
    if !script.ends_with("bin/rift.js") {
        return None;
    }
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => return None,
    };
    let native = script
        .parent()?
        .parent()?
        .join(format!("prebuilds/darwin-{arch}/rift"));
    is_executable(&native).then_some(native)
}

/// A thread's Rift copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Copy {
    /// The copy's root folder.
    pub path: PathBuf,
    /// Where the thread works: the project's place inside the copy (`path` for a project at
    /// the root, `path/<subfolder>` for one below it).
    pub workdir: PathBuf,
    /// The folder it was copied from (the project's jj or git root).
    pub source: PathBuf,
    /// Rift's id for the copy, from its `.rift` file.
    pub rift_id: String,
    /// The jj commit the copy's work starts on. `None` when the project is not a jj repo.
    pub base: Option<String>,
}

impl Copy {
    /// What a copy thread's claude gets in its environment, so the commands it runs (a dev
    /// server, say) can tell copies apart, e.g. to pick a port.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("RIFT_ID".to_string(), self.rift_id.clone()),
            (
                "RIFT_SOURCE".to_string(),
                self.source.to_string_lossy().into_owned(),
            ),
        ]
    }
}

/// Why a copy could not be made or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyError {
    /// Something is already at the copy's path, perhaps left by an earlier try that could not
    /// clean up. `rift_id` is its Rift id, if it has one.
    Exists {
        path: PathBuf,
        rift_id: Option<String>,
    },
    /// Rift still lists a copy at this path, but its folder was deleted. Only `rift gc` makes
    /// Rift forget it, and that also empties every Rift trash on the Mac.
    Registered(PathBuf),
    /// A step failed. `detail` is the tool's own message, `output` the last lines it and Rift's
    /// hooks printed.
    Failed {
        command: String,
        detail: String,
        output: Vec<String>,
    },
}

impl fmt::Display for CopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CopyError::Exists { path, .. } => write!(f, "{} already exists", path.display()),
            CopyError::Registered(path) => write!(
                f,
                "Rift still lists a copy at {}, but its folder is gone. `rift gc` makes Rift \
                 forget it (and empties every Rift trash on this Mac).",
                path.display()
            ),
            CopyError::Failed {
                command, detail, ..
            } => write!(f, "`{command}` failed: {detail}"),
        }
    }
}

impl std::error::Error for CopyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    MainRepoReady,
    Line(String),
}

pub fn destination(project_dir: &Path, name: &str) -> Result<PathBuf, CopyError> {
    let project = std::fs::canonicalize(project_dir)
        .map_err(|error| failure("rift create", format!("{}: {error}", project_dir.display())))?;
    let (root, _) = project_root(&project);
    let (Some(parent), Some(root_name)) = (root.parent(), root.file_name()) else {
        return Err(failure(
            "rift create",
            "Newjen won't copy the whole disk".to_string(),
        ));
    };
    Ok(parent.join(".rifts").join(root_name).join(name))
}

pub fn check_destination(path: &Path) -> Result<(), CopyError> {
    if path.symlink_metadata().is_ok() {
        return Err(CopyError::Exists {
            path: path.to_path_buf(),
            rift_id: read_id(path),
        });
    }
    Ok(())
}

pub fn remove_pending(tools: &Tools, path: &Path, rift_id: Option<&str>) -> Result<(), CopyError> {
    if let Some(id) = rift_id {
        return remove(tools, path, id);
    }
    let parent = path
        .parent()
        .ok_or_else(|| failure("rift remove", "missing parent".to_string()))?;
    if path.symlink_metadata().is_ok() {
        check_directory(path, parent)?;
        match read_id(path) {
            Some(id) => remove(tools, path, &id)?,
            None => delete_directory(path)?,
        }
    }
    for entry in pending_trash(path)? {
        check_directory(&entry, &parent.join(TRASH))?;
        if let Some(id) = read_id(&entry)
            && entry.file_name()
                == Some(OsStr::new(&format!(
                    "{id}-{}",
                    path.file_name().expect("copy name").to_string_lossy()
                )))
        {
            remove(tools, path, &id)?;
        }
    }
    Ok(())
}

pub fn pending_is_gone(path: &Path) -> bool {
    matches!(path.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        && pending_trash(path).is_ok_and(|entries| entries.is_empty())
}

fn pending_trash(path: &Path) -> Result<Vec<PathBuf>, CopyError> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("rift remove", "missing parent".to_string()))?;
    let trash = parent.join(TRASH);
    if matches!(trash.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(Vec::new());
    }
    check_directory(&trash, parent)?;
    let suffix = format!(
        "-{}",
        path.file_name().expect("copy name").to_string_lossy()
    );
    let entries = std::fs::read_dir(&trash)
        .and_then(|entries| entries.collect::<std::io::Result<Vec<_>>>())
        .map_err(|error| failure("rift remove", format!("{}: {error}", trash.display())))?;
    Ok(entries
        .into_iter()
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(&suffix))
        .map(|entry| entry.path())
        .collect())
}

/// Makes a Rift copy of the project at `project_dir` named `name` (see the module docs).
/// Blocking: it can take seconds, more with Rift hooks. Each line Rift and its hooks print on
/// stderr is reported as [`Progress::Line`], after [`Progress::MainRepoReady`].
pub fn create(
    tools: &Tools,
    project_dir: &Path,
    name: &str,
    mut on_progress: impl FnMut(Progress),
) -> Result<Copy, CopyError> {
    create_controlled(
        tools,
        project_dir,
        name,
        &CopyControl::default(),
        |_| Ok(()),
        &mut on_progress,
    )
}

pub(crate) fn create_controlled(
    tools: &Tools,
    project_dir: &Path,
    name: &str,
    control: &CopyControl,
    mut on_created: impl FnMut(&str) -> Result<(), CopyError>,
    mut on_progress: impl FnMut(Progress),
) -> Result<Copy, CopyError> {
    let project = std::fs::canonicalize(project_dir)
        .map_err(|error| failure("rift create", format!("{}: {error}", project_dir.display())))?;
    let (root, jj) = project_root(&project);
    let home = std::env::var_os("HOME").and_then(|home| std::fs::canonicalize(home).ok());
    let (Some(parent), Some(root_name)) = (root.parent(), root.file_name()) else {
        return Err(failure(
            "rift create",
            "Newjen won't copy the whole disk".to_string(),
        ));
    };
    if home.as_deref() == Some(root.as_path()) {
        return Err(failure(
            "rift create",
            "Newjen won't copy your whole home folder".to_string(),
        ));
    }
    let rel = project
        .strip_prefix(&root)
        .expect("the root contains the project")
        .to_path_buf();
    let into = parent.join(".rifts").join(root_name);
    let path = destination(&project, name)?;
    check_destination(&path)?;

    if !root.join(MARKER).exists() {
        run(
            tools,
            &tools.rift,
            &[OsStr::new("init"), root.as_os_str()],
            &root,
            |_| {},
        )?;
    }
    let base = if jj {
        Some(prepare_main_repo(tools, &root)?)
    } else {
        None
    };
    on_progress(Progress::MainRepoReady);
    let created = run_controlled(
        tools,
        &tools.rift,
        &[
            OsStr::new("create"),
            OsStr::new("--copy-all"),
            OsStr::new("--name"),
            OsStr::new(name),
            OsStr::new("--into"),
            into.as_os_str(),
            root.as_os_str(),
        ],
        &root,
        |line| on_progress(Progress::Line(line.to_string())),
        Some(control),
    );
    if let Err(error) = created {
        if matches!(&error, CopyError::Failed { detail, .. } if detail.starts_with("rift directory already exists:"))
        {
            return Err(CopyError::Exists {
                path: path.clone(),
                rift_id: read_id(&path),
            });
        }
        if path.exists() {
            return Err(remove_after(tools, &path, None, error));
        }
        return Err(match error {
            CopyError::Failed { detail, .. }
                if detail.contains("overlaps another managed workspace") =>
            {
                CopyError::Registered(path)
            }
            error => error,
        });
    }

    let rift_id = read_id(&path);
    let mut finish = || -> Result<Copy, CopyError> {
        let rift_id = rift_id
            .clone()
            .ok_or_else(|| failure("rift create", format!("{} has no Rift id", path.display())))?;
        on_created(&rift_id)?;
        if let Some(base) = &base {
            run(
                tools,
                &tools.jj,
                &jj_args(&["new", base.as_str()]),
                &path,
                |_| {},
            )?;
        }
        Ok(Copy {
            workdir: path.join(&rel),
            path: path.clone(),
            source: root.clone(),
            rift_id,
            base: base.clone(),
        })
    };
    finish().map_err(|error| remove_after(tools, &path, rift_id.as_deref(), error))
}

/// Removes the copy at `path` whose Rift id is `rift_id` (see the module docs). Nothing to do
/// if it is already gone, so it can run again after a crash.
pub fn remove(tools: &Tools, path: &Path, rift_id: &str) -> Result<(), CopyError> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(failure(
            "rift remove",
            format!("{} is not a copy", path.display()),
        ));
    };
    if rift_id.is_empty() || !rift_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(failure(
            "rift remove",
            format!("{rift_id:?} is not a Rift id"),
        ));
    }
    let entry = parent
        .join(TRASH)
        .join(format!("{rift_id}-{}", name.to_string_lossy()));
    if path.symlink_metadata().is_ok() {
        check_directory(path, parent)?;
        match read_id(path) {
            None => delete_directory(path)?,
            Some(id) if id == rift_id => {
                check_trash(parent, &entry)?;
                run(
                    tools,
                    &tools.rift,
                    &[OsStr::new("remove"), path.as_os_str()],
                    parent,
                    |_| {},
                )?;
            }
            Some(_) => {}
        }
    }
    check_trash(parent, &entry)?;
    if entry.symlink_metadata().is_ok() && read_id(&entry).as_deref() == Some(rift_id) {
        delete_directory(&entry)?;
    }
    Ok(())
}

fn delete_directory(path: &Path) -> Result<(), CopyError> {
    std::fs::remove_dir_all(path).map_err(|error| {
        failure(
            "rift remove",
            format!("could not delete {}: {error}", path.display()),
        )
    })
}

fn check_trash(parent: &Path, entry: &Path) -> Result<(), CopyError> {
    let trash = parent.join(TRASH);
    if trash.symlink_metadata().is_ok() {
        check_directory(&trash, parent)?;
        if entry.symlink_metadata().is_ok() {
            check_directory(entry, &trash)?;
        }
    }
    Ok(())
}

fn check_directory(path: &Path, parent: &Path) -> Result<(), CopyError> {
    let safe = || -> std::io::Result<bool> {
        let metadata = path.symlink_metadata()?;
        Ok(!metadata.file_type().is_symlink()
            && metadata.is_dir()
            && path.canonicalize()?.parent() == Some(parent.canonicalize()?.as_path()))
    };
    if safe().unwrap_or(false) {
        Ok(())
    } else {
        Err(failure(
            "rift remove",
            format!("refusing unsafe folder {}", path.display()),
        ))
    }
}

/// The folder to copy for `project` (canonical), and whether it is a jj repo: the nearest
/// folder with a `.jj` in it, else the nearest with a `.git`, else `project`.
fn project_root(project: &Path) -> (PathBuf, bool) {
    let nearest = |marker: &str| {
        project
            .ancestors()
            .find(|dir| dir.join(marker).exists())
            .map(Path::to_path_buf)
    };
    match nearest(".jj") {
        Some(root) => (root, true),
        None => (
            nearest(".git").unwrap_or_else(|| project.to_path_buf()),
            false,
        ),
    }
}

/// Steps 1 and 2 of the module docs, in the main repo at `root`. Returns the base.
fn prepare_main_repo(tools: &Tools, root: &Path) -> Result<String, CopyError> {
    // Without `--ignore-working-copy`: this is the one snapshot.
    let state = run(
        tools,
        &tools.jj,
        &jj_args(&[
            "log",
            "--no-graph",
            "-r",
            "@",
            "-T",
            r#"if(empty && parents.len() == 1, "clean", "changed")"#,
        ]),
        root,
        |_| {},
    )?;
    if state != "clean" {
        run(tools, &tools.jj, &jj_args(&["new"]), root, |_| {})?;
    }
    run(
        tools,
        &tools.jj,
        &jj_args(&[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "@-",
            "-T",
            "commit_id",
        ]),
        root,
        |_| {},
    )
}

fn jj_args<'a>(args: &[&'a str]) -> Vec<&'a OsStr> {
    std::iter::once("--color=never")
        .chain(args.iter().copied())
        .map(OsStr::new)
        .collect()
}

/// The Rift id in `copy`'s `.rift` file.
fn read_id(copy: &Path) -> Option<String> {
    let marker = copy.join(MARKER);
    if !marker.symlink_metadata().ok()?.is_file() {
        return None;
    }
    let id = std::fs::read_to_string(marker).ok()?;
    Some(id.trim().to_string()).filter(|id| !id.is_empty())
}

/// `error`, after removing the half-made copy at `path`. If that fails too, the error says so.
fn remove_after(tools: &Tools, path: &Path, rift_id: Option<&str>, error: CopyError) -> CopyError {
    let removed = remove_pending(tools, path, rift_id);
    match (error, removed) {
        (error, Ok(())) => error,
        (
            CopyError::Failed {
                command,
                detail,
                output,
            },
            Err(cleanup),
        ) => CopyError::Failed {
            command,
            detail: format!("{detail}. Removing the half-made copy failed too: {cleanup}"),
            output,
        },
        (error, Err(_)) => error,
    }
}

fn failure(command: &str, detail: String) -> CopyError {
    CopyError::Failed {
        command: command.to_string(),
        detail,
        output: Vec::new(),
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CopyControl(std::sync::Arc<std::sync::Mutex<CopyProcess>>);

#[derive(Debug, Default)]
enum CopyProcess {
    #[default]
    Waiting,
    Running(u32),
    Cancelled,
}

impl CopyControl {
    pub(crate) fn cancel(&self) -> Option<thread::JoinHandle<()>> {
        let previous = std::mem::replace(
            &mut *self.0.lock().expect("copy process"),
            CopyProcess::Cancelled,
        );
        if let CopyProcess::Running(pid) = previous {
            Some(thread::spawn(move || stop_group(pid)))
        } else {
            None
        }
    }
}

fn stop_group(pid: u32) {
    if let Ok(group) = libc::pid_t::try_from(pid) {
        // SAFETY: the child started as the leader of this process group.
        unsafe {
            libc::killpg(group, libc::SIGTERM);
        }
        thread::sleep(Duration::from_millis(150));
        // SAFETY: finish stopping hooks that ignored SIGTERM.
        unsafe {
            libc::killpg(group, libc::SIGKILL);
        }
        crate::orphans::unregister(pid);
    }
}

/// One line a tool printed.
enum Printed {
    Out(String),
    Err(String),
}

/// Runs `program` with `args` in `cwd` and returns its stdout, trimmed. Each stderr line goes
/// to `on_line` as it arrives. Fails with the tool's last stderr line if it exits non-zero.
fn run(
    tools: &Tools,
    program: &Path,
    args: &[&OsStr],
    cwd: &Path,
    mut on_line: impl FnMut(&str),
) -> Result<String, CopyError> {
    run_controlled(tools, program, args, cwd, &mut on_line, None)
}

fn run_controlled(
    tools: &Tools,
    program: &Path,
    args: &[&OsStr],
    cwd: &Path,
    mut on_line: impl FnMut(&str),
    control: Option<&CopyControl>,
) -> Result<String, CopyError> {
    let program_name = program
        .file_name()
        .unwrap_or(program.as_os_str())
        .to_string_lossy();
    let command_text = std::iter::once(program_name.as_ref())
        .chain(args.iter().map(|arg| arg.to_str().unwrap_or("…")))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in &tools.env {
        command.env(name, value);
    }
    use std::os::unix::process::CommandExt;
    let mut process = control.map(|control| control.0.lock().expect("copy process"));
    if matches!(process.as_deref(), Some(CopyProcess::Cancelled)) {
        return Err(failure(
            &command_text,
            "Newjen stopped making the copy".to_string(),
        ));
    }
    if control.is_some() {
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|error| failure(&command_text, format!("could not run it: {error}")))?;

    if let Some(process) = process.as_deref_mut() {
        *process = CopyProcess::Running(child.id());
        let _ = crate::orphans::register(child.id());
    }
    drop(process);
    let (lines, printed) = mpsc::channel();
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let out_lines = lines.clone();
    thread::spawn(move || {
        for_each_line(stdout, |line| {
            let _ = out_lines.send(Printed::Out(line.to_string()));
        })
    });
    thread::spawn(move || {
        for_each_line(stderr, |line| {
            let _ = lines.send(Printed::Err(line.to_string()));
        })
    });

    let mut stdout = String::new();
    let mut kept = VecDeque::new();
    let mut handle = |printed: Printed| match printed {
        Printed::Out(line) => {
            stdout.push_str(&line);
            stdout.push('\n');
        }
        Printed::Err(line) => {
            on_line(&line);
            if kept.len() == KEPT_LINES {
                kept.pop_front();
            }
            kept.push_back(line);
        }
    };
    let status = loop {
        match printed.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => handle(line),
            Err(mpsc::RecvTimeoutError::Disconnected) => break child.wait(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().transpose() {
                    let deadline = Instant::now() + DRAIN_TIMEOUT;
                    while let Ok(line) =
                        printed.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    {
                        handle(line);
                    }
                    break status;
                }
            }
        }
    }
    .map_err(|error| failure(&command_text, error.to_string()))?;

    if let Some(control) = control {
        let mut process = control.0.lock().expect("copy process");
        if matches!(*process, CopyProcess::Running(_)) {
            stop_group(child.id());
            *process = CopyProcess::Waiting;
        }
    }
    if status.success() {
        return Ok(stdout.trim().to_string());
    }
    let output: Vec<String> = kept.into_iter().collect();
    let detail = output
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| status.to_string());
    Err(CopyError::Failed {
        command: command_text,
        detail,
        output,
    })
}
