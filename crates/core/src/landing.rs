use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::thread;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::changes::{self, ChangeSummary};
use crate::rift::{Copy, Tools};

pub const MAX_PATCH_BYTES: usize = 100_000;
const MAX_OUTPUT_BYTES: usize = 1_000_000;
const CHECKS_FILE: &str = ".newjen/checks.json";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingMode {
    #[default]
    HarnessManaged,
    AgentManaged,
}

impl LandingMode {
    pub fn instructions(self) -> &'static str {
        match self {
            Self::HarnessManaged => {
                "Newjen manages version control for this thread. Do not commit, describe, rebase, merge, create bookmarks or branches, or otherwise write version-control metadata. Make file changes only. Newjen snapshots the folder after each turn."
            }
            Self::AgentManaged => {
                "Use jj, not git, to manage version control in this thread. Snapshot your changes with jj and create or update a jj bookmark covering all your work before finishing each turn. Newjen reads recorded jj state only; it does not import git commits or snapshot this thread for you. Do not push unless the user explicitly asks."
            }
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::HarnessManaged => "harness_managed",
            Self::AgentManaged => "agent_managed",
        }
    }
}

impl fmt::Display for LandingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LandingMode {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "harness_managed" => Ok(Self::HarnessManaged),
            "agent_managed" => Ok(Self::AgentManaged),
            _ => bail!("Unknown landing mode {value:?}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integration {
    Rebase,
    Merge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub base: String,
    pub head: String,
    pub patch: String,
    pub truncated: bool,
}

pub fn available(cwd: &Path) -> bool {
    cwd.ancestors()
        .find(|path| path.join(".jj").exists())
        .is_some_and(|root| root.join(".git").exists())
}

pub fn head(tools: &Tools, cwd: &Path) -> Result<String> {
    revision(tools, cwd, "@")
}

pub fn prepare_project(tools: &Tools, cwd: &Path, thread_id: &str) -> Result<String> {
    ensure!(
        available(cwd),
        "Landing currently requires a jj repository colocated with git"
    );
    marker(thread_id)?;
    jj(tools, cwd, &["status"])?;
    let base = head(tools, cwd)?;
    jj(tools, cwd, &["new", "-m", &marker(thread_id)?])?;
    Ok(base)
}

pub fn snapshot(
    tools: &Tools,
    cwd: &Path,
    mode: LandingMode,
    title: &str,
    thread_id: &str,
) -> Result<()> {
    if mode == LandingMode::AgentManaged {
        return Ok(());
    }
    let description = format!("{}\n\n{title}", marker(thread_id)?);
    jj(tools, cwd, &["describe", "-m", &description])?;
    jj(tools, cwd, &["new", "-m", &marker(thread_id)?])?;
    Ok(())
}

pub fn review(tools: &Tools, cwd: &Path, base: &str) -> Result<Review> {
    ensure!(
        available(cwd),
        "Landing currently requires a jj repository colocated with git"
    );
    validate_commit(base)?;
    let head = head(tools, cwd)?;
    ensure_ancestor(tools, cwd, base, &head)?;
    let mut command = jj_command(tools, cwd);
    command.args([
        "diff",
        "--ignore-working-copy",
        "--git",
        "--from",
        base,
        "--to",
        &head,
    ]);
    let output = execute(command, MAX_PATCH_BYTES)?;
    let patch = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(Review {
        base: base.to_string(),
        head,
        patch,
        truncated: output.truncated,
    })
}

/// Per-file counts of everything the thread changed since `base`, up to the last snapshot. One
/// read-only jj call whose output is counted as it streams, so a large diff costs no memory.
pub fn changed_files(tools: &Tools, cwd: &Path, base: &str) -> Result<ChangeSummary> {
    ensure!(
        available(cwd),
        "Landing currently requires a jj repository colocated with git"
    );
    validate_commit(base)?;
    ensure_ancestor(tools, cwd, base, "@")?;
    let mut command = jj_command(tools, cwd);
    command.args([
        "diff",
        "--ignore-working-copy",
        "--git",
        "--context",
        "0",
        "--from",
        base,
        "--to",
        "@",
    ]);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("Cannot start jj")?;
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
        "jj failed: {}",
        String::from_utf8_lossy(&stderr).trim()
    );
    Ok(counted?)
}

pub fn land_copy(
    tools: &Tools,
    copy: &Copy,
    mode: LandingMode,
    thread_id: &str,
    integration: Integration,
    reviewed: &Review,
) -> Result<()> {
    let bookmark = bookmark(thread_id)?;
    let remote = remote(thread_id)?;
    let base = copy
        .base
        .as_deref()
        .context("Landing needs a colocated jj repository")?;
    ensure!(base == reviewed.base, "The review belongs to another copy");
    ensure_review(tools, &copy.path, mode, reviewed)?;
    let target = match mode {
        LandingMode::HarnessManaged => {
            jj(
                tools,
                &copy.path,
                &["bookmark", "set", &bookmark, "-r", &reviewed.head],
            )?;
            reviewed.head.clone()
        }
        LandingMode::AgentManaged => agent_bookmarked_head(tools, &copy.path, reviewed)?,
    };
    run_checks(tools, &copy.source, &copy.workdir)?;
    ensure_review(tools, &copy.path, mode, reviewed)?;
    jj(tools, &copy.source, &["status"])?;
    ensure!(
        log(tools, &copy.source, "@ & conflicts()", "commit_id")?.is_empty(),
        "Resolve the project's existing conflicts before landing"
    );
    if already_landed(tools, &copy.source, &copy.path, &remote, &bookmark, &target)? {
        return Ok(());
    }
    ensure_remote(tools, &copy.source, &remote, &copy.path)?;
    // Only the thread's own bookmarks: a full fetch brings the copy's inherited bookmarks,
    // and with them old versions of changes the project has since rewritten.
    let mut fetch = vec![
        "git".to_string(),
        "fetch".to_string(),
        "--remote".to_string(),
        remote.clone(),
        "--config".to_string(),
        "git.auto-local-bookmark=false".to_string(),
    ];
    for name in work_bookmarks(tools, &copy.path, base, &target)? {
        fetch.push("--branch".to_string());
        fetch.push(name);
    }
    let fetch: Vec<&str> = fetch.iter().map(String::as_str).collect();
    jj(tools, &copy.source, &fetch)?;
    ensure!(
        revision(tools, &copy.source, &target)? == target,
        "The agent's reviewed commit was not exported by a bookmark"
    );
    track_work_refs(tools, &copy.source, &remote, &bookmark, base, &target)?;
    jj(
        tools,
        &copy.source,
        &["bookmark", "set", &bookmark, "-r", &target],
    )?;
    let (base, target) = reconcile_rewritten_base(tools, &copy.source, base, &bookmark)?;
    let base = base.as_str();
    let main = head(tools, &copy.source)?;
    if !log(
        tools,
        &copy.source,
        &format!("{target} & ::{main}"),
        "commit_id",
    )?
    .is_empty()
    {
        return Ok(());
    }
    let operation_before = operation(tools, &copy.source)?;
    let staged_operation = match integration {
        Integration::Rebase => stage(
            tools,
            &copy.source,
            &["rebase", "-r", &format!("{base}..{target}"), "-B", "@"],
        )?,
        Integration::Merge => stage(
            tools,
            &copy.source,
            &["new", "@", &target, "-m", &format!("Land {bookmark}")],
        )?,
    };
    let conflicts = jj(
        tools,
        &copy.source,
        &[
            "log",
            "--at-operation",
            &staged_operation,
            "--no-graph",
            "-r",
            "@ & conflicts()",
            "-T",
            "commit_id",
        ],
    )?;
    ensure!(
        conflicts.is_empty(),
        "Landing would conflict with the project. Resolve the changes in the copy and review again"
    );
    ensure!(
        operation_before == operation(tools, &copy.source)?,
        "The project changed while landing. Review and try again"
    );
    ensure_review(tools, &copy.path, mode, reviewed)?;
    match integration {
        Integration::Rebase => {
            jj(
                tools,
                &copy.source,
                &["rebase", "-r", &format!("{base}..{target}"), "-B", "@"],
            )?;
        }
        Integration::Merge => {
            jj(
                tools,
                &copy.source,
                &["new", "@", &target, "-m", &format!("Land {bookmark}")],
            )?;
        }
    }
    ensure!(
        log(tools, &copy.source, "@ & conflicts()", "commit_id")?.is_empty(),
        "Concurrent project edits caused conflicts. Both versions are preserved in jj; resolve them in the project before continuing"
    );
    Ok(())
}

pub fn land_project(
    tools: &Tools,
    cwd: &Path,
    mode: LandingMode,
    thread_id: &str,
    reviewed: &Review,
) -> Result<()> {
    let bookmark = bookmark(thread_id)?;
    jj(tools, cwd, &["status"])?;
    ensure_review(tools, cwd, mode, reviewed)?;
    ensure!(
        log(tools, cwd, "@ & conflicts()", "commit_id")?.is_empty(),
        "Resolve the project’s conflicts before landing"
    );
    if mode == LandingMode::HarnessManaged {
        ensure_owned(tools, cwd, thread_id, reviewed)?;
    }
    run_checks(tools, cwd, cwd)?;
    jj(tools, cwd, &["status"])?;
    ensure_review(tools, cwd, mode, reviewed)?;
    ensure!(
        log(tools, cwd, "@ & conflicts()", "commit_id")?.is_empty(),
        "Resolve the project’s conflicts before landing"
    );
    jj(
        tools,
        cwd,
        &["bookmark", "set", &bookmark, "-r", &reviewed.head],
    )?;
    jj(tools, cwd, &["new"])?;
    Ok(())
}

pub fn discard_project(
    tools: &Tools,
    cwd: &Path,
    mode: LandingMode,
    thread_id: &str,
    reviewed: &Review,
) -> Result<()> {
    marker(thread_id)?;
    jj(tools, cwd, &["status"])?;
    ensure_review(tools, cwd, mode, reviewed)?;
    if mode == LandingMode::HarnessManaged {
        ensure_owned(tools, cwd, thread_id, reviewed)?;
    }
    let descendants = log(
        tools,
        cwd,
        &format!(
            "({}..{}):: ~ ::{}",
            reviewed.base, reviewed.head, reviewed.head
        ),
        "commit_id",
    )?;
    ensure!(
        descendants.is_empty(),
        "Other changes depend on this thread. Discard them in your terminal"
    );
    jj(tools, cwd, &["new", &reviewed.base])?;
    Ok(())
}

pub fn cleanup_remote(tools: &Tools, source: &Path, thread_id: &str) -> Result<()> {
    let remote = remote(thread_id)?;
    if remote_url(tools, source, &remote)?.is_some() {
        jj(
            tools,
            source,
            &["git", "remote", "remove", &remote, "--ignore-working-copy"],
        )?;
    }
    Ok(())
}

fn ensure_review(tools: &Tools, cwd: &Path, mode: LandingMode, reviewed: &Review) -> Result<()> {
    validate_commit(&reviewed.base)?;
    validate_commit(&reviewed.head)?;
    ensure!(
        !reviewed.truncated,
        "This diff is too large to review here. Review and land it with jj in your terminal"
    );
    if mode == LandingMode::HarnessManaged {
        jj(tools, cwd, &["status"])?;
    }
    ensure!(
        head(tools, cwd)? == reviewed.head,
        "The files changed since this review. Refresh the diff before continuing"
    );
    let current = review(tools, cwd, &reviewed.base)?;
    ensure!(
        current == *reviewed,
        "The diff changed. Refresh it before continuing"
    );
    Ok(())
}

fn ensure_owned(tools: &Tools, cwd: &Path, thread_id: &str, reviewed: &Review) -> Result<()> {
    let marker = serde_json::to_string(&marker(thread_id)?)?;
    let unowned = log(
        tools,
        cwd,
        &format!("{}..{}", reviewed.base, reviewed.head),
        &format!("if(!description.starts_with({marker}), commit_id ++ \"\\n\")"),
    )?;
    ensure!(
        unowned.is_empty(),
        "This project contains changes outside this thread. Manage them with jj in your terminal"
    );
    Ok(())
}

fn agent_bookmarked_head(tools: &Tools, cwd: &Path, reviewed: &Review) -> Result<String> {
    let candidates = log(
        tools,
        cwd,
        &format!(
            "heads(({}..{}) & bookmarks())",
            reviewed.base, reviewed.head
        ),
        "commit_id ++ \"\\n\"",
    )?;
    let candidates: Vec<_> = candidates.lines().collect();
    ensure!(
        candidates.len() == 1,
        "Agent-managed landing needs one bookmarked head. Ask the agent to bookmark its work"
    );
    let target = candidates[0];
    let remaining = jj(
        tools,
        cwd,
        &[
            "diff",
            "--ignore-working-copy",
            "--from",
            target,
            "--to",
            &reviewed.head,
            "--summary",
        ],
    )?;
    ensure!(
        remaining.is_empty(),
        "Some recorded work is not bookmarked. Ask the agent to bookmark all its work"
    );
    Ok(target.to_string())
}

fn already_landed(
    tools: &Tools,
    source: &Path,
    copy: &Path,
    remote: &str,
    bookmark: &str,
    target: &str,
) -> Result<bool> {
    if remote_url(tools, source, remote)?.is_none() {
        return Ok(false);
    }
    let remote_pattern = serde_json::to_string(remote)?;
    let imported = log(
        tools,
        source,
        &format!("remote_bookmarks(remote=exact:{remote_pattern})"),
        "commit_id ++ \"\\n\"",
    )?;
    if !imported.lines().any(|id| id == target) {
        return Ok(false);
    }
    let bookmark_pattern = serde_json::to_string(bookmark)?;
    let landed_change = log(
        tools,
        source,
        &format!("bookmarks(exact:{bookmark_pattern}) & ::@"),
        "change_id",
    )?;
    let source_change = log(tools, copy, target, "change_id")?;
    Ok(landed_change == source_change)
}

fn track_work_refs(
    tools: &Tools,
    source: &Path,
    remote: &str,
    owned_bookmark: &str,
    base: &str,
    target: &str,
) -> Result<()> {
    let refs = jj(
        tools,
        source,
        &[
            "bookmark",
            "list",
            "--ignore-working-copy",
            "--remote",
            remote,
            "-T",
            "if(remote, name ++ \"\\t\" ++ normal_target.commit_id() ++ \"\\n\")",
        ],
    )?;
    let mut work_refs = Vec::new();
    for line in refs.lines() {
        let Some((name, commit)) = line.split_once('\t') else {
            continue;
        };
        validate_commit(commit)?;
        if log(
            tools,
            source,
            &format!("{commit} & ({base}..{target})"),
            "commit_id",
        )?
        .is_empty()
        {
            continue;
        }
        let pattern = serde_json::to_string(name)?;
        let local = log(
            tools,
            source,
            &format!("bookmarks(exact:{pattern})"),
            "commit_id",
        )?;
        ensure!(
            name == owned_bookmark || local.is_empty() || local == commit,
            "The agent bookmark {name:?} conflicts with an existing project bookmark. Rename it in the copy before landing"
        );
        work_refs.push(format!("{name}@{remote}"));
    }
    for name in work_refs {
        jj(tools, source, &["bookmark", "track", &name])?;
    }
    Ok(())
}

/// The copy's bookmarks on the thread's work (`base..target`), the ones landing fetches.
fn work_bookmarks(tools: &Tools, copy: &Path, base: &str, target: &str) -> Result<Vec<String>> {
    let listed = jj(
        tools,
        copy,
        &[
            "bookmark",
            "list",
            "--ignore-working-copy",
            "-T",
            "if(!remote, name ++ \"\\t\" ++ normal_target.commit_id() ++ \"\\n\")",
        ],
    )?;
    let mut names = Vec::new();
    for line in listed.lines() {
        let Some((name, commit)) = line.split_once('\t') else {
            continue;
        };
        validate_commit(commit)?;
        if !log(
            tools,
            copy,
            &format!("{commit} & ({base}..{target})"),
            "commit_id",
        )?
        .is_empty()
        {
            names.push(name.to_string());
        }
    }
    ensure!(
        !names.is_empty(),
        "The reviewed work has no bookmark in the copy to fetch"
    );
    Ok(names)
}

/// If the project rewrote the thread's base change while the copy worked on the old version,
/// the fetch brought that old version back. Move the work onto the current version and hide
/// the old one, so `divergent()` stays empty. Returns the base and the bookmark's commit to
/// land from, after the move.
fn reconcile_rewritten_base(
    tools: &Tools,
    source: &Path,
    base: &str,
    bookmark: &str,
) -> Result<(String, String)> {
    let change = log(tools, source, base, "change_id")?;
    let current = log(
        tools,
        source,
        &format!("change_id({change}) ~ {base}"),
        "commit_id ++ \"\\n\"",
    )?;
    let current: Vec<&str> = current.lines().filter(|line| !line.is_empty()).collect();
    if current.is_empty() {
        return Ok((base.to_string(), revision(tools, source, bookmark)?));
    }
    ensure!(
        current.len() == 1,
        "The thread's base change has several versions in the project. Resolve that before landing"
    );
    validate_commit(current[0])?;
    jj(
        tools,
        source,
        &[
            "rebase",
            "-s",
            &format!("roots({base}..{bookmark})"),
            "-d",
            current[0],
        ],
    )?;
    jj(tools, source, &["abandon", base])?;
    Ok((current[0].to_string(), revision(tools, source, bookmark)?))
}

fn ensure_ancestor(tools: &Tools, cwd: &Path, base: &str, head: &str) -> Result<()> {
    ensure!(
        !log(tools, cwd, &format!("{base} & ::{head}"), "commit_id")?.is_empty(),
        "The thread's base is no longer an ancestor of its work"
    );
    Ok(())
}

fn run_checks(tools: &Tools, trusted_project: &Path, workdir: &Path) -> Result<()> {
    let root = Path::new(&jj(
        tools,
        trusted_project,
        &["root", "--ignore-working-copy"],
    )?)
    .to_path_buf();
    let path = root.join(CHECKS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("Cannot read the project's landing checks"),
    };
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

fn ensure_remote(tools: &Tools, source: &Path, remote: &str, path: &Path) -> Result<()> {
    match remote_url(tools, source, remote)? {
        Some(url) => ensure!(
            Path::new(&url) == path,
            "The thread's jj remote points to another folder"
        ),
        None => {
            jj(
                tools,
                source,
                &[
                    "git",
                    "remote",
                    "add",
                    remote,
                    path.to_str().context("The copy path is not UTF-8")?,
                ],
            )?;
        }
    }
    Ok(())
}

fn remote_url(tools: &Tools, source: &Path, remote: &str) -> Result<Option<String>> {
    let remotes = jj(
        tools,
        source,
        &["git", "remote", "list", "--ignore-working-copy"],
    )?;
    Ok(remotes.lines().find_map(|line| {
        let (name, url) = line.split_once(char::is_whitespace)?;
        (name == remote).then(|| url.trim().to_string())
    }))
}

fn marker(thread_id: &str) -> Result<String> {
    Ok(format!("Newjen {}", bookmark(thread_id)?))
}

fn bookmark(thread_id: &str) -> Result<String> {
    uuid::Uuid::parse_str(thread_id).context("Invalid thread id")?;
    Ok(format!("thread/{thread_id}"))
}

fn remote(thread_id: &str) -> Result<String> {
    bookmark(thread_id)?;
    Ok(format!("thread-{thread_id}"))
}

fn validate_commit(value: &str) -> Result<()> {
    ensure!(
        (40..=64).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "Invalid jj commit id"
    );
    Ok(())
}

fn revision(tools: &Tools, cwd: &Path, revset: &str) -> Result<String> {
    let value = log(tools, cwd, revset, "commit_id")?;
    validate_commit(&value)?;
    Ok(value)
}

fn log(tools: &Tools, cwd: &Path, revset: &str, template: &str) -> Result<String> {
    jj(
        tools,
        cwd,
        &[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            revset,
            "-T",
            template,
        ],
    )
}

fn operation(tools: &Tools, cwd: &Path) -> Result<String> {
    jj(
        tools,
        cwd,
        &[
            "op",
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "--limit",
            "1",
            "-T",
            "id",
        ],
    )
}

fn stage(tools: &Tools, cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = jj_command(tools, cwd);
    command.args(args).arg("--no-integrate-operation");
    let result = execute(command, MAX_OUTPUT_BYTES)
        .context("Safe landing requires jj with --no-integrate-operation support")?;
    result
        .stderr
        .lines()
        .find_map(|line| {
            line.strip_prefix(
                "Operation left uncommitted because --no-integrate-operation was requested: ",
            )
            .map(str::to_string)
        })
        .context("jj did not return the prepared landing operation")
}

fn jj(tools: &Tools, cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = jj_command(tools, cwd);
    command.args(args);
    let output = execute(command, MAX_OUTPUT_BYTES)?;
    ensure!(!output.truncated, "jj output is too large");
    String::from_utf8(output.stdout)
        .map(|s| s.trim().to_string())
        .context("jj returned non-UTF-8 output")
}

fn jj_command(tools: &Tools, cwd: &Path) -> Command {
    let mut command = Command::new(&tools.jj);
    command
        .args(["--color=never", "--no-pager"])
        .current_dir(cwd)
        .envs(tools.env.iter().map(|(key, value)| (key, value)));
    command
}

struct Output {
    stdout: Vec<u8>,
    stderr: String,
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
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    ensure!(status.success(), "{program} failed: {}", stderr.trim());
    Ok(Output {
        stdout,
        stderr,
        truncated,
    })
}
