use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::vcs::{self, Scope};
use newjen_core::worktree::{self, Tools, Worktree};

const THREAD: &str = "6444bd65-a528-420c-b1d7-f18b0287b755";
const NAME: &str = "newjen-6444bd65-abc123";

struct Lab {
    dir: tempfile::TempDir,
    tools: Tools,
}

fn lab() -> Lab {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let tools = Tools::find().with_env([
        ("HOME".to_string(), home.display().to_string()),
        ("GIT_CONFIG_GLOBAL".to_string(), "/dev/null".to_string()),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "1".to_string()),
    ]);
    Lab { dir, tools }
}

impl Lab {
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    fn repo(&self) -> PathBuf {
        let repo = self.folder("parent/project");
        self.git(&repo, &["init", "--quiet", "-b", "main"]);
        self.git(&repo, &["config", "user.name", "Test"]);
        self.git(&repo, &["config", "user.email", "test@example.com"]);
        write(&repo, "base.txt", "base\n");
        write(&repo, "other.txt", "other\n");
        self.commit_all(&repo, "base");
        repo
    }

    fn commit_all(&self, repo: &Path, message: &str) {
        self.git(repo, &["add", "--all"]);
        self.git(repo, &["commit", "--quiet", "-m", message]);
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.output(cwd, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .to_string()
    }

    fn output(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new(&self.tools.git)
            .args(["-c", "core.quotepath=false"])
            .args(args)
            .current_dir(cwd)
            .envs(self.tools.env.iter().map(|(name, value)| (name, value)))
            .output()
            .unwrap()
    }

    fn checkpoint(&self, cwd: &Path) -> String {
        vcs::checkpoint(&self.tools, cwd, THREAD).unwrap()
    }

    fn refs(&self, repo: &Path) -> String {
        self.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname:short)",
                &format!("refs/newjen/{THREAD}/"),
            ],
        )
    }

    fn files(&self, cwd: &Path, scope: Scope) -> Vec<String> {
        let diff = vcs::diff(&self.tools, cwd, THREAD, scope).unwrap();
        let mut files: Vec<String> = diff
            .patch
            .lines()
            .filter_map(|line| line.strip_prefix("diff --git a/"))
            .filter_map(|rest| rest.rsplit_once(" b/").map(|(_, file)| file.to_string()))
            .collect();
        files.sort();
        files
    }

    fn worktree(&self, project: &Path) -> Worktree {
        let made = worktree::create(&self.tools, project, NAME, |_| {}).unwrap();
        self.checkpoint(&made.workdir);
        made
    }

    fn index_path(&self, repo: &Path) -> PathBuf {
        PathBuf::from(self.git(
            repo,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("newjen/{THREAD}.index"),
            ],
        ))
    }
}

fn write(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap()
}

fn error_text<T>(result: anyhow::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => format!("{error:#}"),
    }
}

#[test]
fn a_stale_index_lock_from_a_crashed_checkpoint_does_not_block_the_retry() {
    let lab = lab();
    let repo = lab.repo();
    lab.checkpoint(&repo);
    let lock = {
        let mut path = lab.index_path(&repo).into_os_string();
        path.push(".lock");
        PathBuf::from(path)
    };
    std::fs::write(&lock, "").unwrap();

    let first = vcs::checkpoint(&lab.tools, &repo, THREAD);
    let second = vcs::checkpoint(&lab.tools, &repo, THREAD);
    assert!(
        second.is_ok(),
        "first attempt: {first:?}\nretry: {}\nlock still present: {}",
        error_text(second),
        lock.exists()
    );
}

#[test]
fn revert_turn_keeps_untracked_files_the_user_added_after_the_last_checkpoint() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    write(&wt.workdir, "base.txt", "turn 1\n");
    lab.checkpoint(&wt.workdir);
    write(
        &wt.workdir,
        "notes/user-notes.txt",
        "written by hand after turn 1\n",
    );

    vcs::revert_turn(&lab.tools, &wt, THREAD, 1).unwrap();

    assert_eq!(read(&wt.workdir, "base.txt"), "base\n");
    assert!(
        wt.workdir.join("notes/user-notes.txt").exists(),
        "revert turn 1 deleted notes/user-notes.txt, a file no turn ever touched; the worktree now has: {:?}",
        list(&wt.workdir)
    );
}

#[test]
fn revert_turn_keeps_later_turns_edits_to_other_files() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    write(&wt.workdir, "base.txt", "turn 1\n");
    lab.checkpoint(&wt.workdir);
    write(&wt.workdir, "other.txt", "turn 2\n");
    write(&wt.workdir, "new-in-turn-2.txt", "turn 2\n");
    lab.checkpoint(&wt.workdir);

    vcs::revert_turn(&lab.tools, &wt, THREAD, 1).unwrap();

    assert_eq!(read(&wt.workdir, "base.txt"), "base\n", "turn 1 is undone");
    assert_eq!(
        (
            read(&wt.workdir, "other.txt"),
            wt.workdir.join("new-in-turn-2.txt").exists()
        ),
        ("turn 2\n".to_string(), true),
        "revert turn 1 also undid turn 2's work on files turn 1 never touched"
    );
}

#[test]
fn revert_turn_in_a_subfolder_project_leaves_the_rest_of_the_worktree_alone() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    write(&repo, "apps/api/main.txt", "api\n");
    lab.commit_all(&repo, "apps");
    let wt = lab.worktree(&repo.join("apps/web"));
    write(&wt.workdir, "index.txt", "turn 1\n");
    lab.checkpoint(&wt.workdir);
    write(&wt.path, "apps/api/main.txt", "edited outside the folder\n");
    write(
        &wt.path,
        "apps/api/untracked.txt",
        "new outside the folder\n",
    );

    vcs::revert_turn(&lab.tools, &wt, THREAD, 1).unwrap();

    assert_eq!(read(&wt.workdir, "index.txt"), "web\n");
    assert_eq!(
        (
            read(&wt.path, "apps/api/main.txt"),
            wt.path.join("apps/api/untracked.txt").exists()
        ),
        ("edited outside the folder\n".to_string(), true),
        "revert of the apps/web thread rewrote and deleted files under apps/api; status now:\n{}",
        lab.git(
            &wt.path,
            &["status", "--porcelain", "--untracked-files=all"]
        )
    );
}

#[test]
fn a_sparse_checkout_does_not_show_the_hidden_files_as_deleted() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    write(&repo, "apps/api/main.txt", "api\n");
    lab.commit_all(&repo, "apps");
    lab.git(&repo, &["sparse-checkout", "set", "apps/web"]);
    assert!(!repo.join("apps/api/main.txt").exists());
    lab.checkpoint(&repo);

    let patch = vcs::diff(&lab.tools, &repo, THREAD, Scope::Uncommitted)
        .unwrap()
        .patch;
    assert_eq!(
        patch, "",
        "a clean sparse checkout shows an uncommitted diff:\n{patch}"
    );
}

#[test]
fn commit_does_not_discard_what_the_user_staged() {
    let lab = lab();
    let repo = lab.repo();
    lab.checkpoint(&repo);
    write(&repo, "base.txt", "staged version\n");
    lab.git(&repo, &["add", "base.txt"]);
    write(&repo, "base.txt", "base\n");
    let staged_before = lab.git(&repo, &["diff", "--cached"]);
    assert!(staged_before.contains("+staged version"));
    let status = vcs::status(&lab.tools, &repo).unwrap();
    assert!(
        status.dirty && status.branch.is_some(),
        "the git button sees a dirty branch and offers Commit: {status:?}"
    );

    let result = vcs::commit(&lab.tools, &repo, "feat: staged work");

    let staged_after = lab.git(&repo, &["diff", "--cached"]);
    assert!(
        result.is_ok() || staged_after == staged_before,
        "commit returned {:?}; the user's staged change is gone. Before:\n{staged_before}\nAfter:\n{staged_after:?}",
        result.map_err(|error| format!("{error:#}"))
    );
}

#[test]
fn commit_works_while_a_merge_is_in_progress() {
    let lab = lab();
    let repo = lab.repo();
    lab.checkpoint(&repo);
    lab.git(&repo, &["checkout", "--quiet", "-b", "feature"]);
    write(&repo, "base.txt", "feature\n");
    lab.commit_all(&repo, "feature");
    lab.git(&repo, &["checkout", "--quiet", "main"]);
    write(&repo, "base.txt", "main\n");
    lab.commit_all(&repo, "main");
    let merge = lab.output(&repo, &["merge", "feature"]);
    assert!(!merge.status.success(), "the merge conflicts");
    write(&repo, "base.txt", "resolved\n");
    lab.git(&repo, &["add", "base.txt"]);
    let status = vcs::status(&lab.tools, &repo).unwrap();
    assert!(
        status.dirty && status.branch.as_deref() == Some("main"),
        "the git button sees a dirty branch and offers Commit: {status:?}"
    );

    let result = vcs::commit(&lab.tools, &repo, "merge feature");

    assert!(
        result.is_ok(),
        "Commit was offered but fails: {}",
        error_text(result)
    );
}

#[test]
fn a_folder_with_many_untracked_files_still_gets_checkpoints_and_a_status() {
    let lab = lab();
    let repo = lab.repo();
    lab.checkpoint(&repo);
    for package in 0..300 {
        for file in 0..80 {
            write(
                &repo,
                &format!("web/node_modules/@scope/package-{package:04}/lib/module-{file:03}.js"),
                "x",
            );
        }
    }
    let count: usize = lab
        .git(&repo, &["ls-files", "--others", "--exclude-standard"])
        .lines()
        .count();
    assert_eq!(count, 24_000);

    let checkpoint = vcs::checkpoint(&lab.tools, &repo, THREAD);
    let status = vcs::status(&lab.tools, &repo);
    assert!(
        checkpoint.is_ok() && status.is_ok(),
        "with {count} untracked files (a build output not yet in .gitignore):\ncheckpoint: {}\nstatus: {}",
        checkpoint
            .as_ref()
            .map(|_| "ok".to_string())
            .unwrap_or_else(|e| format!("{e:#}")),
        status
            .as_ref()
            .map(|_| "ok".to_string())
            .unwrap_or_else(|e| format!("{e:#}"))
    );
}

#[test]
fn a_commit_elsewhere_in_the_repository_is_not_the_subfolder_threads_change() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    write(&repo, "apps/api/main.txt", "api\n");
    lab.commit_all(&repo, "apps");
    let web = repo.join("apps/web");
    lab.checkpoint(&web);
    write(&web, "index.txt", "web by agent\n");
    write(&repo, "apps/api/main.txt", "api by user\n");
    write(&repo, "apps/api/new.txt", "new by user\n");
    lab.git(&repo, &["add", "apps/api"]);
    lab.git(&repo, &["commit", "--quiet", "-m", "user commits the api"]);
    lab.checkpoint(&web);

    let all = lab.files(&web, Scope::All);
    let turn = lab.files(&web, Scope::Turn(1));
    let summary = vcs::changed_files(&lab.tools, &web, THREAD, Scope::All).unwrap();
    let mut card: Vec<String> = summary.files.iter().map(|f| f.path.clone()).collect();
    card.sort();
    assert_eq!(
        (all.clone(), turn.clone(), card.clone()),
        (
            vec!["apps/web/index.txt".to_string()],
            vec!["apps/web/index.txt".to_string()],
            vec!["apps/web/index.txt".to_string()]
        ),
        "All: {all:?}\nTurn 1: {turn:?}\nChanges card: {card:?}"
    );
}

#[test]
fn two_checkpoints_at_once_each_get_their_own_ordinal() {
    let lab = lab();
    let repo = lab.repo();
    lab.checkpoint(&repo);
    write(&repo, "a.txt", "a\n");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let runs: Vec<_> = (0..2)
        .map(|_| {
            let tools = lab.tools.clone();
            let repo = repo.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                vcs::checkpoint(&tools, &repo, THREAD).map_err(|e| format!("{e:#}"))
            })
        })
        .collect();
    let results: Vec<_> = runs.into_iter().map(|run| run.join().unwrap()).collect();
    let refs = lab.refs(&repo);
    assert_eq!(
        refs.lines().count(),
        3,
        "two concurrent checkpoints after the start should leave three refs; results {results:?}; refs:\n{refs}"
    );
}

#[test]
fn a_branch_tracking_a_remote_not_named_origin_can_still_push() {
    let lab = lab();
    let repo = lab.repo();
    let bare = lab.folder("remote.git");
    lab.git(&bare, &["init", "--quiet", "--bare", "-b", "main"]);
    lab.git(&repo, &["remote", "add", "github", bare.to_str().unwrap()]);
    lab.git(&repo, &["push", "--quiet", "-u", "github", "main"]);
    write(&repo, "a.txt", "a\n");
    lab.commit_all(&repo, "ahead");

    let status = vcs::status(&lab.tools, &repo).unwrap();
    assert!(
        status.remote && status.upstream && status.ahead == 1,
        "plan() offers Push only when `remote` is true: {status:?}"
    );
}

fn list(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for entry in walkdir(dir) {
        found.push(entry.strip_prefix(dir).unwrap().display().to_string());
    }
    found.sort();
    found
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if path.is_dir() {
            out.extend(walkdir(&path));
        } else {
            out.push(path);
        }
    }
    out
}
