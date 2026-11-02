use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::landing::{self, Review};
use newjen_core::worktree::{self, CopyError, Tools};

const THREAD: &str = "6444bd65-a528-420c-b1d7-f18b0287b755";
const OTHER: &str = "0d0f6e60-3a53-4ee8-9a0a-1e8d1b1d4f11";
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
        self.repo_at("parent/project")
    }

    fn repo_at(&self, name: &str) -> PathBuf {
        let repo = self.folder(name);
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

    fn head(&self, repo: &Path) -> String {
        self.git(repo, &["rev-parse", "HEAD"])
    }

    fn status(&self, repo: &Path) -> String {
        self.git(repo, &["status", "--porcelain", "--untracked-files=all"])
    }

    fn files_of(&self, repo: &Path, revision: &str) -> Vec<String> {
        let text = self.git(repo, &["show", "--name-only", "--format=", revision]);
        let mut files: Vec<String> = text.lines().map(str::to_string).collect();
        files.sort();
        files
    }

    fn has_ref(&self, repo: &Path, thread: &str) -> bool {
        self.output(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/newjen/{thread}"),
            ],
        )
        .status
        .success()
    }

    fn prepare(&self, cwd: &Path, thread: &str) -> String {
        landing::prepare_project(&self.tools, cwd, thread).unwrap()
    }

    fn snap(&self, cwd: &Path, thread: &str) {
        landing::snapshot(&self.tools, cwd, "Update work", thread).unwrap();
    }

    fn review(&self, cwd: &Path, thread: &str, base: &str) -> Review {
        landing::review(&self.tools, cwd, thread, base).unwrap()
    }

    fn land_project(&self, cwd: &Path, thread: &str, reviewed: &Review) -> anyhow::Result<()> {
        landing::land_project(&self.tools, cwd, thread, "Add work", reviewed)
    }

    fn discard(&self, cwd: &Path, thread: &str, reviewed: &Review) -> anyhow::Result<()> {
        landing::discard_project(&self.tools, cwd, thread, reviewed)
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

fn text(result: &anyhow::Result<()>) -> String {
    match result {
        Ok(()) => "Ok(())".to_string(),
        Err(error) => format!("Err({error:#})"),
    }
}

#[test]
fn discard_in_a_subfolder_project_leaves_files_outside_the_project_alone() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    write(&repo, "apps/api/main.txt", "api\n");
    lab.commit_all(&repo, "apps");
    let project = repo.join("apps/web");

    let base = lab.prepare(&project, THREAD);
    // The agent works in apps/web; the user, meanwhile, edits apps/api in their editor.
    write(&project, "index.txt", "web by agent\n");
    write(&repo, "apps/api/main.txt", "api by user\n");
    lab.snap(&project, THREAD);
    let reviewed = lab.review(&project, THREAD, &base);
    eprintln!("review patch files:\n{}", reviewed.patch);

    let result = lab.discard(&project, THREAD, &reviewed);
    eprintln!("discard: {}", text(&result));
    eprintln!(
        "apps/api/main.txt after discard: {:?}",
        read(&repo, "apps/api/main.txt")
    );
    assert!(
        !reviewed.patch.contains("apps/api/main.txt"),
        "the review shows a file outside the project that the agent never touched"
    );
    assert_eq!(read(&repo, "apps/api/main.txt"), "api by user\n");
}

#[test]
fn land_in_a_subfolder_project_commits_only_the_projects_files() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "apps/web/index.txt", "web\n");
    write(&repo, "apps/api/main.txt", "api\n");
    lab.commit_all(&repo, "apps");
    let project = repo.join("apps/web");

    let base = lab.prepare(&project, THREAD);
    write(&project, "index.txt", "web by agent\n");
    write(&repo, "apps/api/main.txt", "api by user\n");
    lab.snap(&project, THREAD);
    let reviewed = lab.review(&project, THREAD, &base);

    let result = lab.land_project(&project, THREAD, &reviewed);
    eprintln!("land: {}", text(&result));
    let files = lab.files_of(&repo, "HEAD");
    eprintln!("files in the landing commit: {files:?}");
    eprintln!("status after land: {:?}", lab.status(&repo));
    assert_eq!(files, vec!["apps/web/index.txt".to_string()]);
}

#[test]
fn two_project_folder_threads_in_one_repository_stay_apart() {
    let lab = lab();
    let repo = lab.repo();
    write(&repo, "a/a.txt", "a\n");
    write(&repo, "b/b.txt", "b\n");
    lab.commit_all(&repo, "a and b");
    let a = repo.join("a");
    let b = repo.join("b");

    let base_a = lab.prepare(&a, THREAD);
    let _base_b = lab.prepare(&b, OTHER);
    write(&b, "new.txt", "thread B's work\n");
    lab.snap(&b, OTHER);
    lab.snap(&a, THREAD);
    let reviewed_a = lab.review(&a, THREAD, &base_a);
    eprintln!("thread A's review patch:\n{}", reviewed_a.patch);

    let result = lab.discard(&a, THREAD, &reviewed_a);
    eprintln!("discard A: {}", text(&result));
    eprintln!(
        "b/new.txt exists after discarding A: {}",
        b.join("new.txt").exists()
    );
    assert_eq!(
        reviewed_a.patch, "",
        "thread A's review shows thread B's file"
    );
    assert!(
        b.join("new.txt").exists(),
        "discarding thread A deleted thread B's work"
    );
}

#[test]
fn snapshot_without_a_base_never_commits_on_the_users_branch() {
    let lab = lab();
    let repo = lab.repo();
    let linked = lab.dir.path().join("parent/linked");
    lab.git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            linked.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    let linked = std::fs::canonicalize(linked).unwrap();

    // In the main checkout, a thread with no recorded base is refused.
    write(&repo, "base.txt", "edited\n");
    let main_head = lab.head(&repo);
    let main_result = landing::snapshot(&lab.tools, &repo, "Update work", THREAD);
    eprintln!("main checkout: {}", text(&main_result));
    assert_eq!(lab.head(&repo), main_head);

    // In the user's own linked worktree, the same call.
    write(&linked, "base.txt", "edited in linked\n");
    let linked_head = lab.head(&linked);
    let linked_result = landing::snapshot(&lab.tools, &linked, "Update work", THREAD);
    eprintln!("linked worktree: {}", text(&linked_result));
    eprintln!(
        "feature branch log: {}",
        lab.git(&linked, &["log", "--oneline", "feature"])
    );
    assert_eq!(
        lab.head(&linked),
        linked_head,
        "Newjen committed on the user's branch 'feature'"
    );
}

#[test]
fn prepare_land_and_discard_work_from_a_linked_worktree_project() {
    let lab = lab();
    let repo = lab.repo();
    let linked = lab.dir.path().join("parent/linked");
    lab.git(
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            linked.to_str().unwrap(),
            "-b",
            "feature",
        ],
    );
    let linked = std::fs::canonicalize(linked).unwrap();
    let base = lab.prepare(&linked, THREAD);
    write(&linked, "agent.txt", "work\n");
    lab.snap(&linked, THREAD);
    let reviewed = lab.review(&linked, THREAD, &base);
    assert!(reviewed.patch.contains("agent.txt"));
    lab.land_project(&linked, THREAD, &reviewed).unwrap();
    assert_eq!(lab.files_of(&linked, "HEAD"), vec!["agent.txt".to_string()]);
    assert_eq!(
        lab.git(&repo, &["rev-parse", "main"]),
        lab.git(&repo, &["rev-parse", "HEAD"])
    );
    assert!(!lab.has_ref(&repo, THREAD));
}

#[test]
fn discard_refuses_a_submodule_the_thread_moved() {
    let lab = lab();
    let upstream = lab.repo_at("parent/upstream");
    let repo = lab.repo();
    lab.git(
        &repo,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            upstream.to_str().unwrap(),
            "sub",
        ],
    );
    lab.commit_all(&repo, "add submodule");
    let sub = repo.join("sub");
    let sub_before = lab.head(&sub);

    let base = lab.prepare(&repo, THREAD);
    write(&sub, "inside.txt", "agent work inside the submodule\n");
    lab.commit_all(&sub, "agent commit in submodule");
    lab.snap(&repo, THREAD);
    let reviewed = lab.review(&repo, THREAD, &base);
    eprintln!("review patch:\n{}", reviewed.patch);

    let moved = lab.head(&sub);
    let result = lab.discard(&repo, THREAD, &reviewed);
    assert!(result.is_err());
    assert_eq!(lab.head(&sub), moved);
    assert!(lab.has_ref(&repo, THREAD));
    assert_ne!(sub_before, moved);
}

#[test]
fn a_snapshot_survives_an_empty_repository_the_agent_initialised() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo, THREAD);
    write(&repo, "agent.txt", "agent work\n");
    let lib = repo.join("newlib");
    std::fs::create_dir(&lib).unwrap();
    lab.git(&lib, &["init", "--quiet"]);
    write(&lib, "lib.txt", "scaffolded\n");
    let first = landing::snapshot(&lab.tools, &repo, "Update work", THREAD);
    eprintln!("snapshot: {}", text(&first));
    let second = landing::snapshot(&lab.tools, &repo, "Update work", THREAD);
    eprintln!("snapshot again (the retry button): {}", text(&second));
    let reviewed = landing::review(&lab.tools, &repo, THREAD, &base).unwrap();
    eprintln!(
        "review shows agent.txt: {}",
        reviewed.patch.contains("agent.txt")
    );
    assert!(first.is_ok(), "the thread can never snapshot again");
}

#[test]
fn a_worktree_snapshot_survives_an_empty_repository_the_agent_initialised() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    write(&wt.workdir, "agent.txt", "agent work\n");
    let lib = wt.workdir.join("newlib");
    std::fs::create_dir(&lib).unwrap();
    lab.git(&lib, &["init", "--quiet"]);
    let result = landing::snapshot(&lab.tools, &wt.workdir, "Update work", THREAD);
    eprintln!("worktree snapshot: {}", text(&result));
    assert!(result.is_ok());
}

#[test]
fn discard_leaves_an_embedded_repository_alone_and_restores_the_rest() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo, THREAD);
    write(&repo, "base.txt", "agent edit\n");
    write(&repo, "agent.txt", "agent work\n");
    // A repository with a commit, as `git clone` leaves one.
    let lib = repo.join("newlib");
    std::fs::create_dir(&lib).unwrap();
    lab.git(&lib, &["init", "--quiet"]);
    lab.git(&lib, &["config", "user.name", "Test"]);
    lab.git(&lib, &["config", "user.email", "test@example.com"]);
    write(&lib, "lib.txt", "cloned\n");
    lab.commit_all(&lib, "upstream");
    lab.snap(&repo, THREAD);
    let reviewed = lab.review(&repo, THREAD, &base);
    eprintln!("review patch:\n{}", reviewed.patch);

    assert!(!reviewed.patch.contains("newlib"));
    lab.discard(&repo, THREAD, &reviewed).unwrap();
    assert_eq!(read(&repo, "base.txt"), "base\n");
    assert!(!repo.join("agent.txt").exists());
    assert_eq!(read(&lib, "lib.txt"), "cloned\n");
    assert!(!lab.has_ref(&repo, THREAD));
}

#[test]
fn a_refreshed_review_after_a_hand_edit_lets_the_project_thread_land() {
    let lab = lab();
    let repo = lab.repo();
    let base = lab.prepare(&repo, THREAD);
    write(&repo, "agent.txt", "agent work\n");
    lab.snap(&repo, THREAD);
    let first = lab.review(&repo, THREAD, &base);
    // The user fixes a typo in the agent's file by hand, then refreshes the review as told.
    write(&repo, "agent.txt", "agent work, fixed by hand\n");
    let refreshed = lab.review(&repo, THREAD, &base);
    eprintln!("refreshed review equals the first: {}", refreshed == first);
    let result = lab.land_project(&repo, THREAD, &refreshed);
    eprintln!("land with the refreshed review: {}", text(&result));
    assert!(
        refreshed.patch.contains("fixed by hand"),
        "the refreshed review does not show the hand edit"
    );
}

#[test]
fn a_refreshed_review_after_a_hand_edit_lets_the_worktree_thread_land() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    write(&wt.workdir, "agent.txt", "agent work\n");
    lab.snap(&wt.workdir, THREAD);
    let first = lab.review(&wt.workdir, THREAD, &wt.base);
    write(&wt.workdir, "agent.txt", "agent work, fixed by hand\n");
    let refreshed = lab.review(&wt.workdir, THREAD, &wt.base);
    eprintln!("refreshed review equals the first: {}", refreshed == first);
    let result = landing::land_worktree(&lab.tools, &wt, THREAD, "Add work", &refreshed);
    eprintln!("land with the refreshed review: {}", text(&result));
    assert!(refreshed.patch.contains("fixed by hand"));
}

#[test]
fn land_worktree_refuses_a_project_with_a_detached_head() {
    let lab = lab();
    let repo = lab.repo();
    let wt = worktree::create(&lab.tools, &repo, NAME, |_| {}).unwrap();
    write(&wt.workdir, "agent.txt", "agent work\n");
    lab.snap(&wt.workdir, THREAD);
    let reviewed = lab.review(&wt.workdir, THREAD, &wt.base);
    lab.git(&repo, &["checkout", "--quiet", "--detach"]);
    let main_before = lab.git(&repo, &["rev-parse", "main"]);
    let result = landing::land_worktree(&lab.tools, &wt, THREAD, "Add work", &reviewed);
    eprintln!("land: {}", text(&result));
    eprintln!(
        "HEAD: {} (symbolic: {:?}); main: {} -> {}",
        lab.head(&repo),
        String::from_utf8_lossy(&lab.output(&repo, &["symbolic-ref", "-q", "HEAD"]).stdout).trim(),
        main_before,
        lab.git(&repo, &["rev-parse", "main"])
    );
    assert!(
        result.is_err(),
        "the merge landed on a detached HEAD that no branch points to"
    );
}

#[test]
fn land_project_refuses_when_the_users_index_differs_from_head_for_a_thread_file() {
    let lab = lab();
    let repo = lab.repo();
    // The user staged a version of other.txt that is not in the working tree (git add, then
    // put the file back by hand: the index holds "staged only").
    write(&repo, "other.txt", "staged only\n");
    lab.git(&repo, &["add", "other.txt"]);
    write(&repo, "other.txt", "other\n");
    let staged_before = lab.git(&repo, &["diff", "--cached"]);
    eprintln!("staged before the thread:\n{staged_before}");

    let base = lab.prepare(&repo, THREAD);
    write(&repo, "other.txt", "agent edit\n");
    lab.snap(&repo, THREAD);
    let reviewed = lab.review(&repo, THREAD, &base);
    let result = lab.land_project(&repo, THREAD, &reviewed);
    eprintln!("land: {}", text(&result));
    eprintln!("staged after: {:?}", lab.git(&repo, &["diff", "--cached"]));
    eprintln!(
        "committed other.txt: {:?}",
        lab.git(&repo, &["show", "HEAD:other.txt"])
    );
    assert!(
        result.is_err(),
        "the user's staged content was overwritten and lost"
    );
}

#[test]
fn a_worktree_for_an_uncommitted_subfolder_project_is_refused() {
    let lab = lab();
    let repo = lab.repo();
    let project = repo.join("newservice");
    write(&project, "notes.txt", "not committed yet\n");
    let made = worktree::create(&lab.tools, &project, NAME, |_| {});
    assert!(matches!(made, Err(CopyError::Failed { .. })));
    assert!(!worktree::destination(&project, NAME).unwrap().exists());
    assert_eq!(lab.git(&repo, &["branch", "--list", "newjen/*"]), "");
}

#[test]
fn prepare_works_in_a_project_that_already_holds_an_empty_nested_repository() {
    let lab = lab();
    let repo = lab.repo();
    let vendored = repo.join("vendor/tool");
    std::fs::create_dir_all(&vendored).unwrap();
    lab.git(&vendored, &["init", "--quiet"]);
    let result = landing::prepare_project(&lab.tools, &repo, THREAD);
    eprintln!(
        "prepare: {:?}",
        result.as_ref().map(|_| ()).map_err(|e| format!("{e:#}"))
    );
    assert!(result.is_ok(), "a project-folder thread cannot start here");
}
