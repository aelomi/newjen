use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::env::find_program;
use crate::session::for_each_line;

const FOLDER: &str = ".newjen-worktrees";

pub const BRANCH_PREFIX: &str = "newjen/";

const KEPT_LINES: usize = 20;

const DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tools {
    pub git: PathBuf,
    pub env: Vec<(String, String)>,
}

impl Tools {
    pub fn find() -> Tools {
        Tools {
            git: find_git(),
            env: Vec::new(),
        }
    }

    pub fn with_env(mut self, env: impl IntoIterator<Item = (String, String)>) -> Tools {
        self.env.extend(env);
        self
    }
}

pub fn find_git() -> PathBuf {
    find_program("NEWJEN_GIT", "git", &[])
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Where the thread works: the project's place inside the worktree (`path` for a project
    /// at the repository root, `path/<subfolder>` for one below it).
    pub workdir: PathBuf,
    pub source: PathBuf,
    pub branch: String,
    pub base: String,
}

impl Worktree {
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("NEWJEN_WORKTREE".to_string(), self.branch.clone()),
            (
                "NEWJEN_SOURCE".to_string(),
                self.source.to_string_lossy().into_owned(),
            ),
        ]
    }

    /// A Rift copy from an older Newjen: kept on disk, never landed or removed by Newjen.
    pub fn is_legacy(&self) -> bool {
        self.branch.is_empty()
    }
}

pub fn branch_name(name: &str) -> String {
    format!(
        "{BRANCH_PREFIX}{}",
        name.strip_prefix("newjen-").unwrap_or(name)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyError {
    Exists {
        path: PathBuf,
    },
    /// A step failed. `detail` is the tool's own message, `output` the last lines it printed.
    Failed {
        command: String,
        detail: String,
        output: Vec<String>,
    },
    /// The user cancelled while the worktree was made. Whatever was made is removed again.
    Cancelled,
}

impl fmt::Display for CopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CopyError::Exists { path } => write!(f, "{} already exists", path.display()),
            CopyError::Failed {
                command, detail, ..
            } => write!(f, "`{command}` failed: {detail}"),
            CopyError::Cancelled => write!(f, "Newjen stopped making the worktree"),
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
    let project = std::fs::canonicalize(project_dir).map_err(|error| {
        failure(
            "git worktree add",
            format!("{}: {error}", project_dir.display()),
        )
    })?;
    let root = project_root(&project);
    let (Some(parent), Some(root_name)) = (root.parent(), root.file_name()) else {
        return Err(failure(
            "git worktree add",
            "Newjen won't make a worktree of the whole disk".to_string(),
        ));
    };
    Ok(parent.join(FOLDER).join(root_name).join(name))
}

pub fn check_destination(path: &Path) -> Result<(), CopyError> {
    if path.symlink_metadata().is_ok() {
        return Err(CopyError::Exists {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

pub fn available(project_dir: &Path) -> bool {
    std::fs::canonicalize(project_dir)
        .is_ok_and(|project| project_root(&project).join(".git").exists())
}

pub fn remove_pending(tools: &Tools, project: &Path, path: &Path) -> Result<(), CopyError> {
    check_copy_path(project, path)?;
    let name = path
        .file_name()
        .ok_or_else(|| failure("git worktree remove", "missing name".to_string()))?
        .to_string_lossy()
        .into_owned();
    remove_folder(tools, project, path)?;
    let branch = branch_name(&name);
    if branch_exists(tools, project, &branch) {
        run(
            tools,
            &git_args(&["branch", "-D", &branch]),
            project,
            |_| {},
        )?;
    }
    Ok(())
}

pub fn pending_is_gone(path: &Path) -> bool {
    matches!(path.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

/// Makes a worktree of the project at `project_dir` named `name` (see the module docs).
pub fn create(
    tools: &Tools,
    project_dir: &Path,
    name: &str,
    mut on_progress: impl FnMut(Progress),
) -> Result<Worktree, CopyError> {
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
) -> Result<Worktree, CopyError> {
    if control.cancelled() {
        return Err(CopyError::Cancelled);
    }
    let project = std::fs::canonicalize(project_dir).map_err(|error| {
        failure(
            "git worktree add",
            format!("{}: {error}", project_dir.display()),
        )
    })?;
    let root = project_root(&project);
    if !root.join(".git").exists() {
        return Err(failure(
            "git worktree add",
            "Worktrees need a git repository. Run `git init` and make a first commit".to_string(),
        ));
    }
    let rel = project
        .strip_prefix(&root)
        .expect("the root contains the project")
        .to_path_buf();
    let path = destination(&project, name)?;
    check_destination(&path)?;
    let base = run(
        tools,
        &git_args(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
        &root,
        |_| {},
    )
    .map_err(|_| {
        failure(
            "git worktree add",
            "Worktrees need a first commit in the repository".to_string(),
        )
    })?;
    on_progress(Progress::MainRepoReady);
    let parent = path.parent().expect("worktree destination parent");
    std::fs::create_dir_all(parent)
        .map_err(|error| failure("git worktree add", format!("{}: {error}", parent.display())))?;
    let branch = branch_name(name);
    if branch_exists(tools, &root, &branch) {
        return Err(failure(
            "git worktree add",
            format!("a branch named {branch} already exists"),
        ));
    }
    let created = run_controlled(
        tools,
        &[
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-b"),
            OsStr::new(&branch),
            path.as_os_str(),
            OsStr::new(&base),
        ],
        &root,
        |line| on_progress(Progress::Line(line.to_string())),
        Some(control),
    );
    if let Err(error) = created {
        return Err(remove_after(tools, &root, &path, error));
    }
    let finish = on_created(&branch).map(|()| Worktree {
        workdir: path.join(&rel),
        path: path.clone(),
        source: root.clone(),
        branch,
        base,
    });
    let worktree = finish.map_err(|error| remove_after(tools, &root, &path, error))?;
    if control.cancelled() {
        return Err(remove_after(tools, &root, &path, CopyError::Cancelled));
    }
    Ok(worktree)
}

pub fn remove(tools: &Tools, project: &Path, path: &Path, branch: &str) -> Result<(), CopyError> {
    if branch.is_empty() {
        return Ok(());
    }
    check_copy_path(project, path)?;
    if !branch.starts_with(BRANCH_PREFIX) {
        return Err(failure(
            "git worktree remove",
            format!("{branch:?} is not a Newjen branch"),
        ));
    }
    remove_folder(tools, project, path)?;
    if branch_exists(tools, project, branch) {
        let _ = run(tools, &git_args(&["branch", "-d", branch]), project, |_| {});
    }
    Ok(())
}

fn remove_folder(tools: &Tools, project: &Path, path: &Path) -> Result<(), CopyError> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("git worktree remove", "missing parent".to_string()))?;
    if path.symlink_metadata().is_ok() {
        check_directory(path, parent)?;
        let removed = run(
            tools,
            &[
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                OsStr::new("--force"),
                path.as_os_str(),
            ],
            project,
            |_| {},
        );
        if removed.is_err() && path.symlink_metadata().is_ok() {
            delete_directory(path)?;
        }
    }
    run(tools, &git_args(&["worktree", "prune"]), project, |_| {})?;
    Ok(())
}

fn branch_exists(tools: &Tools, project: &Path, branch: &str) -> bool {
    run(
        tools,
        &git_args(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ]),
        project,
        |_| {},
    )
    .is_ok()
}

fn delete_directory(path: &Path) -> Result<(), CopyError> {
    std::fs::remove_dir_all(path).map_err(|error| {
        failure(
            "git worktree remove",
            format!("could not delete {}: {error}", path.display()),
        )
    })
}

fn check_copy_path(project: &Path, path: &Path) -> Result<(), CopyError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let expected = destination(project, name)?;
    if path != expected {
        return Err(failure(
            "git worktree remove",
            format!("refusing unexpected copy {}", path.display()),
        ));
    }
    let anchor = expected
        .ancestors()
        .nth(3)
        .expect("copy destination parent");
    let mut parent = anchor.to_path_buf();
    for component in expected
        .strip_prefix(anchor)
        .expect("copy destination")
        .components()
    {
        let folder = parent.join(component);
        match folder.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            _ => check_directory(&folder, &parent)?,
        }
        parent = folder;
    }
    Ok(())
}

fn check_directory(path: &Path, parent: &Path) -> Result<(), CopyError> {
    let safe = || -> std::io::Result<bool> {
        let metadata = path.symlink_metadata()?;
        Ok(!metadata.file_type().is_symlink()
            && metadata.is_dir()
            && parent.canonicalize()? == parent
            && path.canonicalize()? == path)
    };
    if safe().unwrap_or(false) {
        Ok(())
    } else {
        Err(failure(
            "git worktree remove",
            format!("refusing unsafe folder {}", path.display()),
        ))
    }
}

pub fn project_root(project: &Path) -> PathBuf {
    project
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map_or_else(|| project.to_path_buf(), Path::to_path_buf)
}

fn git_args<'a>(args: &[&'a str]) -> Vec<&'a OsStr> {
    args.iter().copied().map(OsStr::new).collect()
}

fn remove_after(tools: &Tools, project: &Path, path: &Path, error: CopyError) -> CopyError {
    let removed = remove_pending(tools, project, path);
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
            detail: format!("{detail}. Removing the half-made worktree failed too: {cleanup}"),
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
    fn cancelled(&self) -> bool {
        matches!(
            *self.0.lock().expect("copy process"),
            CopyProcess::Cancelled
        )
    }

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

enum Printed {
    Out(String),
    Err(String),
}

fn run(
    tools: &Tools,
    args: &[&OsStr],
    cwd: &Path,
    mut on_line: impl FnMut(&str),
) -> Result<String, CopyError> {
    run_controlled(tools, args, cwd, &mut on_line, None)
}

fn run_controlled(
    tools: &Tools,
    args: &[&OsStr],
    cwd: &Path,
    mut on_line: impl FnMut(&str),
    control: Option<&CopyControl>,
) -> Result<String, CopyError> {
    let command_text = std::iter::once("git")
        .chain(args.iter().map(|arg| arg.to_str().unwrap_or("…")))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command = Command::new(&tools.git);
    command
        .args(["-c", "color.ui=never"])
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
        return Err(CopyError::Cancelled);
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
        if matches!(*process, CopyProcess::Cancelled) {
            return Err(CopyError::Cancelled);
        }
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
