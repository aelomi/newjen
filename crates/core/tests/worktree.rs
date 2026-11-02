//! Makes and removes real git worktrees with the installed `git`, in temp folders. Each test has
//! its own `HOME` and ignores the machine's git configuration.

use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::worktree::{self, CopyError, Progress, Tools, Worktree};

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
    /// An empty folder `name` (canonical) inside the temp folder.
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    /// The git repository `parent/project` with one commit, "base", holding `base.txt`.
    fn repo(&self) -> PathBuf {
        let repo = self.folder("parent/project");
        self.git(&repo, &["init", "--quiet", "-b", "main"]);
        self.git(&repo, &["config", "user.name", "Test"]);
        self.git(&repo, &["config", "user.email", "test@example.com"]);
        write(&repo, "base.txt", "base\n");
        self.commit_all(&repo, "base");
        repo
    }

    fn commit_all(&self, repo: &Path, message: &str) {
        self.git(repo, &["add", "--all"]);
        self.git(repo, &["commit", "--quiet", "-m", message]);
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git_output(cwd, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn git_succeeds(&self, cwd: &Path, args: &[&str]) -> bool {
        self.git_output(cwd, args).status.success()
    }

    fn git_output(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new(&self.tools.git)
            .args(args)
            .current_dir(cwd)
            .envs(self.tools.env.iter().map(|(name, value)| (name, value)))
            .output()
            .unwrap()
    }

    fn has_branch(&self, repo: &Path, branch: &str) -> bool {
        self.git_succeeds(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
    }

    fn worktrees(&self, repo: &Path) -> String {
        self.git(repo, &["worktree", "list", "--porcelain"])
    }

    /// Commits `file` with `text` on `wt`'s branch, so the branch is ahead of the project.
    fn commit_in(&self, wt: &Worktree, file: &str, text: &str) {
        self.git(&wt.path, &["config", "user.name", "Test"]);
        self.git(&wt.path, &["config", "user.email", "test@example.com"]);
        write(&wt.path, file, text);
        self.commit_all(&wt.path, "thread work");
    }
}

fn write(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const NAME: &str = "newjen-1a2b3c4d-abc123";

#[test]
fn branch_names_drop_the_newjen_prefix() {
    assert_eq!(worktree::branch_name(NAME), "newjen/1a2b3c4d-abc123");
    assert_eq!(worktree::branch_name("plain"), "newjen/plain");
}

#[test]
fn create_makes_a_branch_and_a_worktree_at_head() {
    let lab = lab();
    let repo = lab.repo();
    let head = lab.git(&repo, &["rev-parse", "HEAD"]);
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    assert_eq!(wt.branch, "newjen/1a2b3c4d-abc123");
    assert_eq!(wt.base, head);
    assert_eq!(wt.source, repo);
    assert_eq!(wt.workdir, wt.path);
    assert_eq!(
        wt.path,
        lab.folder("parent")
            .join(".newjen-worktrees/project")
            .join(NAME)
    );
    assert_eq!(wt.path, worktree::destination(&repo, NAME).unwrap());
    assert!(!wt.is_legacy());
    assert_eq!(
        std::fs::read_to_string(wt.path.join("base.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(lab.git(&wt.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        lab.git(&wt.path, &["symbolic-ref", "--short", "HEAD"]),
        wt.branch
    );
    assert!(lab.has_branch(&repo, &wt.branch));
    assert!(
        lab.worktrees(&repo)
            .contains(&wt.path.display().to_string())
    );
}

#[test]
fn create_leaves_the_project_branch_and_files_alone() {
    let lab = lab();
    let repo = lab.repo();
    let head = lab.git(&repo, &["rev-parse", "HEAD"]);
    worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    assert_eq!(lab.git(&repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(lab.git(&repo, &["status", "--porcelain"]), "");
}

#[test]
fn create_reports_progress() {
    let lab = lab();
    let repo = lab.repo();
    let mut progress = Vec::new();
    worktree::create(&lab.tools, &repo, NAME, |step| progress.push(step)).unwrap();
    assert_eq!(progress.first(), Some(&Progress::MainRepoReady));
}

#[test]
fn create_for_a_project_below_the_repository_root_works_in_its_folder() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    lab.commit_all(&repo, "web");
    let project = repo.join("apps/web");
    let wt = worktree::create(&lab.tools, &project, NAME, |_| {}).unwrap();
    assert_eq!(wt.source, repo);
    assert_eq!(wt.workdir, wt.path.join("apps/web"));
    assert_eq!(
        wt.path,
        lab.folder("parent")
            .join(".newjen-worktrees/project")
            .join(NAME)
    );
    assert_eq!(
        std::fs::read_to_string(wt.workdir.join("index.txt")).unwrap(),
        "web\n"
    );
}

#[test]
fn create_does_not_copy_uncommitted_or_ignored_files() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, ".gitignore", "target/\n");
    lab.commit_all(&repo, "ignore");
    write(&repo, "target/out.bin", "built\n");
    write(&repo, "untracked.txt", "new\n");
    write(&repo, "base.txt", "edited\n");
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    assert!(!wt.path.join("target").exists());
    assert!(!wt.path.join("untracked.txt").exists());
    assert_eq!(
        std::fs::read_to_string(wt.path.join("base.txt")).unwrap(),
        "base\n"
    );
    let status = lab.git(&repo, &["status", "--porcelain"]);
    assert!(
        status.contains("M base.txt") && status.contains("?? untracked.txt"),
        "{status}"
    );
}

#[test]
fn create_starts_on_the_commit_the_project_has_checked_out() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "second.txt", "2\n");
    lab.commit_all(&repo, "second");
    let head = lab.git(&repo, &["rev-parse", "HEAD"]);
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    assert_eq!(wt.base, head);
    assert!(wt.path.join("second.txt").exists());
}

#[test]
fn create_in_a_folder_without_git_fails() {
    let lab = lab();
    let folder = lab.folder("parent/plain");
    write(&folder, "a.txt", "a\n");
    let error = worktree::create(&lab.tools, &folder, NAME, |_| {}).unwrap_err();
    assert!(matches!(error, CopyError::Failed { .. }), "{error:?}");
    assert!(!worktree::available(&folder));
    assert!(!folder.parent().unwrap().join(".newjen-worktrees").exists());
}

#[test]
fn create_in_a_repository_without_a_commit_fails() {
    let lab = lab();
    let repo = lab.folder("parent/empty");
    lab.git(&repo, &["init", "--quiet", "-b", "main"]);
    let error = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap_err();
    assert!(
        matches!(&error, CopyError::Failed { detail, .. } if detail.contains("first commit")),
        "{error:?}"
    );
}

#[test]
fn create_refuses_a_destination_that_exists() {
    let lab = lab();
    let repo = lab.repo();
    let path = worktree::destination(&repo, NAME).unwrap();
    write(&path, "keep.txt", "mine\n");
    let error = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap_err();
    assert_eq!(error, CopyError::Exists { path: path.clone() });
    assert_eq!(
        std::fs::read_to_string(path.join("keep.txt")).unwrap(),
        "mine\n"
    );
    assert!(!lab.has_branch(&repo, &worktree::branch_name(NAME)));
}

#[test]
fn creating_the_same_name_twice_reports_exists_and_keeps_the_first() {
    let lab = lab();
    let repo = lab.repo();
    let first = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    let error = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap_err();
    assert_eq!(
        error,
        CopyError::Exists {
            path: first.path.clone()
        }
    );
    assert!(first.path.join("base.txt").exists());
    assert!(lab.has_branch(&repo, &first.branch));
}

#[test]
fn create_keeps_an_existing_unmerged_branch_with_the_same_name() {
    let lab = lab();
    let repo = lab.repo();
    let old = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    lab.commit_in(&old, "old.txt", "old work\n");
    let tip = lab.git(&repo, &["rev-parse", &old.branch]);
    lab.git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            &old.path.display().to_string(),
        ],
    );
    assert!(lab.has_branch(&repo, &old.branch));
    let error = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap_err();
    assert!(matches!(error, CopyError::Failed { .. }), "{error:?}");
    assert!(
        lab.has_branch(&repo, &old.branch),
        "the unmerged branch was deleted"
    );
    assert_eq!(lab.git(&repo, &["rev-parse", &old.branch]), tip);
}

#[test]
fn remove_after_a_merge_deletes_the_worktree_and_its_branch() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    lab.commit_in(&wt, "work.txt", "work\n");
    lab.git(&repo, &["merge", "--quiet", "--no-edit", &wt.branch]);
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    assert!(!wt.path.exists());
    assert!(!lab.has_branch(&repo, &wt.branch));
    assert!(
        !lab.worktrees(&repo)
            .contains(&wt.path.display().to_string())
    );
    assert!(repo.join("work.txt").exists());
}

#[test]
fn remove_deletes_a_branch_that_has_no_new_commits() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    assert!(!lab.has_branch(&repo, &wt.branch));
}

#[test]
fn remove_keeps_an_unmerged_branch_so_its_work_can_be_recovered() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    lab.commit_in(&wt, "work.txt", "work\n");
    let tip = lab.git(&repo, &["rev-parse", &wt.branch]);
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    assert!(!wt.path.exists());
    assert!(
        !lab.worktrees(&repo)
            .contains(&wt.path.display().to_string())
    );
    assert!(lab.has_branch(&repo, &wt.branch));
    assert_eq!(lab.git(&repo, &["rev-parse", &wt.branch]), tip);
}

#[test]
fn remove_deletes_a_worktree_with_uncommitted_and_untracked_files() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    write(&wt.path, "base.txt", "dirty\n");
    write(&wt.path, "scratch/new.txt", "new\n");
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    assert!(!wt.path.exists());
    assert!(!lab.has_branch(&repo, &wt.branch));
    assert_eq!(
        std::fs::read_to_string(repo.join("base.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn remove_can_run_again_after_it_finished() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    lab.commit_in(&wt, "work.txt", "work\n");
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    assert!(lab.has_branch(&repo, &wt.branch));
    lab.git(&repo, &["branch", "-D", &wt.branch]);
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
}

#[test]
fn remove_finishes_when_the_folder_was_deleted_by_hand() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    std::fs::remove_dir_all(&wt.path).unwrap();
    worktree::remove(&lab.tools, &wt.source, &wt.path, &wt.branch).unwrap();
    assert!(
        !lab.worktrees(&repo)
            .contains(&wt.path.display().to_string())
    );
    assert!(!lab.has_branch(&repo, &wt.branch));
}

#[test]
fn remove_leaves_a_legacy_copy_alone() {
    let lab = lab();
    let repo = lab.repo();
    let copy = lab.folder("parent/.newjen-worktrees/project/legacy");
    write(&copy, "kept.txt", "kept\n");
    worktree::remove(&lab.tools, &repo, &copy, "").unwrap();
    assert!(copy.join("kept.txt").exists());
}

#[test]
fn remove_refuses_a_path_that_is_not_the_projects_worktree() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    let elsewhere = lab.folder("elsewhere");
    write(&elsewhere, "keep.txt", "keep\n");
    let error = worktree::remove(&lab.tools, &repo, &elsewhere, &wt.branch).unwrap_err();
    assert!(matches!(error, CopyError::Failed { .. }), "{error:?}");
    assert!(elsewhere.join("keep.txt").exists());
    assert!(wt.path.exists());
}

#[test]
fn remove_refuses_a_branch_that_is_not_a_newjen_branch() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    let error = worktree::remove(&lab.tools, &repo, &wt.path, "main").unwrap_err();
    assert!(matches!(error, CopyError::Failed { .. }), "{error:?}");
    assert!(wt.path.exists());
    assert!(lab.has_branch(&repo, "main"));
}

#[test]
fn remove_refuses_a_symlinked_worktree_folder() {
    let lab = lab();
    let repo = lab.repo();
    let target = lab.folder("precious");
    write(&target, "keep.txt", "keep\n");
    let path = worktree::destination(&repo, NAME).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    let error =
        worktree::remove(&lab.tools, &repo, &path, &worktree::branch_name(NAME)).unwrap_err();
    assert!(matches!(error, CopyError::Failed { .. }), "{error:?}");
    assert!(target.join("keep.txt").exists());
}

#[test]
fn remove_pending_deletes_the_folder_registration_and_an_unmerged_branch() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    lab.commit_in(&wt, "work.txt", "work\n");
    worktree::remove_pending(&lab.tools, &repo, &wt.path).unwrap();
    assert!(worktree::pending_is_gone(&wt.path));
    assert!(!lab.has_branch(&repo, &wt.branch));
    assert!(
        !lab.worktrees(&repo)
            .contains(&wt.path.display().to_string())
    );
}

#[test]
fn remove_pending_can_run_again() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    worktree::remove_pending(&lab.tools, &repo, &wt.path).unwrap();
    worktree::remove_pending(&lab.tools, &repo, &wt.path).unwrap();
    assert!(worktree::pending_is_gone(&wt.path));
}

#[test]
fn remove_pending_refuses_a_folder_that_is_not_a_worktree_destination() {
    let lab = lab();
    let repo = lab.repo();
    let elsewhere = lab.folder("elsewhere");
    write(&elsewhere, "keep.txt", "keep\n");
    let error = worktree::remove_pending(&lab.tools, &repo, &elsewhere).unwrap_err();
    assert!(matches!(error, CopyError::Failed { .. }), "{error:?}");
    assert!(elsewhere.join("keep.txt").exists());
}

#[test]
fn available_is_true_inside_a_repository_and_false_outside() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    assert!(worktree::available(&repo));
    assert!(worktree::available(&repo.join("apps/web")));
    assert!(!worktree::available(&lab.folder("parent/plain")));
    assert!(!worktree::available(&repo.join("missing")));
}

#[test]
fn project_root_is_the_nearest_folder_with_a_git_entry() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    assert_eq!(worktree::project_root(&repo.join("apps/web")), repo);
    let plain = lab.folder("parent/plain");
    assert_eq!(worktree::project_root(&plain), plain);
}

#[test]
fn worktrees_of_one_project_are_independent() {
    let lab = lab();
    let repo = lab.repo();
    let first = worktree::create(&lab.tools, &repo, "newjen-aaaaaaaa-111111", |_| {}).unwrap();
    let second = worktree::create(&lab.tools, &repo, "newjen-bbbbbbbb-222222", |_| {}).unwrap();
    write(&first.path, "only-first.txt", "1\n");
    assert!(!second.path.join("only-first.txt").exists());
    worktree::remove(&lab.tools, &repo, &first.path, &first.branch).unwrap();
    assert!(second.path.join("base.txt").exists());
    assert!(lab.has_branch(&repo, &second.branch));
}

#[test]
fn the_agent_environment_names_the_branch_and_the_source() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    let env = wt.env();
    assert!(env.contains(&("NEWJEN_WORKTREE".to_string(), wt.branch.clone())));
    assert!(env.contains(&("NEWJEN_SOURCE".to_string(), repo.display().to_string())));
}
