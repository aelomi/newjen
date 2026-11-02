//! Reviews and lands work with the installed `git`, in temp folders. Each test has its own
//! `HOME` and ignores the machine's git configuration.

use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::changes::ChangeKind;
use newjen_core::landing::{self, Review};
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
    /// The git repository `parent/project` with one commit holding `base.txt`, `other.txt` and
    /// `deleted.txt`, and a local identity.
    fn repo(&self) -> PathBuf {
        let repo = self.init();
        self.git(&repo, &["config", "user.name", "Test"]);
        self.git(&repo, &["config", "user.email", "test@example.com"]);
        self.first_commit(&repo, &[]);
        repo
    }

    /// The same repository with no identity anywhere, as on a fresh Mac.
    fn repo_without_identity(&self) -> PathBuf {
        let repo = self.init();
        self.first_commit(
            &repo,
            &[
                ("GIT_AUTHOR_NAME", "Setup"),
                ("GIT_AUTHOR_EMAIL", "setup@example.com"),
                ("GIT_COMMITTER_NAME", "Setup"),
                ("GIT_COMMITTER_EMAIL", "setup@example.com"),
            ],
        );
        repo
    }

    fn init(&self) -> PathBuf {
        let path = self.dir.path().join("parent/project");
        std::fs::create_dir_all(&path).unwrap();
        let repo = std::fs::canonicalize(path).unwrap();
        self.git(&repo, &["init", "--quiet", "-b", "main"]);
        repo
    }

    fn first_commit(&self, repo: &Path, env: &[(&str, &str)]) {
        write(repo, "base.txt", "base\n");
        write(repo, "other.txt", "other\n");
        write(repo, "deleted.txt", "deleted\n");
        self.git(repo, &["add", "--all"]);
        self.git_with(repo, &["commit", "--quiet", "-m", "base"], env);
    }

    fn commit_all(&self, repo: &Path, message: &str) {
        self.git(repo, &["add", "--all"]);
        self.git(repo, &["commit", "--quiet", "-m", message]);
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.git_with(cwd, args, &[])
    }

    fn git_with(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let output = self.output(cwd, args, env);
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

    fn succeeds(&self, cwd: &Path, args: &[&str]) -> bool {
        self.output(cwd, args, &[]).status.success()
    }

    fn output(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        Command::new(&self.tools.git)
            .args(["-c", "core.quotepath=false"])
            .args(args)
            .current_dir(cwd)
            .envs(self.tools.env.iter().map(|(name, value)| (name, value)))
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn head(&self, repo: &Path) -> String {
        self.git(repo, &["rev-parse", "HEAD"])
    }

    fn status(&self, repo: &Path) -> String {
        self.git(repo, &["status", "--porcelain", "--untracked-files=all"])
    }

    fn staged(&self, repo: &Path) -> String {
        self.git(repo, &["diff", "--cached", "--name-status"])
    }

    /// The files a commit changed, sorted.
    fn files_of(&self, repo: &Path, revision: &str) -> Vec<String> {
        let text = self.git(repo, &["show", "--name-only", "--format=", revision]);
        let mut files: Vec<String> = text.lines().map(str::to_string).collect();
        files.sort();
        files
    }

    fn count(&self, repo: &Path, range: &str) -> usize {
        self.git(repo, &["rev-list", "--count", range])
            .parse()
            .unwrap()
    }

    fn has_ref(&self, repo: &Path) -> bool {
        self.succeeds(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/newjen/{THREAD}"),
            ],
        )
    }

    fn merging(&self, repo: &Path) -> bool {
        self.succeeds(repo, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
    }

    fn worktree(&self, repo: &Path) -> Worktree {
        worktree::create(&self.tools, repo, NAME, |_| {}).unwrap()
    }

    fn snap(&self, cwd: &Path) {
        landing::snapshot(&self.tools, cwd, "Update work", THREAD).unwrap();
    }

    /// Prepares the project folder as a thread's base.
    fn prepare(&self, repo: &Path) -> String {
        landing::prepare_project(&self.tools, repo, THREAD).unwrap()
    }

    fn review(&self, cwd: &Path, base: &str) -> Review {
        landing::review(&self.tools, cwd, THREAD, base).unwrap()
    }

    /// A worktree thread's reviewed work, after it changed `file`.
    fn worktree_review(&self, wt: &Worktree, file: &str, text: &str) -> Review {
        write(&wt.workdir, file, text);
        self.snap(&wt.workdir);
        self.review(&wt.workdir, &wt.base)
    }

    fn land_worktree(&self, wt: &Worktree, reviewed: &Review) -> anyhow::Result<()> {
        landing::land_worktree(&self.tools, wt, THREAD, "Add work", reviewed)
    }

    fn land_project(&self, repo: &Path, reviewed: &Review) -> anyhow::Result<()> {
        landing::land_project(&self.tools, repo, THREAD, "Add work", reviewed)
    }

    fn discard(&self, repo: &Path, reviewed: &Review) -> anyhow::Result<()> {
        landing::discard_project(&self.tools, repo, THREAD, reviewed)
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

fn error_text(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.unwrap_err())
}

#[test]
fn instructions_tell_the_agent_to_leave_version_control_alone() {
    let text = landing::instructions();
    assert!(text.contains("Do not commit"));
    assert!(text.contains("snapshots"));
}

#[test]
fn available_needs_a_git_repository() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    assert!(landing::available(&repo));
    assert!(landing::available(&repo.join("apps/web")));
    let plain = lab.dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert!(!landing::available(&plain));
}

#[test]
fn invalid_thread_ids_are_rejected_before_git_runs() {
    let lab = lab();
    let repo = lab.repo();
    let tools = &lab.tools;
    assert!(landing::prepare_project(tools, &repo, "not-a-uuid").is_err());
    assert!(landing::snapshot(tools, &repo, "t", "../escape").is_err());
    assert!(landing::head(tools, &repo, "refs/heads/main").is_err());
    assert!(landing::release(tools, &repo, "x").is_err());
    assert_eq!(lab.status(&repo), "");
    assert!(!repo.join(".git/newjen").exists());
}

#[test]
fn prepare_in_a_folder_without_git_fails() {
    let lab = lab();
    let plain = lab.dir.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let error = landing::prepare_project(&lab.tools, &plain, THREAD).unwrap_err();
    assert!(error.to_string().contains("git repository"), "{error:#}");
}

#[test]
fn a_repository_without_a_commit_yet_gets_a_base_with_no_parent() {
    let lab = lab();
    let repo = lab.init();
    write(&repo, "owner.txt", "owner file\n");
    let base = lab.prepare(&repo);
    assert_eq!(
        lab.git(&repo, &["rev-list", "--parents", "-n1", &base]),
        base
    );
    assert_eq!(lab.files_of(&repo, &base), vec!["owner.txt"]);
    assert!(!lab.succeeds(&repo, &["rev-parse", "--verify", "--quiet", "HEAD"]));
    assert_eq!(lab.staged(&repo), "");
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert!(reviewed.patch.contains("agent.txt"));
    assert!(!reviewed.patch.contains("owner.txt"));
}

#[test]
fn landing_in_a_repository_without_a_commit_yet_makes_its_first_commit() {
    let lab = lab();
    let repo = lab.init();
    write(&repo, "owner.txt", "owner file\n");
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    lab.git(&repo, &["config", "user.name", "Test"]);
    lab.git(&repo, &["config", "user.email", "test@example.com"]);

    lab.land_project(&repo, &reviewed).unwrap();

    assert_eq!(lab.count(&repo, "HEAD"), 1);
    assert_eq!(lab.files_of(&repo, "HEAD"), vec!["agent.txt"]);
    assert_eq!(lab.status(&repo), "?? owner.txt");
    assert!(!lab.has_ref(&repo));
}

#[test]
fn discarding_in_a_repository_without_a_commit_yet_removes_the_threads_files() {
    let lab = lab();
    let repo = lab.init();
    write(&repo, "owner.txt", "owner file\n");
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    write(&repo, "owner.txt", "agent edit\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    lab.discard(&repo, &reviewed).unwrap();
    assert!(!repo.join("agent.txt").exists());
    assert_eq!(read(&repo, "owner.txt"), "owner file\n");
}

// ---------- project folder ----------

#[test]
fn prepare_records_a_base_without_touching_the_users_index_branch_or_files() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    write(&repo, "staged.txt", "staged\n");
    lab.git(&repo, &["add", "staged.txt"]);
    write(&repo, "untracked.txt", "untracked\n");
    let head = lab.head(&repo);
    let status = lab.status(&repo);
    let staged = lab.staged(&repo);
    assert_eq!(staged, "A\tstaged.txt");

    let base = lab.prepare(&repo);

    assert_eq!(lab.head(&repo), head);
    assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(lab.status(&repo), status);
    assert_eq!(lab.staged(&repo), staged);
    assert_eq!(read(&repo, "base.txt"), "owner edit\n");
    assert_eq!(
        lab.git(&repo, &["rev-parse", &format!("refs/newjen/{THREAD}")]),
        base
    );
    assert_eq!(lab.git(&repo, &["rev-parse", &format!("{base}^")]), head);
    assert_eq!(landing::head(&lab.tools, &repo, THREAD).unwrap(), base);
    assert!(repo.join(format!(".git/newjen/{THREAD}.index")).exists());
}

#[test]
fn the_base_holds_the_work_that_was_there_before_the_thread() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    write(&repo, "untracked.txt", "untracked\n");
    write(&repo, ".gitignore", "target/\n");
    write(&repo, "target/out.bin", "built\n");
    let base = lab.prepare(&repo);
    assert_eq!(
        lab.git(&repo, &["show", &format!("{base}:base.txt")]),
        "owner edit"
    );
    assert_eq!(
        lab.git(&repo, &["show", &format!("{base}:untracked.txt")]),
        "untracked"
    );
    assert!(!lab.succeeds(
        &repo,
        &["cat-file", "-e", &format!("{base}:target/out.bin")]
    ));
}

#[test]
fn head_is_the_projects_commit_until_the_thread_has_a_base() {
    let lab = lab();
    let repo = lab.repo();
    assert_eq!(
        landing::head(&lab.tools, &repo, THREAD).unwrap(),
        lab.head(&repo)
    );
}

#[test]
fn review_shows_only_the_threads_change_not_the_work_that_was_there_before() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    write(&repo, "owner.txt", "owner file\n");
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "agent work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert_eq!(reviewed.base, base);
    assert_eq!(
        reviewed.head,
        landing::head(&lab.tools, &repo, THREAD).unwrap()
    );
    assert!(!reviewed.truncated);
    assert!(reviewed.patch.contains("agent.txt"));
    assert!(reviewed.patch.contains("+agent work"));
    assert!(!reviewed.patch.contains("owner.txt"));
    assert!(!reviewed.patch.contains("base.txt"));
    let summary = landing::changed_files(&lab.tools, &repo, THREAD, &base).unwrap();
    assert_eq!(summary.files.len(), 1);
    assert_eq!(summary.files[0].path, "agent.txt");
    assert_eq!(summary.files[0].kind, ChangeKind::Added);
    assert_eq!((summary.added(), summary.removed()), (1, 0));
}

#[test]
fn a_snapshot_moves_the_threads_ref_only_when_files_changed() {
    let lab = lab();
    let repo = lab.repo();
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    lab.snap(&repo);
    assert_eq!(landing::head(&lab.tools, &repo, THREAD).unwrap(), base);

    write(&repo, "agent.txt", "one\n");
    lab.snap(&repo);
    let first = landing::head(&lab.tools, &repo, THREAD).unwrap();
    assert_ne!(first, base);
    assert_eq!(lab.git(&repo, &["rev-parse", &format!("{first}^")]), base);
    lab.snap(&repo);
    assert_eq!(landing::head(&lab.tools, &repo, THREAD).unwrap(), first);

    write(&repo, "agent.txt", "two\n");
    std::fs::remove_file(repo.join("deleted.txt")).unwrap();
    lab.snap(&repo);
    let second = landing::head(&lab.tools, &repo, THREAD).unwrap();
    assert_eq!(lab.git(&repo, &["rev-parse", &format!("{second}^")]), first);
    assert!(
        lab.git(&repo, &["log", "-1", "--format=%B", &second])
            .contains("Update work")
    );

    assert_eq!(lab.head(&repo), head, "the project's branch never moves");
    assert_eq!(lab.count(&repo, "HEAD"), 1);
    assert_eq!(lab.staged(&repo), "", "the project's index is not touched");
}

#[test]
fn a_snapshot_leaves_ignored_files_out() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, ".gitignore", "target/\n");
    lab.commit_all(&repo, "ignore");
    let base = lab.prepare(&repo);
    write(&repo, "target/out.bin", "built\n");
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert!(reviewed.patch.contains("agent.txt"));
    assert!(!reviewed.patch.contains("target"));
}

#[test]
fn changed_files_counts_added_modified_and_deleted_lines() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "base.txt", "changed\n");
    write(&repo, "agent.txt", "a\nb\n");
    std::fs::remove_file(repo.join("deleted.txt")).unwrap();
    lab.snap(&repo);
    let summary = landing::changed_files(&lab.tools, &repo, THREAD, &base).unwrap();
    let mut files: Vec<_> = summary
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.kind, file.added, file.removed))
        .collect();
    files.sort_by_key(|file| file.0);
    assert_eq!(
        files,
        vec![
            ("agent.txt", ChangeKind::Added, 2, 0),
            ("base.txt", ChangeKind::Modified, 1, 1),
            ("deleted.txt", ChangeKind::Deleted, 0, 1),
        ]
    );
    assert_eq!((summary.added(), summary.removed()), (3, 2));
}

#[test]
fn review_refuses_a_base_that_is_not_a_commit_id() {
    let lab = lab();
    let repo = lab.repo();
    let error = landing::review(&lab.tools, &repo, THREAD, "main").unwrap_err();
    assert!(
        error.to_string().contains("Invalid git commit id"),
        "{error:#}"
    );
    let error = landing::changed_files(&lab.tools, &repo, THREAD, "--help").unwrap_err();
    assert!(
        error.to_string().contains("Invalid git commit id"),
        "{error:#}"
    );
}

#[test]
fn review_refuses_a_base_that_is_not_an_ancestor_of_the_work() {
    let lab = lab();
    let repo = lab.repo();
    lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    lab.git(&repo, &["checkout", "--quiet", "--orphan", "other"]);
    write(&repo, "unrelated.txt", "x\n");
    lab.commit_all(&repo, "unrelated");
    let unrelated = lab.head(&repo);
    let error = landing::review(&lab.tools, &repo, THREAD, &unrelated).unwrap_err();
    assert!(
        error.to_string().contains("no longer an ancestor"),
        "{error:#}"
    );
}

#[test]
fn land_project_commits_only_the_threads_files() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    write(&repo, "staged.txt", "staged\n");
    lab.git(&repo, &["add", "staged.txt"]);
    write(&repo, "owner.txt", "owner file\n");
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "agent work\n");
    write(&repo, "other.txt", "agent edit\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);

    lab.land_project(&repo, &reviewed).unwrap();

    assert_eq!(lab.files_of(&repo, "HEAD"), vec!["agent.txt", "other.txt"]);
    assert_eq!(lab.git(&repo, &["log", "-1", "--format=%s"]), "Add work");
    assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^"]), head);
    assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(read(&repo, "other.txt"), "agent edit\n");
    let status = lab.status(&repo);
    assert!(status.contains(" M base.txt"), "{status}");
    assert!(status.contains("A  staged.txt"), "{status}");
    assert!(status.contains("?? owner.txt"), "{status}");
    assert!(
        !status.contains("agent.txt") && !status.contains("other.txt"),
        "{status}"
    );
    assert_eq!(read(&repo, "base.txt"), "owner edit\n");
    assert_eq!(lab.staged(&repo), "A\tstaged.txt");
    assert!(!lab.has_ref(&repo));
    assert!(!repo.join(format!(".git/newjen/{THREAD}.index")).exists());
}

#[test]
fn land_project_commits_deletions_and_awkward_file_names() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "dir/with space.txt", "space\n");
    write(&repo, "näme.txt", "unicode\n");
    write(&repo, "glob[1]*.txt", "glob\n");
    lab.commit_all(&repo, "awkward names");
    let base = lab.prepare(&repo);
    std::fs::remove_file(repo.join("deleted.txt")).unwrap();
    write(&repo, "dir/with space.txt", "space edit\n");
    write(&repo, "näme.txt", "unicode edit\n");
    write(&repo, "glob[1]*.txt", "glob edit\n");
    write(&repo, "glob[2]*.txt", "new glob\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);

    lab.land_project(&repo, &reviewed).unwrap();

    assert_eq!(
        lab.files_of(&repo, "HEAD"),
        vec![
            "deleted.txt",
            "dir/with space.txt",
            "glob[1]*.txt",
            "glob[2]*.txt",
            "näme.txt"
        ]
    );
    assert_eq!(lab.status(&repo), "");
    assert!(!repo.join("deleted.txt").exists());
}

#[test]
fn land_project_with_an_empty_title_still_commits() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    landing::land_project(&lab.tools, &repo, THREAD, "  ", &reviewed).unwrap();
    assert_eq!(
        lab.git(&repo, &["log", "-1", "--format=%s"]),
        "Newjen thread"
    );
}

#[test]
fn land_project_with_no_changes_commits_nothing_and_releases_the_thread() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "owner.txt", "owner file\n");
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert_eq!(reviewed.patch, "");
    lab.land_project(&repo, &reviewed).unwrap();
    assert_eq!(lab.head(&repo), head);
    assert!(!lab.has_ref(&repo));
    assert_eq!(read(&repo, "owner.txt"), "owner file\n");
}

#[test]
fn land_project_refuses_when_a_thread_file_already_had_uncommitted_changes() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    write(&repo, "base.txt", "agent edit\n");
    write(&repo, "agent.txt", "agent work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);

    let error = error_text(lab.land_project(&repo, &reviewed));

    assert!(error.contains("other uncommitted changes"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert_eq!(read(&repo, "base.txt"), "agent edit\n");
    assert!(repo.join("agent.txt").exists());
    assert!(
        lab.has_ref(&repo),
        "the thread keeps its snapshots after a refusal"
    );
    assert_eq!(lab.staged(&repo), "");
}

#[test]
fn land_project_refuses_a_thread_file_that_was_staged_before_the_thread() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "other.txt", "staged edit\n");
    lab.git(&repo, &["add", "other.txt"]);
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    write(&repo, "other.txt", "agent edit\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("other uncommitted changes"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert_eq!(lab.staged(&repo), "M\tother.txt");
}

#[test]
fn land_project_refuses_a_thread_file_the_user_committed_meanwhile() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "other.txt", "agent edit\n");
    lab.git(&repo, &["add", "other.txt"]);
    lab.git(&repo, &["commit", "--quiet", "-m", "user commit"]);
    let head = lab.head(&repo);
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("other uncommitted changes"), "{error}");
    assert_eq!(lab.head(&repo), head);
}

#[test]
fn land_project_works_after_the_user_committed_their_own_earlier_changes() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "agent work\n");
    lab.git(&repo, &["add", "base.txt"]);
    lab.git(&repo, &["commit", "--quiet", "-m", "user commit"]);
    let user_commit = lab.head(&repo);
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert!(!reviewed.patch.contains("base.txt"));

    lab.land_project(&repo, &reviewed).unwrap();

    assert_eq!(lab.files_of(&repo, "HEAD"), vec!["agent.txt"]);
    assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^"]), user_commit);
    assert_eq!(lab.status(&repo), "");
}

#[test]
fn land_project_refuses_a_review_the_files_changed_since() {
    let lab = lab();
    let repo = lab.repo();
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "first\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);

    write(&repo, "agent.txt", "second\n");
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("changed since"), "{error}");
    assert_eq!(read(&repo, "agent.txt"), "second\n");

    lab.snap(&repo);
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("changed since"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert!(lab.has_ref(&repo));
}

#[test]
fn land_project_refuses_a_diff_too_large_to_review() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "big.txt", &"line of text\n".repeat(20_000));
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert!(reviewed.truncated);
    assert!(reviewed.patch.len() <= landing::MAX_PATCH_BYTES);
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("too large"), "{error}");
    let error = error_text(lab.discard(&repo, &reviewed));
    assert!(error.contains("too large"), "{error}");
    assert!(repo.join("big.txt").exists());
}

#[test]
fn discard_project_restores_modified_recreates_deleted_and_removes_added_files() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "base.txt", "owner edit\n");
    write(&repo, "owner.txt", "owner file\n");
    write(&repo, "staged.txt", "staged\n");
    lab.git(&repo, &["add", "staged.txt"]);
    let status = lab.status(&repo);
    let staged = lab.staged(&repo);
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);

    write(&repo, "other.txt", "agent edit\n");
    std::fs::remove_file(repo.join("deleted.txt")).unwrap();
    write(&repo, "added.txt", "added\n");
    write(&repo, "nested/deep/added.txt", "added\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert!(reviewed.patch.contains("added.txt"));

    lab.discard(&repo, &reviewed).unwrap();

    assert_eq!(read(&repo, "other.txt"), "other\n");
    assert_eq!(read(&repo, "deleted.txt"), "deleted\n");
    assert!(!repo.join("added.txt").exists());
    assert!(!repo.join("nested/deep/added.txt").exists());
    assert_eq!(read(&repo, "base.txt"), "owner edit\n");
    assert_eq!(read(&repo, "owner.txt"), "owner file\n");
    assert_eq!(lab.status(&repo), status);
    assert_eq!(lab.staged(&repo), staged);
    assert_eq!(lab.head(&repo), head);
    assert!(!lab.has_ref(&repo));
    assert!(!repo.join(format!(".git/newjen/{THREAD}.index")).exists());
}

#[test]
fn discard_project_with_no_changes_only_releases_the_thread() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "owner.txt", "owner file\n");
    let base = lab.prepare(&repo);
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    lab.discard(&repo, &reviewed).unwrap();
    assert_eq!(read(&repo, "owner.txt"), "owner file\n");
    assert!(!lab.has_ref(&repo));
}

#[test]
fn a_stale_review_cannot_discard() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "first\n");
    lab.snap(&repo);
    let first = lab.review(&repo, &base);
    write(&repo, "agent.txt", "second\n");
    let error = error_text(lab.discard(&repo, &first));
    assert!(error.contains("changed since"), "{error}");
    assert_eq!(read(&repo, "agent.txt"), "second\n");
    assert!(lab.has_ref(&repo));
}

#[test]
fn reading_a_review_leaves_unsnapshotted_files_unsnapshotted() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "first\n");
    lab.snap(&repo);
    let before = lab.review(&repo, &base);
    write(&repo, "agent.txt", "second\n");
    write(&repo, "later.txt", "later\n");
    let after = lab.review(&repo, &base);
    assert_eq!(before, after);
    assert_eq!(lab.staged(&repo), "");
}

#[test]
fn release_forgets_the_snapshots_and_keeps_the_files() {
    let lab = lab();
    let repo = lab.repo();
    let head = lab.head(&repo);
    lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    assert!(lab.has_ref(&repo));

    landing::release(&lab.tools, &repo, THREAD).unwrap();
    landing::release(&lab.tools, &repo, THREAD).unwrap();

    assert!(!lab.has_ref(&repo));
    assert!(!repo.join(format!(".git/newjen/{THREAD}.index")).exists());
    assert_eq!(read(&repo, "agent.txt"), "work\n");
    assert_eq!(landing::head(&lab.tools, &repo, THREAD).unwrap(), head);
    assert_eq!(lab.head(&repo), head);
}

#[test]
fn preparing_again_starts_the_thread_over_from_the_folders_current_files() {
    let lab = lab();
    let repo = lab.repo();
    let first = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let second = lab.prepare(&repo);
    assert_ne!(first, second);
    assert_eq!(landing::head(&lab.tools, &repo, THREAD).unwrap(), second);
    assert_eq!(lab.review(&repo, &second).patch, "");
}

#[test]
fn snapshots_of_two_threads_are_kept_apart() {
    const OTHER: &str = "0d0f6e60-3a53-4ee8-9a0a-1e8d1b1d4f11";
    let lab = lab();
    let repo = lab.repo();
    let first = lab.prepare(&repo);
    let second = landing::prepare_project(&lab.tools, &repo, OTHER).unwrap();
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    assert_ne!(landing::head(&lab.tools, &repo, THREAD).unwrap(), first);
    assert_eq!(landing::head(&lab.tools, &repo, OTHER).unwrap(), second);
    landing::release(&lab.tools, &repo, THREAD).unwrap();
    assert_eq!(landing::head(&lab.tools, &repo, OTHER).unwrap(), second);
}

// ---------- worktree ----------

#[test]
fn a_worktree_snapshot_commits_on_the_branch_only_when_something_changed() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let main = lab.head(&repo);
    lab.snap(&wt.workdir);
    assert_eq!(lab.count(&wt.path, &format!("{}..HEAD", wt.base)), 0);

    write(&wt.workdir, "agent.txt", "work\n");
    write(&wt.workdir, "base.txt", "edited\n");
    lab.snap(&wt.workdir);
    assert_eq!(lab.count(&wt.path, &format!("{}..HEAD", wt.base)), 1);
    assert_eq!(lab.status(&wt.path), "");
    let message = lab.git(&wt.path, &["log", "-1", "--format=%B"]);
    assert!(
        message.contains(&format!("Newjen thread/{THREAD}")),
        "{message}"
    );
    assert!(message.contains("Update work"), "{message}");
    assert_eq!(
        lab.files_of(&wt.path, "HEAD"),
        vec!["agent.txt", "base.txt"]
    );

    lab.snap(&wt.workdir);
    assert_eq!(lab.count(&wt.path, &format!("{}..HEAD", wt.base)), 1);

    std::fs::remove_file(wt.workdir.join("deleted.txt")).unwrap();
    lab.snap(&wt.workdir);
    assert_eq!(lab.count(&wt.path, &format!("{}..HEAD", wt.base)), 2);
    assert_eq!(lab.files_of(&wt.path, "HEAD"), vec!["deleted.txt"]);

    assert_eq!(lab.head(&repo), main, "the project's branch never moves");
    assert_eq!(lab.status(&repo), "");
    assert!(!lab.has_ref(&repo), "a worktree thread has no project ref");
    assert_eq!(
        landing::head(&lab.tools, &wt.workdir, THREAD).unwrap(),
        lab.git(&repo, &["rev-parse", &wt.branch])
    );
}

#[test]
fn a_worktree_snapshot_leaves_ignored_files_out() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, ".gitignore", "target/\n");
    lab.commit_all(&repo, "ignore");
    let wt = lab.worktree(&repo);
    write(&wt.workdir, "target/out.bin", "built\n");
    write(&wt.workdir, "agent.txt", "work\n");
    lab.snap(&wt.workdir);
    assert_eq!(lab.files_of(&wt.path, "HEAD"), vec!["agent.txt"]);
}

#[test]
fn worktree_review_shows_everything_since_the_base() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    write(&wt.workdir, "agent.txt", "work\n");
    lab.snap(&wt.workdir);
    write(&wt.workdir, "other.txt", "more\n");
    lab.snap(&wt.workdir);
    let reviewed = lab.review(&wt.workdir, &wt.base);
    assert_eq!(reviewed.base, wt.base);
    assert_eq!(reviewed.head, lab.git(&repo, &["rev-parse", &wt.branch]));
    assert!(reviewed.patch.contains("+++ b/agent.txt"));
    assert!(reviewed.patch.contains("+++ b/other.txt"));
    assert!(!reviewed.truncated);
    let summary = landing::changed_files(&lab.tools, &wt.workdir, THREAD, &wt.base).unwrap();
    assert_eq!(summary.files.len(), 2);
    assert_eq!((summary.added(), summary.removed()), (2, 1));
}

#[test]
fn land_worktree_merges_the_branch_with_a_merge_commit() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "owner.txt", "owner file\n");
    write(&repo, "base.txt", "owner edit\n");
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "agent work\n");

    lab.land_worktree(&wt, &reviewed).unwrap();

    let line = lab.git(&repo, &["rev-list", "--parents", "-n1", "HEAD"]);
    let parents: Vec<&str> = line.split(' ').skip(1).collect();
    assert_eq!(parents, vec![head.as_str(), reviewed.head.as_str()]);
    assert_eq!(read(&repo, "agent.txt"), "agent work\n");
    assert_eq!(
        lab.git(&repo, &["log", "-1", "--format=%s"]),
        format!("Land {}: Add work", wt.branch)
    );
    assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(
        read(&repo, "base.txt"),
        "owner edit\n",
        "the project's own edits stay"
    );
    assert_eq!(read(&repo, "owner.txt"), "owner file\n");
    assert!(lab.status(&repo).contains(" M base.txt"));
    assert!(wt.path.exists(), "landing does not remove the worktree");
    assert!(!lab.merging(&repo));
}

#[test]
fn landing_a_worktree_twice_does_not_land_it_again() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "agent work\n");
    lab.land_worktree(&wt, &reviewed).unwrap();
    let landed = lab.head(&repo);
    lab.land_worktree(&wt, &reviewed).unwrap();
    assert_eq!(lab.head(&repo), landed);
}

#[test]
fn landing_a_worktree_without_changes_does_nothing() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    lab.snap(&wt.workdir);
    let reviewed = lab.review(&wt.workdir, &wt.base);
    assert_eq!(reviewed.patch, "");
    lab.land_worktree(&wt, &reviewed).unwrap();
    assert_eq!(lab.head(&repo), head);
}

#[test]
fn landing_a_worktree_after_the_project_moved_on_keeps_both() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "agent work\n");
    write(&repo, "owner.txt", "owner work\n");
    lab.commit_all(&repo, "owner commit");
    let owner = lab.head(&repo);

    lab.land_worktree(&wt, &reviewed).unwrap();

    assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^1"]), owner);
    assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^2"]), reviewed.head);
    assert_eq!(read(&repo, "agent.txt"), "agent work\n");
    assert_eq!(read(&repo, "owner.txt"), "owner work\n");
    assert_eq!(lab.status(&repo), "");
}

#[test]
fn a_worktree_for_a_project_below_the_repository_root_lands_like_any_other() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    lab.commit_all(&repo, "web");
    let wt = worktree::create(&lab.tools, &repo.join("apps/web"), NAME, |_| {}).unwrap();
    let reviewed = lab.worktree_review(&wt, "page.txt", "page\n");
    assert!(reviewed.patch.contains("apps/web/page.txt"));
    lab.land_worktree(&wt, &reviewed).unwrap();
    assert_eq!(read(&repo, "apps/web/page.txt"), "page\n");
}

#[test]
fn landing_refuses_a_predicted_conflict_and_leaves_the_project_unchanged() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "base.txt", "agent edit\n");
    write(&repo, "base.txt", "owner edit\n");
    lab.commit_all(&repo, "owner commit");
    let head = lab.head(&repo);

    let error = error_text(lab.land_worktree(&wt, &reviewed));

    assert!(error.contains("conflict"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert!(!lab.merging(&repo));
    assert_eq!(lab.status(&repo), "");
    assert_eq!(read(&repo, "base.txt"), "owner edit\n");
    assert_eq!(lab.count(&repo, "HEAD"), 2);
}

#[test]
fn landing_aborts_a_merge_the_projects_uncommitted_changes_would_block() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "other.txt", "agent edit\n");
    write(&repo, "other.txt", "owner uncommitted\n");
    let head = lab.head(&repo);

    let error = error_text(lab.land_worktree(&wt, &reviewed));

    assert!(error.contains("Landing could not merge"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert!(!lab.merging(&repo));
    assert_eq!(read(&repo, "other.txt"), "owner uncommitted\n");
    assert_eq!(lab.status(&repo), " M other.txt");
}

#[test]
fn landing_refuses_a_review_the_worktree_changed_since() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "first\n");

    write(&wt.workdir, "late.txt", "after the review\n");
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("changed since"), "{error}");

    lab.snap(&wt.workdir);
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("changed since"), "{error}");

    assert_eq!(lab.head(&repo), head);
    assert!(!repo.join("agent.txt").exists());
}

#[test]
fn landing_refuses_a_review_that_belongs_to_another_worktree() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let mut reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    reviewed.base = "0".repeat(40);
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("another worktree"), "{error}");
}

#[test]
fn landing_refuses_a_diff_too_large_to_review() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    let reviewed = lab.worktree_review(&wt, "big.txt", &"line of text\n".repeat(20_000));
    assert!(reviewed.truncated);
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("too large"), "{error}");
    assert_eq!(lab.head(&repo), head);
}

#[test]
fn landing_leaves_a_legacy_rift_copy_to_the_user() {
    let lab = lab();
    let repo = lab.repo();
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    let legacy = Worktree {
        branch: String::new(),
        base: String::new(),
        ..wt
    };
    assert!(legacy.is_legacy());
    let error = error_text(lab.land_worktree(&legacy, &reviewed));
    assert!(error.contains("older Newjen"), "{error}");
    assert!(!repo.join("agent.txt").exists());
}

// ---------- landing checks ----------

fn checks(lab: &Lab, repo: &Path, json: &str) {
    write(repo, ".newjen/checks.json", json);
    lab.commit_all(repo, "landing checks");
}

#[test]
fn a_failing_check_blocks_landing_a_worktree() {
    let lab = lab();
    let repo = lab.repo();
    checks(&lab, &repo, r#"[["sh", "-c", "echo broken >&2; exit 3"]]"#);
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("Landing check"), "{error}");
    assert!(error.contains("broken"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert!(!repo.join("agent.txt").exists());
}

#[test]
fn checks_run_in_the_worktree_and_a_passing_check_lets_it_land() {
    let lab = lab();
    let repo = lab.repo();
    checks(
        &lab,
        &repo,
        r#"[["true"], ["sh", "-c", "test -f agent.txt"]]"#,
    );
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    lab.land_worktree(&wt, &reviewed).unwrap();
    assert_eq!(read(&repo, "agent.txt"), "work\n");
}

#[test]
fn a_thread_cannot_switch_off_the_checks_by_editing_them_in_its_worktree() {
    let lab = lab();
    let repo = lab.repo();
    checks(&lab, &repo, r#"[["sh", "-c", "exit 1"]]"#);
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    write(&wt.workdir, ".newjen/checks.json", "[]");
    let reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("Landing check"), "{error}");
    assert_eq!(lab.head(&repo), head);
}

#[test]
fn a_failing_check_blocks_landing_the_project_folder() {
    let lab = lab();
    let repo = lab.repo();
    checks(&lab, &repo, r#"[["sh", "-c", "exit 1"]]"#);
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("Landing check"), "{error}");
    assert_eq!(lab.head(&repo), head);
    assert!(repo.join("agent.txt").exists());
    assert!(lab.has_ref(&repo));
}

#[test]
fn a_project_folder_thread_cannot_switch_off_the_committed_checks() {
    let lab = lab();
    let repo = lab.repo();
    checks(&lab, &repo, r#"[["sh", "-c", "exit 1"]]"#);
    let head = lab.head(&repo);
    let base = lab.prepare(&repo);
    write(&repo, ".newjen/checks.json", "[]");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    let error = error_text(lab.land_project(&repo, &reviewed));
    assert!(error.contains("Landing check"), "{error}");
    assert_eq!(lab.head(&repo), head);
}

#[test]
fn a_project_folder_without_a_recorded_base_is_never_committed_to() {
    let lab = lab();
    let repo = lab.repo();
    let head = lab.head(&repo);
    write(&repo, "agent.txt", "work\n");
    let result = landing::snapshot(&lab.tools, &repo, "Update work", THREAD);
    assert!(result.is_err());
    assert_eq!(lab.head(&repo), head);
    assert_eq!(lab.staged(&repo), "");
}

#[test]
fn a_passing_check_lets_the_project_folder_land() {
    let lab = lab();
    let repo = lab.repo();
    checks(&lab, &repo, r#"[["sh", "-c", "test -f agent.txt"]]"#);
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    lab.land_project(&repo, &reviewed).unwrap();
    assert_eq!(lab.files_of(&repo, "HEAD"), vec!["agent.txt"]);
}

#[test]
fn a_checks_file_that_is_not_a_list_of_commands_blocks_landing() {
    let lab = lab();
    let repo = lab.repo();
    checks(&lab, &repo, r#"{"not": "a list"}"#);
    let wt = lab.worktree(&repo);
    let head = lab.head(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    let error = error_text(lab.land_worktree(&wt, &reviewed));
    assert!(error.contains("checks.json"), "{error}");
    assert_eq!(lab.head(&repo), head);
}

// ---------- no git identity ----------

const STAND_IN: &str = "Newjen <newjen@localhost>";

#[test]
fn project_snapshots_and_landing_commit_as_newjen_when_git_has_no_identity() {
    let lab = lab();
    let repo = lab.repo_without_identity();
    assert!(!lab.succeeds(&repo, &["config", "user.email"]));
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    let format = "--format=%an <%ae> / %cn <%ce>";
    assert_eq!(
        lab.git(&repo, &["log", "-1", format, &reviewed.head]),
        format!("{STAND_IN} / {STAND_IN}")
    );
    assert_eq!(
        lab.git(&repo, &["log", "-1", format, &reviewed.base]),
        format!("{STAND_IN} / {STAND_IN}")
    );

    lab.land_project(&repo, &reviewed).unwrap();

    assert_eq!(
        lab.git(&repo, &["log", "-1", format]),
        format!("{STAND_IN} / {STAND_IN}")
    );
    assert_eq!(lab.files_of(&repo, "HEAD"), vec!["agent.txt"]);
}

#[test]
fn worktree_snapshots_and_landing_commit_as_newjen_when_git_has_no_identity() {
    let lab = lab();
    let repo = lab.repo_without_identity();
    let wt = lab.worktree(&repo);
    let reviewed = lab.worktree_review(&wt, "agent.txt", "work\n");
    let format = "--format=%an <%ae> / %cn <%ce>";
    assert_eq!(
        lab.git(&repo, &["log", "-1", format, &reviewed.head]),
        format!("{STAND_IN} / {STAND_IN}")
    );

    lab.land_worktree(&wt, &reviewed).unwrap();

    assert_eq!(
        lab.git(&repo, &["log", "-1", format]),
        format!("{STAND_IN} / {STAND_IN}")
    );
    assert_eq!(read(&repo, "agent.txt"), "work\n");
}

#[test]
fn a_configured_identity_is_used_when_there_is_one() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo);
    write(&repo, "agent.txt", "work\n");
    lab.snap(&repo);
    let reviewed = lab.review(&repo, &base);
    assert_eq!(
        lab.git(&repo, &["log", "-1", "--format=%an <%ae>", &reviewed.head]),
        "Test <test@example.com>"
    );
}
