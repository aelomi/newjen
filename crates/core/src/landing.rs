use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, anyhow, bail, ensure};

use crate::changes::{self, ChangeSummary};
use crate::worktree::{Tools, Worktree};

pub const MAX_PATCH_BYTES: usize = 100_000;
const MAX_OUTPUT_BYTES: usize = 1_000_000;
const CHECKS_FILE: &str = ".newjen/checks.json";

pub fn instructions() -> &'static str {
    "Newjen manages version control for this thread. Do not commit, stage, stash, rebase, merge, create branches or tags, or otherwise write git metadata. Make file changes only. Newjen snapshots the folder after each turn."
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub base: String,
    pub head: String,
    pub patch: String,
    pub truncated: bool,
}

pub fn available(cwd: &Path) -> bool {
    cwd.ancestors().any(|path| path.join(".git").exists())
}

pub fn head(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<String> {
    match project_ref(tools, cwd, thread_id)? {
        Some(snapshot) => Ok(snapshot),
        None => revision(tools, cwd, "HEAD"),
    }
}

/// Records the project folder's files as a project-folder thread's base, before the agent
/// changes anything. Returns the base commit.
pub fn prepare_project(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<String> {
    ensure!(available(cwd), "Review and landing need a git repository");
    let name = reference(thread_id)?;
    let parent = revision(tools, cwd, "HEAD").ok();
    let index = index_path(tools, cwd, thread_id)?;
    let _ = std::fs::remove_file(&index);
    let tree = match &parent {
        Some(parent) => snapshot_tree(tools, cwd, &index, parent)?,
        None => snapshot_tree(tools, cwd, &index, &empty_tree(tools, cwd)?)?,
    };
    let base = commit_tree(tools, cwd, &tree, parent.as_deref(), &marker(thread_id)?)?;
    git(tools, cwd, &["update-ref", &name, &base])?;
    Ok(base)
}

/// Records what the agent changed this turn: a commit on a worktree's branch, or a snapshot
/// under `refs/newjen/<thread id>` for the project folder. Nothing when nothing changed.
pub fn snapshot(tools: &Tools, cwd: &Path, title: &str, thread_id: &str) -> Result<()> {
    let message = format!("{}\n\n{title}", marker(thread_id)?);
    if let Some(previous) = project_ref(tools, cwd, thread_id)? {
        let index = index_path(tools, cwd, thread_id)?;
        let tree = snapshot_tree(tools, cwd, &index, &previous)?;
        if tree == git(tools, cwd, &["rev-parse", &format!("{previous}^{{tree}}")])? {
            return Ok(());
        }
        let next = commit_tree(tools, cwd, &tree, Some(&previous), &message)?;
        git(
            tools,
            cwd,
            &["update-ref", &reference(thread_id)?, &next, &previous],
        )?;
        return Ok(());
    }
    ensure!(
        git(tools, cwd, &["symbolic-ref", "--quiet", "HEAD"])
            .is_ok_and(|branch| branch.starts_with("refs/heads/newjen/")),
        "This thread's recorded base is missing, so Newjen won't snapshot the project folder"
    );
    let root = root(tools, cwd)?;
    add_all(tools, &root, None)?;
    if succeeds(tools, cwd, &["diff", "--cached", "--quiet"])? {
        return Ok(());
    }
    commit(
        tools,
        cwd,
        &["commit", "--quiet", "--no-verify", "-m", &message],
    )?;
    Ok(())
}

/// Records the folder as it is now, then diffs it against `base`, so edits made after the last
/// turn are part of the review.
pub fn review(tools: &Tools, cwd: &Path, thread_id: &str, base: &str) -> Result<Review> {
    validate_commit(base)?;
    snapshot(tools, cwd, "Changes made outside a turn", thread_id)?;
    diff(tools, cwd, thread_id, base)
}

fn diff(tools: &Tools, cwd: &Path, thread_id: &str, base: &str) -> Result<Review> {
    ensure!(available(cwd), "Review and landing need a git repository");
    validate_commit(base)?;
    let head = head(tools, cwd, thread_id)?;
    ensure_ancestor(tools, cwd, base, &head)?;
    let mut command = git_command(tools, cwd);
    command.args(["diff", "--no-ext-diff", "--no-renames", base, &head]);
    let output = execute(command, MAX_PATCH_BYTES)?;
    let patch = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(Review {
        base: base.to_string(),
        head,
        patch,
        truncated: output.truncated,
    })
}

/// Per-file counts of everything the thread changed since `base`, up to the last snapshot.
pub fn changed_files(
    tools: &Tools,
    cwd: &Path,
    thread_id: &str,
    base: &str,
) -> Result<ChangeSummary> {
    ensure!(available(cwd), "Review and landing need a git repository");
    validate_commit(base)?;
    let head = head(tools, cwd, thread_id)?;
    ensure_ancestor(tools, cwd, base, &head)?;
    let mut command = git_command(tools, cwd);
    command.args([
        "diff",
        "--no-ext-diff",
        "--no-renames",
        "--unified=0",
        base,
        &head,
    ]);
    command
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

pub fn land_worktree(
    tools: &Tools,
    worktree: &Worktree,
    thread_id: &str,
    title: &str,
    reviewed: &Review,
) -> Result<()> {
    ensure!(
        !worktree.is_legacy(),
        "This thread works in a Rift copy from an older Newjen. Land it by hand"
    );
    ensure!(
        worktree.base == reviewed.base,
        "The review belongs to another worktree"
    );
    ensure_review(tools, &worktree.path, thread_id, reviewed)?;
    run_checks(tools, &worktree.source, &worktree.workdir)?;
    ensure_review(tools, &worktree.path, thread_id, reviewed)?;
    let source = &worktree.source;
    if succeeds(
        tools,
        source,
        &["merge-base", "--is-ancestor", &reviewed.head, "HEAD"],
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
            &[
                "merge-tree",
                "--write-tree",
                "--no-messages",
                "HEAD",
                &reviewed.head
            ]
        )?,
        "Landing would conflict with the project. Resolve the changes in the worktree and review again"
    );
    let message = format!("Land {}: {title}", worktree.branch);
    let merged = commit(
        tools,
        source,
        &[
            "merge",
            "--no-ff",
            "--no-verify",
            "--no-edit",
            "-m",
            &message,
            &reviewed.head,
        ],
    );
    if let Err(error) = merged {
        if succeeds(
            tools,
            source,
            &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"],
        )? {
            let _ = git(tools, source, &["merge", "--abort"]);
        }
        return Err(error.context(
            "Landing could not merge into the project. Its files are unchanged; commit or stash the project's own changes to these files and try again",
        ));
    }
    Ok(())
}

/// Commits a project-folder thread's reviewed files on the project's current branch. Refuses
/// when any of them also had uncommitted changes from before the thread, which git cannot
/// separate.
pub fn land_project(
    tools: &Tools,
    cwd: &Path,
    thread_id: &str,
    title: &str,
    reviewed: &Review,
) -> Result<()> {
    ensure_review(tools, cwd, thread_id, reviewed)?;
    run_checks(tools, cwd, cwd)?;
    ensure_review(tools, cwd, thread_id, reviewed)?;
    let root = root(tools, cwd)?;
    ensure_on_branch(tools, &root)?;
    let changed = changed_paths(tools, &root, &reviewed.base, &reviewed.head)?;
    if !changed.is_empty() {
        let paths: Vec<&str> = changed.iter().map(|change| change.path.as_str()).collect();
        let head = revision(tools, &root, "HEAD").or_else(|_| empty_tree(tools, &root))?;
        let mut compare = vec![
            "diff",
            "--quiet",
            "--no-renames",
            &head,
            &reviewed.base,
            "--",
        ];
        compare.extend(&paths);
        let mut staged = vec!["diff", "--cached", "--quiet", "--no-renames", "--"];
        staged.extend(&paths);
        ensure!(
            succeeds(tools, &root, &compare)? && succeeds(tools, &root, &staged)?,
            "Some of these files had other uncommitted changes. Commit them in your terminal, or choose Keep files and close"
        );
        let mut add = vec!["add", "--all", "--"];
        add.extend(&paths);
        git(tools, &root, &add)?;
        let message = if title.trim().is_empty() {
            "Newjen thread"
        } else {
            title
        };
        let mut commit_args = vec![
            "commit",
            "--quiet",
            "--no-verify",
            "-m",
            message,
            "--only",
            "--",
        ];
        commit_args.extend(&paths);
        commit(tools, &root, &commit_args)?;
    }
    release(tools, cwd, thread_id)
}

pub fn discard_project(
    tools: &Tools,
    cwd: &Path,
    thread_id: &str,
    reviewed: &Review,
) -> Result<()> {
    ensure_review(tools, cwd, thread_id, reviewed)?;
    let root = root(tools, cwd)?;
    let changed = changed_paths(tools, &root, &reviewed.base, &reviewed.head)?;
    ensure!(
        !changed.iter().any(|change| change.submodule),
        "This thread changed a submodule. Discard its work in your terminal, or choose Keep files and close"
    );
    let restore: Vec<&str> = changed
        .iter()
        .filter(|change| !change.added)
        .map(|change| change.path.as_str())
        .collect();
    if !restore.is_empty() {
        let index = index_path(tools, cwd, thread_id)?;
        let mut read = git_command(tools, &root);
        read.env("GIT_INDEX_FILE", &index)
            .args(["read-tree", &reviewed.base]);
        execute(read, MAX_OUTPUT_BYTES)?;
        let mut checkout = git_command(tools, &root);
        checkout
            .env("GIT_INDEX_FILE", &index)
            .args(["checkout-index", "--force", "--"])
            .args(&restore);
        execute(checkout, MAX_OUTPUT_BYTES)?;
    }
    for change in changed.iter().filter(|change| change.added) {
        let file = root.join(&change.path);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot delete {}", file.display()));
            }
        }
    }
    release(tools, cwd, thread_id)
}

/// Forgets a project-folder thread's base and snapshots. Its files stay as they are.
pub fn release(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<()> {
    if project_ref(tools, cwd, thread_id)?.is_some() {
        git(tools, cwd, &["update-ref", "-d", &reference(thread_id)?])?;
    }
    let _ = std::fs::remove_file(index_path(tools, cwd, thread_id)?);
    Ok(())
}

fn ensure_review(tools: &Tools, cwd: &Path, thread_id: &str, reviewed: &Review) -> Result<()> {
    validate_commit(&reviewed.base)?;
    validate_commit(&reviewed.head)?;
    ensure!(
        !reviewed.truncated,
        "This diff is too large to review here. Review and land it with git in your terminal"
    );
    let changed = match project_ref(tools, cwd, thread_id)? {
        Some(snapshot) => {
            let index = index_path(tools, cwd, thread_id)?;
            let tree = snapshot_tree(tools, cwd, &index, &snapshot)?;
            snapshot != reviewed.head
                || tree != git(tools, cwd, &["rev-parse", &format!("{snapshot}^{{tree}}")])?
        }
        None => {
            let root = root(tools, cwd)?;
            let mut status = git_command_with(tools, &root, false);
            status
                .args(["status", "--porcelain", "--untracked-files=all", "--"])
                .args(pathspec(tools, &root, None)?);
            revision(tools, cwd, "HEAD")? != reviewed.head
                || !execute(status, MAX_OUTPUT_BYTES)?.stdout.is_empty()
        }
    };
    ensure!(
        !changed,
        "The files changed since this review. Refresh the diff before continuing"
    );
    let current = diff(tools, cwd, thread_id, &reviewed.base)?;
    ensure!(
        current == *reviewed,
        "The diff changed. Refresh it before continuing"
    );
    Ok(())
}

struct Change {
    path: String,
    added: bool,
    submodule: bool,
}

fn changed_paths(tools: &Tools, root: &Path, base: &str, head: &str) -> Result<Vec<Change>> {
    let mut command = git_command(tools, root);
    command.args(["diff", "--raw", "--no-renames", "-z", base, head]);
    let output = execute(command, MAX_OUTPUT_BYTES)?;
    ensure!(!output.truncated, "Too many files changed to land here");
    let text = String::from_utf8(output.stdout).context("A changed file name is not UTF-8")?;
    let mut fields = text.split('\0').filter(|field| !field.is_empty());
    let mut changed = Vec::new();
    while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
        let meta: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
        ensure!(meta.len() == 5, "Unexpected git diff output");
        changed.push(Change {
            path: path.to_string(),
            added: meta[4] == "A",
            submodule: meta[0] == "160000" || meta[1] == "160000",
        });
    }
    Ok(changed)
}

fn snapshot_tree(tools: &Tools, cwd: &Path, index: &Path, parent: &str) -> Result<String> {
    let root = root(tools, cwd)?;
    if !index.exists() {
        let mut read = git_command(tools, &root);
        read.env("GIT_INDEX_FILE", index)
            .args(["read-tree", parent]);
        execute(read, MAX_OUTPUT_BYTES)?;
    }
    add_all(tools, cwd, Some(index))?;
    let mut write = git_command(tools, &root);
    write.env("GIT_INDEX_FILE", index).arg("write-tree");
    let output = execute(write, MAX_OUTPUT_BYTES)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
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

fn commit_tree(
    tools: &Tools,
    cwd: &Path,
    tree: &str,
    parent: Option<&str>,
    message: &str,
) -> Result<String> {
    let mut args = vec!["commit-tree", tree, "-m", message];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    let id = commit(tools, cwd, &args)?;
    validate_commit(&id)?;
    Ok(id)
}

/// Runs a git command that makes a commit, with a stand-in identity when git has none, and
/// without signing: a passphrase prompt would wait forever.
fn commit(tools: &Tools, cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = git_command(tools, cwd);
    command.args(["-c", "commit.gpgsign=false"]).args(args);
    if git(tools, cwd, &["config", "user.email"]).is_err() {
        command.envs([
            ("GIT_AUTHOR_NAME", "Newjen"),
            ("GIT_AUTHOR_EMAIL", "newjen@localhost"),
            ("GIT_COMMITTER_NAME", "Newjen"),
            ("GIT_COMMITTER_EMAIL", "newjen@localhost"),
        ]);
    }
    let output = execute(command, MAX_OUTPUT_BYTES)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn empty_tree(tools: &Tools, cwd: &Path) -> Result<String> {
    git(tools, cwd, &["hash-object", "-t", "tree", "--stdin"])
}

fn project_ref(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<Option<String>> {
    let name = reference(thread_id)?;
    let mut command = git_command(tools, cwd);
    command.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{name}^{{commit}}"),
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = command.output().context("Cannot start git")?;
    if !output.status.success() {
        return Ok(None);
    }
    let id = String::from_utf8(output.stdout)?.trim().to_string();
    validate_commit(&id)?;
    Ok(Some(id))
}

fn index_path(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<PathBuf> {
    reference(thread_id)?;
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

fn ensure_ancestor(tools: &Tools, cwd: &Path, base: &str, head: &str) -> Result<()> {
    ensure!(
        succeeds(tools, cwd, &["merge-base", "--is-ancestor", base, head])?,
        "The thread's base is no longer an ancestor of its work"
    );
    Ok(())
}

fn run_checks(tools: &Tools, trusted_project: &Path, workdir: &Path) -> Result<()> {
    let committed = format!("HEAD:{CHECKS_FILE}");
    if !succeeds(
        tools,
        trusted_project,
        &["rev-parse", "--verify", "--quiet", &committed],
    )? {
        return Ok(());
    }
    let mut show = git_command(tools, trusted_project);
    show.args(["cat-file", "blob", &committed]);
    let output = execute(show, 64_001).context("Cannot read the project's landing checks")?;
    let bytes = output.stdout;
    ensure!(
        bytes.len() <= 64_000,
        "Landing checks configuration is too large"
    );
    let checks: Vec<Vec<String>> = serde_json::from_slice(&bytes)
        .context(".newjen/checks.json must be an array of command argument arrays")?;
    for argv in checks {
        let program = argv
            .first()
            .filter(|s| !s.is_empty())
            .context("A landing check needs a program name")?;
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .current_dir(workdir)
            .envs(tools.env.iter().map(|(key, value)| (key, value)));
        execute(command, MAX_OUTPUT_BYTES)
            .with_context(|| format!("Landing check {program:?} failed"))?;
    }
    Ok(())
}

fn marker(thread_id: &str) -> Result<String> {
    uuid::Uuid::parse_str(thread_id).context("Invalid thread id")?;
    Ok(format!("Newjen thread/{thread_id}"))
}

fn reference(thread_id: &str) -> Result<String> {
    uuid::Uuid::parse_str(thread_id).context("Invalid thread id")?;
    Ok(format!("refs/newjen/{thread_id}"))
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
