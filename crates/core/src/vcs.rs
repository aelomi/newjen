use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use anyhow::{Context, Result, anyhow, bail, ensure};

use crate::changes::{self, ChangeSummary};
use crate::worktree::{Tools, Worktree};

pub const MAX_PATCH_BYTES: usize = 100_000;
const MAX_OUTPUT_BYTES: usize = 1_000_000;
const CHECKPOINT_AUTHOR: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Newjen"),
    ("GIT_AUTHOR_EMAIL", "newjen@localhost"),
    ("GIT_COMMITTER_NAME", "Newjen"),
    ("GIT_COMMITTER_EMAIL", "newjen@localhost"),
];

/// Finds `gh`, overridden by `$NEWJEN_GH`.
pub fn find_gh() -> PathBuf {
    crate::env::find_program("NEWJEN_GH", "gh", &[])
}

pub fn available(cwd: &Path) -> bool {
    cwd.ancestors().any(|path| path.join(".git").exists())
}

/// Records the thread's folder as a private commit under `refs/newjen/<thread id>/<n>`, the
/// next turn's checkpoint. The project's index, `HEAD` and branches are never touched, and
/// only the thread's folder is recorded. The first one is the thread's starting point.
pub fn checkpoint(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<String> {
    let ordinal = checkpoints(tools, cwd, thread_id)?.len();
    let tree = live_tree(tools, cwd, thread_id)?;
    let head = revision(tools, cwd, "HEAD").ok();
    let mut args = vec![
        "commit-tree".to_string(),
        tree,
        "-m".to_string(),
        format!("Newjen checkpoint {ordinal}"),
    ];
    if let Some(head) = &head {
        args.extend(["-p".to_string(), head.clone()]);
    }
    let mut command = git_command(tools, cwd);
    command.args(&args).envs(CHECKPOINT_AUTHOR);
    let id = String::from_utf8(execute(command, MAX_OUTPUT_BYTES)?.stdout)?
        .trim()
        .to_string();
    validate_commit(&id)?;
    git(
        tools,
        cwd,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "update-ref",
            &checkpoint_ref(thread_id, ordinal)?,
            &id,
        ],
    )?;
    Ok(id)
}

/// How many turns have a checkpoint after them (the first checkpoint is the start).
pub fn turns(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<usize> {
    Ok(checkpoints(tools, cwd, thread_id)?.len().saturating_sub(1))
}

/// Forgets a thread's checkpoints. Its files and commits stay as they are.
pub fn release(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<()> {
    for (name, _) in refs(tools, cwd, thread_id)? {
        git(
            tools,
            cwd,
            &["-c", "core.hooksPath=/dev/null", "update-ref", "-d", &name],
        )?;
    }
    let _ = std::fs::remove_file(index_path(tools, cwd, thread_id)?);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Everything the thread's folder gained since the thread started, committed or not.
    All,
    /// What is not committed yet.
    Uncommitted,
    /// One turn, from 1.
    Turn(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub patch: String,
    pub truncated: bool,
}

pub fn diff(tools: &Tools, cwd: &Path, thread_id: &str, scope: Scope) -> Result<Diff> {
    let (from, to) = range(tools, cwd, thread_id, scope)?;
    let mut command = git_command(tools, cwd);
    command.args(["diff", "--no-ext-diff", "--no-renames", &from, &to]);
    let output = execute(command, MAX_PATCH_BYTES)?;
    Ok(Diff {
        patch: String::from_utf8_lossy(&output.stdout).into_owned(),
        truncated: output.truncated,
    })
}

/// Per-file counts for a scope. The diff is counted as it streams, so a large one costs no
/// memory.
pub fn changed_files(
    tools: &Tools,
    cwd: &Path,
    thread_id: &str,
    scope: Scope,
) -> Result<ChangeSummary> {
    let (from, to) = range(tools, cwd, thread_id, scope)?;
    let mut command = git_command(tools, cwd);
    command
        .args([
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--unified=0",
            &from,
            &to,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("Cannot start git")?;
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let stderr = thread::spawn(move || read_bounded(err, 16_000));
    let counted = changes::count_git_diff(std::io::BufReader::new(out));
    if counted.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    let (stderr, _) = stderr
        .join()
        .map_err(|_| anyhow!("Command error reader failed"))??;
    ensure!(
        status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&stderr).trim()
    );
    Ok(counted?)
}

fn range(tools: &Tools, cwd: &Path, thread_id: &str, scope: Scope) -> Result<(String, String)> {
    ensure!(available(cwd), "Changes need a git repository");
    let checkpoints = checkpoints(tools, cwd, thread_id)?;
    Ok(match scope {
        Scope::All => {
            let start = checkpoints
                .first()
                .context("This thread has no starting checkpoint")?;
            (start.clone(), live_tree(tools, cwd, thread_id)?)
        }
        Scope::Uncommitted => {
            let head = revision(tools, cwd, "HEAD").or_else(|_| empty_tree(tools, cwd))?;
            (head, live_tree(tools, cwd, thread_id)?)
        }
        Scope::Turn(turn) => {
            ensure!(
                turn >= 1 && turn < checkpoints.len(),
                "This thread has no turn {turn}"
            );
            (checkpoints[turn - 1].clone(), checkpoints[turn].clone())
        }
    })
}

/// The branch and how it stands against its upstream, and whether the folder has uncommitted
/// changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: Option<String>,
    pub upstream: bool,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: bool,
    pub remote: bool,
    pub default_branch: bool,
}

pub fn status(tools: &Tools, cwd: &Path) -> Result<Status> {
    ensure!(available(cwd), "Changes need a git repository");
    let branch = git(tools, cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]).ok();
    let root = root(tools, cwd)?;
    let mut porcelain = git_command_with(tools, cwd, false);
    porcelain
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["status", "--porcelain", "--untracked-files=all", "--"])
        .args(pathspec(tools, cwd, None)?);
    let dirty = !execute(porcelain, MAX_OUTPUT_BYTES)?.stdout.is_empty();
    let remote = git(tools, &root, &["remote", "get-url", "origin"]).is_ok();
    let counts = git(
        tools,
        cwd,
        &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
    )
    .ok();
    let (behind, ahead) = counts
        .as_deref()
        .and_then(|counts| counts.split_once(char::is_whitespace))
        .map(|(behind, ahead)| {
            (
                behind.trim().parse().unwrap_or(0),
                ahead.trim().parse().unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));
    let default = git(
        tools,
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .and_then(|name| name.strip_prefix("origin/").map(str::to_string));
    let default_branch = match (&branch, default) {
        (Some(branch), Some(default)) => *branch == default,
        (Some(branch), None) => branch == "main" || branch == "master",
        (None, _) => false,
    };
    Ok(Status {
        upstream: counts.is_some(),
        branch,
        ahead,
        behind,
        dirty,
        remote,
        default_branch,
    })
}

/// Whether the branch has commits that no remote-tracking branch has.
pub fn unpushed(tools: &Tools, cwd: &Path) -> Result<bool> {
    if revision(tools, cwd, "HEAD").is_err() {
        return Ok(false);
    }
    let count = git(
        tools,
        cwd,
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    )?;
    Ok(count.trim().parse::<usize>().unwrap_or(0) > 0)
}

/// Whether a worktree thread's `HEAD` is already part of the branch the project has checked out.
pub fn merged_into_source(tools: &Tools, worktree: &Worktree) -> Result<bool> {
    let head = revision(tools, &worktree.path, "HEAD")?;
    succeeds(
        tools,
        &worktree.source,
        &["merge-base", "--is-ancestor", &head, "HEAD"],
    )
}

/// What `commit` would commit, for writing its message.
pub fn uncommitted_patch(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<Diff> {
    diff(tools, cwd, thread_id, Scope::Uncommitted)
}

/// Stages everything in the thread's folder and commits it on the current branch, with the
/// project's own hooks and identity.
pub fn commit(tools: &Tools, cwd: &Path, message: &str) -> Result<()> {
    ensure!(!message.trim().is_empty(), "Write a commit message first");
    ensure_on_branch(tools, cwd)?;
    add_all(tools, cwd, None)?;
    ensure!(
        !succeeds(tools, cwd, &["diff", "--cached", "--quiet"])?,
        "There is nothing to commit"
    );
    let mut commit = git_command_with(tools, cwd, false);
    commit
        .args(["commit", "--quiet", "-m", message, "--"])
        .args(pathspec(tools, cwd, None)?);
    execute(commit, MAX_OUTPUT_BYTES)?;
    Ok(())
}

/// Pushes the current branch, setting its upstream on `origin` the first time.
pub fn push(tools: &Tools, cwd: &Path) -> Result<()> {
    ensure_on_branch(tools, cwd)?;
    if git(tools, cwd, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_ok() {
        git(tools, cwd, &["push", "--quiet"])?;
    } else {
        git(
            tools,
            cwd,
            &["push", "--quiet", "--set-upstream", "origin", "HEAD"],
        )?;
    }
    Ok(())
}

/// Opens a pull request for the current branch with `gh`, and returns its URL.
pub fn create_pr(tools: &Tools, gh: &Path, cwd: &Path, title: &str, body: &str) -> Result<String> {
    let branch = git(tools, cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .context("Check out a branch before opening a pull request")?;
    let mut command = Command::new(gh);
    command
        .args([
            "pr", "create", "--head", &branch, "--title", title, "--body", body,
        ])
        .current_dir(cwd)
        .envs(tools.env.iter().map(|(key, value)| (key, value)));
    let output =
        execute(command, MAX_OUTPUT_BYTES).context("gh could not open the pull request")?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .rev()
        .find(|line| line.starts_with("https://"))
        .unwrap_or_default()
        .to_string())
}

/// Merges a worktree thread's branch into the branch the project has checked out. Its
/// uncommitted changes are not part of the merge; commit them first.
pub fn merge_worktree(tools: &Tools, worktree: &Worktree, title: &str) -> Result<()> {
    ensure!(
        !worktree.is_legacy(),
        "This thread works in a Rift copy from an older Newjen. Merge it by hand"
    );
    let source = &worktree.source;
    let head = revision(tools, &worktree.path, "HEAD")?;
    if succeeds(
        tools,
        source,
        &["merge-base", "--is-ancestor", &head, "HEAD"],
    )? {
        return Ok(());
    }
    ensure_on_branch(tools, source)?;
    ensure!(
        !succeeds(
            tools,
            source,
            &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"]
        )?,
        "The project is in the middle of a merge. Finish or abort it first"
    );
    ensure!(
        succeeds(
            tools,
            source,
            &["merge-tree", "--write-tree", "--no-messages", "HEAD", &head]
        )?,
        "Merging would conflict with the project. Resolve it in the worktree (merge the project's branch there), then merge again"
    );
    let message = format!("Merge {}: {title}", worktree.branch);
    if let Err(error) = git(
        tools,
        source,
        &["merge", "--no-ff", "--no-edit", "-m", &message, &head],
    ) {
        if succeeds(
            tools,
            source,
            &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"],
        )? {
            let _ = git(tools, source, &["merge", "--abort"]);
        }
        return Err(error.context(
            "The merge failed and was undone. Commit or stash the project's own changes to these files and try again",
        ));
    }
    Ok(())
}

/// Puts a worktree thread's files back to how they were before `turn`. Commits and `HEAD`
/// stay as they are, so a commit the turn made shows as an uncommitted undo.
pub fn revert_turn(tools: &Tools, worktree: &Worktree, thread_id: &str, turn: usize) -> Result<()> {
    let dir = &worktree.path;
    let checkpoints = checkpoints(tools, dir, thread_id)?;
    ensure!(
        turn >= 1 && turn < checkpoints.len(),
        "This thread has no turn {turn}"
    );
    let before = &checkpoints[turn - 1];
    git(
        tools,
        dir,
        &[
            "restore",
            "--source",
            before,
            "--staged",
            "--worktree",
            "--",
            ".",
        ],
    )?;
    git(tools, dir, &["clean", "-fd", "--quiet"])?;
    git(tools, dir, &["reset", "--quiet"])?;
    Ok(())
}

/// The thread's folder as a tree: every file git does not ignore, under the folder only, on
/// top of `HEAD`, built in the thread's own index.
fn live_tree(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<String> {
    let lock = index_lock(thread_id);
    let _held = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let root = root(tools, cwd)?;
    let index = index_path(tools, cwd, thread_id)?;
    let mut read = git_command(tools, &root);
    read.env("GIT_INDEX_FILE", &index);
    match revision(tools, cwd, "HEAD") {
        Ok(head) => read.args(["read-tree", &head]),
        Err(_) => read.args(["read-tree", "--empty"]),
    };
    execute(read, MAX_OUTPUT_BYTES)?;
    add_all(tools, cwd, Some(&index))?;
    let mut write = git_command(tools, &root);
    write.env("GIT_INDEX_FILE", &index).arg("write-tree");
    Ok(String::from_utf8(execute(write, MAX_OUTPUT_BYTES)?.stdout)?
        .trim()
        .to_string())
}

fn index_lock(thread_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(thread_id.to_string()).or_default().clone()
}

fn checkpoints(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<Vec<String>> {
    let mut found = refs(tools, cwd, thread_id)?;
    found.sort_by_key(|(name, _)| {
        name.rsplit('/')
            .next()
            .and_then(|ordinal| ordinal.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    Ok(found.into_iter().map(|(_, id)| id).collect())
}

fn refs(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<Vec<(String, String)>> {
    let prefix = format!("{}/", thread_refs(thread_id)?);
    let listed = git(
        tools,
        cwd,
        &["for-each-ref", "--format=%(refname) %(objectname)", &prefix],
    )?;
    listed
        .lines()
        .map(|line| {
            let (name, id) = line.split_once(' ').context("Unexpected git ref listing")?;
            validate_commit(id)?;
            Ok((name.to_string(), id.to_string()))
        })
        .collect()
}

fn thread_refs(thread_id: &str) -> Result<String> {
    uuid::Uuid::parse_str(thread_id).context("Invalid thread id")?;
    Ok(format!("refs/newjen/{thread_id}"))
}

fn checkpoint_ref(thread_id: &str, ordinal: usize) -> Result<String> {
    Ok(format!("{}/{ordinal}", thread_refs(thread_id)?))
}

/// Stages every file under `dir` except embedded git repositories (a nested clone or a fresh
/// `git init`), which git cannot snapshot as files.
fn add_all(tools: &Tools, dir: &Path, index: Option<&Path>) -> Result<()> {
    let mut add = git_command_with(tools, dir, false);
    if let Some(index) = index {
        add.env("GIT_INDEX_FILE", index);
    }
    add.args(["add", "--all", "--"])
        .args(pathspec(tools, dir, index)?);
    execute(add, MAX_OUTPUT_BYTES)?;
    Ok(())
}

/// `dir` without the embedded git repositories in it, as magic pathspecs.
fn pathspec(tools: &Tools, dir: &Path, index: Option<&Path>) -> Result<Vec<String>> {
    let mut others = git_command(tools, dir);
    if let Some(index) = index {
        others.env("GIT_INDEX_FILE", index);
    }
    others.args(["ls-files", "--others", "--exclude-standard", "-z"]);
    let output = execute(others, MAX_OUTPUT_BYTES)?;
    ensure!(!output.truncated, "Too many new folders to snapshot");
    let text = String::from_utf8(output.stdout).context("A new file name is not UTF-8")?;
    let mut spec = vec![".".to_string()];
    for entry in text.split('\0').filter(|entry| entry.ends_with('/')) {
        if dir.join(entry).join(".git").exists() {
            spec.push(format!(":(exclude,literal){entry}"));
        }
    }
    Ok(spec)
}

fn ensure_on_branch(tools: &Tools, dir: &Path) -> Result<()> {
    ensure!(
        succeeds(tools, dir, &["symbolic-ref", "--quiet", "HEAD"])?,
        "The project has no branch checked out (detached HEAD). Check out a branch, then land"
    );
    Ok(())
}

fn empty_tree(tools: &Tools, cwd: &Path) -> Result<String> {
    git(tools, cwd, &["hash-object", "-t", "tree", "--stdin"])
}

fn index_path(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<PathBuf> {
    thread_refs(thread_id)?;
    let path = git(
        tools,
        cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            &format!("newjen/{thread_id}.index"),
        ],
    )?;
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(path)
}

fn root(tools: &Tools, cwd: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        tools,
        cwd,
        &["rev-parse", "--show-toplevel"],
    )?))
}

fn validate_commit(value: &str) -> Result<()> {
    ensure!(
        (40..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid git commit id"
    );
    Ok(())
}

fn revision(tools: &Tools, cwd: &Path, name: &str) -> Result<String> {
    let value = git(
        tools,
        cwd,
        &["rev-parse", "--verify", &format!("{name}^{{commit}}")],
    )?;
    validate_commit(&value)?;
    Ok(value)
}

fn succeeds(tools: &Tools, cwd: &Path, args: &[&str]) -> Result<bool> {
    let mut command = git_command(tools, cwd);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let output = command.output().context("Cannot start git")?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn git(tools: &Tools, cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = git_command(tools, cwd);
    command.args(args);
    let output = execute(command, MAX_OUTPUT_BYTES)?;
    ensure!(!output.truncated, "git output is too large");
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .context("git returned non-UTF-8 output")
}

fn git_command(tools: &Tools, cwd: &Path) -> Command {
    git_command_with(tools, cwd, true)
}

fn git_command_with(tools: &Tools, cwd: &Path, literal: bool) -> Command {
    let mut command = Command::new(&tools.git);
    command.args([
        "-c",
        "color.ui=never",
        "-c",
        "core.quotepath=false",
        "--no-pager",
    ]);
    if literal {
        command.arg("--literal-pathspecs");
    }
    command
        .current_dir(cwd)
        .envs(tools.env.iter().map(|(key, value)| (key, value)));
    command
}

struct Output {
    stdout: Vec<u8>,
    truncated: bool,
}

fn read_bounded(mut stream: impl Read, limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let n = stream.read(&mut buffer)?;
        if n == 0 {
            return Ok((kept, truncated));
        }
        let retain = n.min(limit.saturating_sub(kept.len()));
        kept.extend_from_slice(&buffer[..retain]);
        truncated |= retain < n;
    }
}

fn execute(mut command: Command, limit: usize) -> Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .spawn()
        .with_context(|| format!("Cannot start {program}"))?;
    let out = child.stdout.take().expect("piped stdout");
    let err = child.stderr.take().expect("piped stderr");
    let stdout = thread::spawn(move || read_bounded(out, limit));
    let stderr = thread::spawn(move || read_bounded(err, 16_000));
    let status = child.wait()?;
    let (stdout, truncated) = stdout
        .join()
        .map_err(|_| anyhow!("Command output reader failed"))??;
    let (stderr, _) = stderr
        .join()
        .map_err(|_| anyhow!("Command error reader failed"))??;
    let stderr = String::from_utf8_lossy(&stderr);
    ensure!(status.success(), "{program} failed: {}", stderr.trim());
    Ok(Output { stdout, truncated })
}
