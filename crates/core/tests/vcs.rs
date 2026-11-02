use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::changes::{ChangeKind, ChangeSummary};
use newjen_core::vcs::{self, Scope};
use newjen_core::worktree::{self, Tools, Worktree};

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

const SETUP_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Setup"),
    ("GIT_AUTHOR_EMAIL", "setup@example.com"),
    ("GIT_COMMITTER_NAME", "Setup"),
    ("GIT_COMMITTER_EMAIL", "setup@example.com"),
];

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

    fn repo_without_identity(&self) -> PathBuf {
        let repo = self.folder("parent/project");
        self.git(&repo, &["init", "--quiet", "-b", "main"]);
        self.git(&repo, &["config", "user.useConfigOnly", "true"]);
        write(&repo, "base.txt", "base\n");
        self.git(&repo, &["add", "--all"]);
        self.git_env(&repo, &["commit", "--quiet", "-m", "base"], &SETUP_IDENTITY);
        repo
    }

    fn unborn_repo(&self) -> PathBuf {
        let repo = self.folder("parent/project");
        self.git(&repo, &["init", "--quiet", "-b", "main"]);
        self.git(&repo, &["config", "user.name", "Test"]);
        self.git(&repo, &["config", "user.email", "test@example.com"]);
        repo
    }

    fn commit_all(&self, repo: &Path, message: &str) {
        self.git(repo, &["add", "--all"]);
        self.git(repo, &["commit", "--quiet", "-m", message]);
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.git_env(cwd, args, &[])
    }

    fn git_env(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let output = self.output_env(cwd, args, env);
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
        self.output_env(cwd, args, &[])
    }

    fn output_env(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
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

    fn files_of(&self, repo: &Path, revision: &str) -> Vec<String> {
        let text = self.git(repo, &["show", "--name-only", "--format=", revision]);
        let mut files: Vec<String> = text.lines().map(str::to_string).collect();
        files.sort();
        files
    }

    fn tree_files(&self, repo: &Path, revision: &str) -> Vec<String> {
        let text = self.git(repo, &["ls-tree", "-r", "--name-only", revision]);
        let mut files: Vec<String> = text.lines().map(str::to_string).collect();
        files.sort();
        files
    }

    fn count(&self, repo: &Path, range: &str) -> usize {
        self.git(repo, &["rev-list", "--count", range])
            .parse()
            .unwrap()
    }

    fn merging(&self, repo: &Path) -> bool {
        self.output(repo, &["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
            .status
            .success()
    }

    fn checkpoint(&self, cwd: &Path) -> String {
        vcs::checkpoint(&self.tools, cwd, THREAD).unwrap()
    }

    fn checkpoint_of(&self, cwd: &Path, thread: &str) -> String {
        vcs::checkpoint(&self.tools, cwd, thread).unwrap()
    }

    fn refs(&self, repo: &Path, thread: &str) -> Vec<(usize, String)> {
        let listed = self.git(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                &format!("refs/newjen/{thread}/"),
            ],
        );
        let mut refs: Vec<(usize, String)> = listed
            .lines()
            .map(|line| {
                let (name, id) = line.split_once(' ').unwrap();
                let ordinal = name.rsplit('/').next().unwrap().parse().unwrap();
                (ordinal, id.to_string())
            })
            .collect();
        refs.sort();
        refs
    }

    fn turns(&self, cwd: &Path) -> usize {
        vcs::turns(&self.tools, cwd, THREAD).unwrap()
    }

    /// The files a scope shows, after checking that the patch and the per-file counts agree.
    fn files(&self, cwd: &Path, scope: Scope) -> Vec<String> {
        self.files_of_thread(cwd, THREAD, scope)
    }

    fn files_of_thread(&self, cwd: &Path, thread: &str, scope: Scope) -> Vec<String> {
        let diff = vcs::diff(&self.tools, cwd, thread, scope).unwrap();
        let summary = vcs::changed_files(&self.tools, cwd, thread, scope).unwrap();
        let from_patch = patch_files(&diff.patch);
        assert_eq!(
            from_patch,
            summary_files(&summary),
            "changed_files and diff disagree for {scope:?}"
        );
        from_patch
    }

    fn patch(&self, cwd: &Path, scope: Scope) -> String {
        vcs::diff(&self.tools, cwd, THREAD, scope).unwrap().patch
    }

    fn status_of(&self, cwd: &Path) -> vcs::Status {
        vcs::status(&self.tools, cwd).unwrap()
    }

    fn bare_remote(&self, repo: &Path) -> PathBuf {
        let bare = self.folder("remote.git");
        self.git(&bare, &["init", "--quiet", "--bare", "-b", "main"]);
        self.git(repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        self.git(repo, &["push", "--quiet", "-u", "origin", "main"]);
        self.git(repo, &["remote", "set-head", "origin", "main"]);
        bare
    }

    fn worktree(&self, repo: &Path) -> Worktree {
        let made = worktree::create(&self.tools, repo, NAME, |_| {}).unwrap();
        self.checkpoint(&made.workdir);
        made
    }

    fn remote_head(&self, bare: &Path, branch: &str) -> String {
        self.git(bare, &["rev-parse", &format!("refs/heads/{branch}")])
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

fn patch_files(patch: &str) -> Vec<String> {
    let mut files: Vec<String> = patch
        .lines()
        .filter_map(|line| line.strip_prefix("diff --git a/"))
        .filter_map(|rest| rest.rsplit_once(" b/").map(|(_, file)| file.to_string()))
        .collect();
    files.sort();
    files
}

fn summary_files(summary: &ChangeSummary) -> Vec<String> {
    let mut files: Vec<String> = summary.files.iter().map(|file| file.path.clone()).collect();
    files.sort();
    files
}

fn names(files: &[&str]) -> Vec<String> {
    files.iter().map(|file| file.to_string()).collect()
}

fn error_text<T>(result: anyhow::Result<T>) -> String {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => format!("{error:#}"),
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn embedded_repo(lab: &Lab, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    lab.git(dir, &["init", "--quiet"]);
    lab.git(dir, &["config", "user.name", "Test"]);
    lab.git(dir, &["config", "user.email", "test@example.com"]);
    write(dir, "lib.txt", "cloned\n");
    lab.commit_all(dir, "upstream");
}

mod checkpoints {
    use super::*;

    #[test]
    fn the_first_checkpoint_is_the_start_and_each_one_after_ends_a_turn() {
        let lab = lab();
        let repo = lab.repo();
        assert_eq!(
            lab.turns(&repo),
            0,
            "a thread with no checkpoint has no turns"
        );

        let start = lab.checkpoint(&repo);
        assert_eq!(lab.refs(&repo, THREAD), vec![(0, start.clone())]);
        assert_eq!(lab.turns(&repo), 0, "the start is not a turn");

        write(&repo, "agent.txt", "work\n");
        let first = lab.checkpoint(&repo);
        write(&repo, "agent.txt", "more work\n");
        let second = lab.checkpoint(&repo);
        assert_eq!(
            lab.refs(&repo, THREAD),
            vec![(0, start.clone()), (1, first.clone()), (2, second.clone())]
        );
        assert_eq!(lab.turns(&repo), 2);
        for id in [&start, &first, &second] {
            assert_eq!(lab.git(&repo, &["cat-file", "-t", id]), "commit");
        }
        assert_eq!(lab.refs(&repo, OTHER), vec![]);
    }

    #[test]
    fn checkpoint_numbers_sort_as_numbers_past_nine() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        for step in 1..=11 {
            write(&repo, &format!("step{step}.txt"), "work\n");
            lab.checkpoint(&repo);
        }
        assert_eq!(lab.turns(&repo), 11);
        assert_eq!(lab.files(&repo, Scope::Turn(2)), names(&["step2.txt"]));
        assert_eq!(lab.files(&repo, Scope::Turn(10)), names(&["step10.txt"]));
        assert_eq!(lab.files(&repo, Scope::Turn(11)), names(&["step11.txt"]));
    }

    #[test]
    fn checkpoints_never_touch_the_users_index_head_branches_or_files() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "base.txt", "edited, not staged\n");
        write(&repo, "staged.txt", "staged\n");
        lab.git(&repo, &["add", "staged.txt"]);
        write(&repo, "loose.txt", "loose\n");
        lab.git(&repo, &["branch", "side"]);
        lab.git(&repo, &["tag", "v1"]);
        write(&repo, "agent.txt", "work\n");
        let refs_outside_newjen = |lab: &Lab| {
            lab.git(
                &repo,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                    "refs/tags",
                ],
            )
        };

        let status = lab.status(&repo);
        let staged = lab.git(&repo, &["diff", "--cached"]);
        let head = lab.head(&repo);
        let refs = refs_outside_newjen(&lab);
        let index = std::fs::read(repo.join(".git/index")).unwrap();
        let files = (
            read(&repo, "base.txt"),
            read(&repo, "staged.txt"),
            read(&repo, "loose.txt"),
        );

        let first = lab.checkpoint(&repo);
        write(&repo, "agent.txt", "more work\n");
        lab.checkpoint(&repo);

        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), index);
        assert_eq!(lab.head(&repo), head);
        assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(refs_outside_newjen(&lab), refs);
        assert_eq!(lab.git(&repo, &["diff", "--cached"]), staged);
        assert_eq!(lab.status(&repo), status);
        assert_eq!(
            files,
            (
                read(&repo, "base.txt"),
                read(&repo, "staged.txt"),
                read(&repo, "loose.txt")
            )
        );
        assert_eq!(lab.git(&repo, &["stash", "list"]), "");
        assert_eq!(
            lab.git(&repo, &["show", &format!("{first}:loose.txt")]),
            "loose"
        );
        assert_eq!(
            lab.git(&repo, &["show", &format!("{first}:base.txt")]),
            "edited, not staged"
        );
        assert_eq!(
            lab.git(&repo, &["show", &format!("{first}:staged.txt")]),
            "staged"
        );
    }

    #[test]
    fn checkpoints_work_without_a_git_identity() {
        let lab = lab();
        let repo = lab.repo_without_identity();
        write(&repo, "agent.txt", "work\n");
        let start = lab.checkpoint(&repo);
        write(&repo, "agent.txt", "more\n");
        lab.checkpoint(&repo);
        assert_eq!(lab.turns(&repo), 1);
        assert_eq!(
            lab.git(&repo, &["log", "-1", "--format=%an", &start]),
            "Newjen"
        );
    }

    #[test]
    fn checkpoints_run_no_hooks_and_ignore_commit_signing() {
        let lab = lab();
        let repo = lab.repo();
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["pre-commit", "commit-msg", "post-commit"] {
            script(&hooks.join(hook), "echo 'hook ran' >&2\nexit 1");
        }
        lab.git(&repo, &["config", "commit.gpgsign", "true"]);
        lab.git(&repo, &["config", "gpg.program", "/nonexistent/gpg"]);
        lab.git(&repo, &["config", "gpg.format", "ssh"]);
        lab.git(&repo, &["config", "user.signingkey", "/nonexistent/key"]);

        lab.checkpoint(&repo);
        write(&repo, "agent.txt", "work\n");
        let turn = lab.checkpoint(&repo);

        assert_eq!(lab.turns(&repo), 1);
        assert_eq!(lab.files(&repo, Scope::Turn(1)), names(&["agent.txt"]));
        assert_eq!(
            lab.git(&repo, &["cat-file", "commit", &turn])
                .matches("gpgsig")
                .count(),
            0
        );
    }

    #[test]
    fn a_reference_transaction_hook_cannot_veto_a_checkpoint() {
        let lab = lab();
        let repo = lab.repo();
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        script(
            &hooks.join("reference-transaction"),
            "echo 'hook ran' >&2\nexit 1",
        );

        let made = vcs::checkpoint(&lab.tools, &repo, THREAD);

        assert!(made.is_ok(), "{:#}", made.unwrap_err());
    }

    #[test]
    fn awkward_file_names_are_checkpointed_diffed_and_committed_exactly() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        let awkward = [
            "my file.txt",
            "ünï.txt",
            "-dash.txt",
            "[x].txt",
            "star*.txt",
            "dir with space/inner file.txt",
            "emoji 🚀.txt",
        ];
        for file in awkward {
            write(&repo, file, "work\n");
        }
        lab.checkpoint(&repo);

        let mut expected: Vec<String> = awkward.iter().map(|file| file.to_string()).collect();
        expected.sort();
        for scope in [Scope::All, Scope::Uncommitted, Scope::Turn(1)] {
            let patch = lab.patch(&repo, scope);
            for file in awkward {
                assert!(
                    patch.contains(&format!("b/{file}")),
                    "{scope:?} {file}\n{patch}"
                );
            }
            let summary = vcs::changed_files(&lab.tools, &repo, THREAD, scope).unwrap();
            assert_eq!(summary.files.len(), awkward.len(), "{scope:?}");
        }
        vcs::commit(&lab.tools, &repo, "feat: awkward names").unwrap();
        assert_eq!(lab.files_of(&repo, "HEAD"), expected);
        assert_eq!(lab.status(&repo), "");
    }

    #[test]
    fn changed_files_names_awkward_files_without_spaces_exactly() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        let awkward = [
            "ünï.txt",
            "-dash.txt",
            "[x].txt",
            "star*.txt",
            "emoji🚀.txt",
        ];
        for file in awkward {
            write(&repo, file, "work\n");
        }
        let mut expected: Vec<String> = awkward.iter().map(|file| file.to_string()).collect();
        expected.sort();
        assert_eq!(lab.files(&repo, Scope::All), expected);
    }

    #[test]
    fn changed_files_names_files_with_spaces_exactly() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        write(&repo, "my file.txt", "work\n");
        write(&repo, "dir with space/inner file.txt", "work\n");
        let summary = vcs::changed_files(&lab.tools, &repo, THREAD, Scope::All).unwrap();
        assert_eq!(
            summary_files(&summary),
            names(&["dir with space/inner file.txt", "my file.txt"])
        );
    }

    #[test]
    fn a_checkpoint_leaves_ignored_files_and_embedded_repositories_out() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, ".gitignore", "target/\n");
        lab.commit_all(&repo, "ignore");
        lab.checkpoint(&repo);

        write(&repo, "target/out.bin", "built\n");
        write(&repo, "agent.txt", "work\n");
        embedded_repo(&lab, &repo.join("newlib"));
        std::fs::create_dir_all(repo.join("vendor/tool")).unwrap();
        lab.git(&repo.join("vendor/tool"), &["init", "--quiet"]);
        let first = lab.checkpoint(&repo);
        let second = lab.checkpoint(&repo);

        let expected = names(&[".gitignore", "agent.txt", "base.txt", "other.txt"]);
        assert_eq!(lab.tree_files(&repo, &first), expected);
        assert_eq!(lab.tree_files(&repo, &second), expected);
        assert_eq!(lab.files(&repo, Scope::All), names(&["agent.txt"]));
        assert_eq!(lab.files(&repo, Scope::Uncommitted), names(&["agent.txt"]));
    }

    #[test]
    fn an_unborn_repository_gets_a_checkpoint_with_no_parent() {
        let lab = lab();
        let repo = lab.unborn_repo();
        write(&repo, "pre.txt", "was here first\n");
        let start = lab.checkpoint(&repo);
        assert_eq!(
            lab.git(&repo, &["rev-list", "--parents", "-n1", &start]),
            start
        );
        assert_eq!(lab.tree_files(&repo, &start), names(&["pre.txt"]));

        write(&repo, "new.txt", "agent work\n");
        lab.checkpoint(&repo);
        assert_eq!(lab.files(&repo, Scope::All), names(&["new.txt"]));
        assert_eq!(lab.files(&repo, Scope::Turn(1)), names(&["new.txt"]));
        assert_eq!(
            lab.files(&repo, Scope::Uncommitted),
            names(&["new.txt", "pre.txt"]),
            "with nothing committed yet, everything is uncommitted"
        );
    }

    #[test]
    fn a_detached_head_still_gets_checkpoints() {
        let lab = lab();
        let repo = lab.repo();
        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        lab.checkpoint(&repo);
        write(&repo, "agent.txt", "work\n");
        lab.checkpoint(&repo);
        assert_eq!(lab.files(&repo, Scope::All), names(&["agent.txt"]));
    }

    #[test]
    fn invalid_thread_ids_are_rejected_before_git_runs() {
        let lab = lab();
        let repo = lab.repo();
        let broken = Tools {
            git: PathBuf::from("/nonexistent/git"),
            env: Vec::new(),
        };
        let nested = format!("{THREAD}/../x");
        for id in ["", "../evil", "not-a-uuid", nested.as_str()] {
            for message in [
                error_text(vcs::checkpoint(&broken, &repo, id)),
                error_text(vcs::turns(&broken, &repo, id)),
                error_text(vcs::release(&broken, &repo, id)),
                error_text(vcs::diff(&lab.tools, &repo, id, Scope::All)),
                error_text(vcs::changed_files(&lab.tools, &repo, id, Scope::All)),
            ] {
                assert!(message.contains("Invalid thread id"), "{id:?}: {message}");
            }
        }
        assert_eq!(lab.git(&repo, &["for-each-ref", "refs/newjen"]), "");
    }

    #[test]
    fn a_folder_without_git_has_no_checkpoints_or_changes() {
        let lab = lab();
        let plain = lab.folder("plain");
        write(&plain, "a.txt", "a\n");
        assert!(!vcs::available(&plain));
        assert!(vcs::checkpoint(&lab.tools, &plain, THREAD).is_err());
        assert!(
            error_text(vcs::diff(&lab.tools, &plain, THREAD, Scope::All))
                .contains("git repository")
        );
        assert!(vcs::status(&lab.tools, &plain).is_err());
        assert!(vcs::available(&lab.repo()));
    }

    #[test]
    fn release_forgets_the_checkpoints_and_keeps_the_files_and_other_threads() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        write(&repo, "agent.txt", "work\n");
        lab.checkpoint(&repo);
        lab.checkpoint_of(&repo, OTHER);
        let head = lab.head(&repo);
        assert!(repo.join(format!(".git/newjen/{THREAD}.index")).exists());

        vcs::release(&lab.tools, &repo, THREAD).unwrap();

        assert_eq!(lab.refs(&repo, THREAD), vec![]);
        assert_eq!(lab.turns(&repo), 0);
        assert!(vcs::diff(&lab.tools, &repo, THREAD, Scope::All).is_err());
        assert!(!repo.join(format!(".git/newjen/{THREAD}.index")).exists());
        assert_eq!(read(&repo, "agent.txt"), "work\n");
        assert_eq!(lab.head(&repo), head);
        assert_eq!(lab.refs(&repo, OTHER).len(), 1);
        vcs::release(&lab.tools, &repo, THREAD).unwrap();

        lab.checkpoint(&repo);
        assert_eq!(
            lab.refs(&repo, THREAD).len(),
            1,
            "a new start, numbered from 0"
        );
        assert_eq!(
            lab.files(&repo, Scope::All),
            Vec::<String>::new(),
            "the new start already holds the files that are there now"
        );
    }

    #[test]
    fn checkpoints_work_from_a_linked_worktree_without_moving_its_branch() {
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
        let head = lab.head(&linked);
        lab.checkpoint(&linked);
        write(&linked, "agent.txt", "work\n");
        lab.checkpoint(&linked);
        assert_eq!(lab.files(&linked, Scope::All), names(&["agent.txt"]));
        assert_eq!(lab.head(&linked), head);
        assert_eq!(lab.git(&repo, &["rev-parse", "main"]), head);
        assert_eq!(lab.git(&repo, &["rev-parse", "feature"]), head);
        assert_eq!(lab.status(&repo), "");
    }
}

mod scopes {
    use super::*;

    #[test]
    fn all_is_the_work_since_the_start_without_what_was_already_there() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "other.txt", "the user's edit, before the thread\n");
        write(&repo, "pre.txt", "the user's new file\n");
        lab.checkpoint(&repo);

        write(&repo, "base.txt", "agent edit\n");
        write(&repo, "agent.txt", "agent file\n");

        assert_eq!(
            lab.files(&repo, Scope::All),
            names(&["agent.txt", "base.txt"])
        );
        let patch = lab.patch(&repo, Scope::All);
        assert!(patch.contains("+agent edit"), "{patch}");
        assert!(
            !patch.contains("other.txt") && !patch.contains("pre.txt"),
            "{patch}"
        );
        assert_eq!(
            lab.files(&repo, Scope::Uncommitted),
            names(&["agent.txt", "base.txt", "other.txt", "pre.txt"]),
            "uncommitted is everything that differs from HEAD, whoever wrote it"
        );
    }

    #[test]
    fn all_includes_what_the_agent_committed_and_what_it_left_uncommitted() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);

        write(&repo, "base.txt", "committed by the agent\n");
        lab.commit_all(&repo, "agent commit");
        write(&repo, "other.txt", "left uncommitted\n");

        assert_eq!(
            lab.files(&repo, Scope::All),
            names(&["base.txt", "other.txt"])
        );
        let patch = lab.patch(&repo, Scope::All);
        assert!(patch.contains("+committed by the agent"), "{patch}");
        assert!(patch.contains("+left uncommitted"), "{patch}");
        assert_eq!(lab.files(&repo, Scope::Uncommitted), names(&["other.txt"]));

        lab.checkpoint(&repo);
        assert_eq!(
            lab.files(&repo, Scope::Turn(1)),
            names(&["base.txt", "other.txt"]),
            "a turn's diff holds the commit the agent made in it"
        );
    }

    #[test]
    fn committing_does_not_hide_the_work_from_all() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        write(&repo, "agent.txt", "work\n");
        vcs::commit(&lab.tools, &repo, "feat: add agent").unwrap();
        assert_eq!(lab.files(&repo, Scope::Uncommitted), Vec::<String>::new());
        assert_eq!(lab.files(&repo, Scope::All), names(&["agent.txt"]));
    }

    #[test]
    fn a_turn_is_what_changed_between_its_two_checkpoints() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "pre.txt", "uncommitted before the thread\n");
        lab.checkpoint(&repo);

        write(&repo, "t1.txt", "one\n");
        write(&repo, "base.txt", "turn one edit\n");
        lab.checkpoint(&repo);
        std::fs::remove_file(repo.join("t1.txt")).unwrap();
        write(&repo, "t2.txt", "two\n");
        lab.checkpoint(&repo);
        write(&repo, "after.txt", "edited after the last checkpoint\n");

        assert_eq!(
            lab.files(&repo, Scope::Turn(1)),
            names(&["base.txt", "t1.txt"])
        );
        assert_eq!(
            lab.files(&repo, Scope::Turn(2)),
            names(&["t1.txt", "t2.txt"])
        );
        let summary = vcs::changed_files(&lab.tools, &repo, THREAD, Scope::Turn(2)).unwrap();
        let kind = |path: &str| {
            summary
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap()
                .kind
        };
        assert_eq!(kind("t1.txt"), ChangeKind::Deleted);
        assert_eq!(kind("t2.txt"), ChangeKind::Added);
        assert_eq!(
            lab.files(&repo, Scope::All),
            names(&["after.txt", "base.txt", "t2.txt"]),
            "the live folder is not part of any turn"
        );
        assert!(!lab.patch(&repo, Scope::Turn(1)).contains("after.txt"));
    }

    #[test]
    fn turns_outside_the_recorded_range_are_refused() {
        let lab = lab();
        let repo = lab.repo();
        assert!(error_text(vcs::diff(&lab.tools, &repo, THREAD, Scope::All)).contains("starting"));
        lab.checkpoint(&repo);
        write(&repo, "agent.txt", "work\n");
        lab.checkpoint(&repo);
        for turn in [0, 2, 99] {
            assert!(
                error_text(vcs::diff(&lab.tools, &repo, THREAD, Scope::Turn(turn)))
                    .contains("no turn"),
                "turn {turn}"
            );
            assert!(vcs::changed_files(&lab.tools, &repo, THREAD, Scope::Turn(turn)).is_err());
        }
        assert!(vcs::diff(&lab.tools, &repo, THREAD, Scope::Turn(1)).is_ok());
    }

    #[test]
    fn changed_files_counts_added_modified_deleted_and_binary_files() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "base.txt", "one\ntwo\nthree\n");
        lab.commit_all(&repo, "three lines");
        lab.checkpoint(&repo);

        write(&repo, "base.txt", "one\ntwo\nthree\nfour\nfive\n");
        std::fs::remove_file(repo.join("other.txt")).unwrap();
        write(&repo, "new.txt", "a\nb\nc\n");
        std::fs::write(repo.join("img.bin"), [0u8, 1, 2, 0, 255]).unwrap();

        let summary = vcs::changed_files(&lab.tools, &repo, THREAD, Scope::All).unwrap();
        let stat = |path: &str| summary.files.iter().find(|file| file.path == path).unwrap();
        assert_eq!(summary.files.len(), 4);
        assert_eq!(
            (
                stat("base.txt").kind,
                stat("base.txt").added,
                stat("base.txt").removed
            ),
            (ChangeKind::Modified, 2, 0)
        );
        assert_eq!(
            (
                stat("other.txt").kind,
                stat("other.txt").added,
                stat("other.txt").removed
            ),
            (ChangeKind::Deleted, 0, 1)
        );
        assert_eq!(
            (
                stat("new.txt").kind,
                stat("new.txt").added,
                stat("new.txt").removed
            ),
            (ChangeKind::Added, 3, 0)
        );
        assert!(stat("img.bin").binary);
        assert_eq!((summary.added(), summary.removed()), (5, 1));
        assert_eq!(
            lab.files(&repo, Scope::All),
            names(&["base.txt", "img.bin", "new.txt", "other.txt"])
        );
    }

    #[test]
    fn a_huge_diff_is_cut_off_but_still_counted() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        let lines = 12_000;
        write(&repo, "big.txt", &"a line of text\n".repeat(lines));

        let diff = vcs::diff(&lab.tools, &repo, THREAD, Scope::All).unwrap();
        assert!(diff.truncated);
        assert!(diff.patch.len() <= vcs::MAX_PATCH_BYTES);
        let summary = vcs::changed_files(&lab.tools, &repo, THREAD, Scope::All).unwrap();
        assert_eq!(summary.added(), lines);

        write(&repo, "big.txt", "small\n");
        let small = vcs::diff(&lab.tools, &repo, THREAD, Scope::All).unwrap();
        assert!(!small.truncated);
    }

    #[test]
    fn a_thread_in_a_subfolder_sees_only_its_own_folder() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "apps/web/index.txt", "web\n");
        write(&repo, "apps/api/main.txt", "api\n");
        lab.commit_all(&repo, "apps");
        let web = repo.join("apps/web");

        let start = lab.checkpoint(&web);
        write(&web, "index.txt", "web by agent\n");
        write(&repo, "apps/api/main.txt", "api by user\n");
        write(&repo, "apps/api/new.txt", "new by user\n");
        write(&repo, "top.txt", "top by user\n");
        let end = lab.checkpoint(&web);

        assert_eq!(lab.files(&web, Scope::All), names(&["apps/web/index.txt"]));
        assert_eq!(
            lab.files(&web, Scope::Uncommitted),
            names(&["apps/web/index.txt"])
        );
        assert_eq!(
            lab.files(&web, Scope::Turn(1)),
            names(&["apps/web/index.txt"])
        );
        assert_eq!(
            lab.git(&repo, &["show", &format!("{end}:apps/api/main.txt")]),
            "api",
            "a checkpoint holds the committed version of everything outside the folder"
        );
        assert_eq!(
            lab.tree_files(&repo, &end),
            lab.tree_files(&repo, &start),
            "and does not pick up the user's new files there"
        );
    }

    #[test]
    fn threads_in_separate_folders_of_one_repository_stay_apart() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "a/a.txt", "a\n");
        write(&repo, "b/b.txt", "b\n");
        lab.commit_all(&repo, "a and b");
        let a = repo.join("a");
        let b = repo.join("b");

        lab.checkpoint_of(&a, THREAD);
        lab.checkpoint_of(&b, OTHER);
        write(&b, "new.txt", "thread B's work\n");
        lab.checkpoint_of(&b, OTHER);
        lab.checkpoint_of(&a, THREAD);

        assert_eq!(
            lab.files_of_thread(&a, THREAD, Scope::All),
            Vec::<String>::new()
        );
        assert_eq!(
            lab.files_of_thread(&b, OTHER, Scope::All),
            names(&["b/new.txt"])
        );
        assert_eq!(
            lab.files_of_thread(&a, THREAD, Scope::Turn(1)),
            Vec::<String>::new()
        );
        assert_eq!(
            lab.files_of_thread(&b, OTHER, Scope::Turn(1)),
            names(&["b/new.txt"])
        );
    }

    #[test]
    fn two_threads_in_one_folder_each_measure_from_their_own_start() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint_of(&repo, THREAD);
        write(&repo, "a.txt", "first\n");
        lab.checkpoint_of(&repo, OTHER);
        write(&repo, "b.txt", "second\n");
        lab.checkpoint_of(&repo, THREAD);
        lab.checkpoint_of(&repo, OTHER);

        assert_eq!(
            lab.files_of_thread(&repo, THREAD, Scope::All),
            names(&["a.txt", "b.txt"])
        );
        assert_eq!(
            lab.files_of_thread(&repo, OTHER, Scope::All),
            names(&["b.txt"])
        );
        assert_eq!(
            (lab.refs(&repo, THREAD).len(), lab.refs(&repo, OTHER).len()),
            (2, 2)
        );
    }
}

mod status {
    use super::*;

    #[test]
    fn without_a_remote_a_repository_reports_its_branch_and_whether_it_is_dirty() {
        let lab = lab();
        let repo = lab.repo();
        assert_eq!(
            lab.status_of(&repo),
            vcs::Status {
                branch: Some("main".to_string()),
                upstream: false,
                ahead: 0,
                behind: 0,
                dirty: false,
                remote: false,
                default_branch: true,
            }
        );

        write(&repo, "base.txt", "edited\n");
        assert!(lab.status_of(&repo).dirty);
        lab.commit_all(&repo, "edit");
        assert!(!lab.status_of(&repo).dirty);
        write(&repo, "new.txt", "untracked\n");
        assert!(lab.status_of(&repo).dirty, "untracked files count");
        std::fs::remove_file(repo.join("new.txt")).unwrap();
        std::fs::remove_file(repo.join("other.txt")).unwrap();
        assert!(lab.status_of(&repo).dirty, "deletions count");
    }

    #[test]
    fn the_default_branch_is_main_or_master_when_no_remote_says_otherwise() {
        let lab = lab();
        let repo = lab.repo();
        lab.git(&repo, &["switch", "--quiet", "-c", "feature"]);
        let on_feature = lab.status_of(&repo);
        assert_eq!(on_feature.branch.as_deref(), Some("feature"));
        assert!(!on_feature.default_branch);
        lab.git(&repo, &["switch", "--quiet", "-c", "master"]);
        assert!(lab.status_of(&repo).default_branch);
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        let lab = lab();
        let repo = lab.repo();
        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        let status = lab.status_of(&repo);
        assert_eq!(status.branch, None);
        assert!(!status.default_branch);
        assert!(!status.upstream);
    }

    #[test]
    fn against_a_remote_it_counts_commits_ahead_and_behind() {
        let lab = lab();
        let repo = lab.repo();
        let bare = lab.bare_remote(&repo);
        let synced = lab.status_of(&repo);
        assert!(synced.remote && synced.upstream && synced.default_branch);
        assert_eq!((synced.ahead, synced.behind), (0, 0));

        write(&repo, "local.txt", "local\n");
        lab.commit_all(&repo, "local one");
        write(&repo, "local.txt", "local two\n");
        lab.commit_all(&repo, "local two");
        let ahead = lab.status_of(&repo);
        assert_eq!((ahead.ahead, ahead.behind), (2, 0));

        let other = lab.folder("other-clone");
        lab.git(&other, &["clone", "--quiet", bare.to_str().unwrap(), "."]);
        lab.git(&other, &["config", "user.name", "Test"]);
        lab.git(&other, &["config", "user.email", "test@example.com"]);
        write(&other, "remote.txt", "remote\n");
        lab.commit_all(&other, "remote one");
        lab.git(&other, &["push", "--quiet", "origin", "main"]);
        lab.git(&repo, &["fetch", "--quiet", "origin"]);
        let diverged = lab.status_of(&repo);
        assert_eq!((diverged.ahead, diverged.behind), (2, 1));
    }

    #[test]
    fn a_branch_without_an_upstream_is_not_the_remotes_default() {
        let lab = lab();
        let repo = lab.repo();
        lab.bare_remote(&repo);
        lab.git(&repo, &["switch", "--quiet", "-c", "feature"]);
        let status = lab.status_of(&repo);
        assert!(status.remote);
        assert!(!status.upstream);
        assert!(!status.default_branch);
        assert_eq!((status.ahead, status.behind), (0, 0));
    }

    #[test]
    fn the_remotes_own_head_decides_which_branch_is_the_default() {
        let lab = lab();
        let repo = lab.repo();
        lab.bare_remote(&repo);
        lab.git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/develop",
            ],
        );
        assert!(
            !lab.status_of(&repo).default_branch,
            "main is not the default once origin points at develop"
        );
        lab.git(&repo, &["switch", "--quiet", "-c", "develop"]);
        assert!(lab.status_of(&repo).default_branch);
    }

    #[test]
    fn dirty_ignores_ignored_files_embedded_repositories_and_other_folders() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, ".gitignore", "target/\n");
        write(&repo, "apps/web/index.txt", "web\n");
        write(&repo, "apps/api/main.txt", "api\n");
        lab.commit_all(&repo, "setup");

        write(&repo, "target/out.bin", "built\n");
        embedded_repo(&lab, &repo.join("newlib"));
        assert!(!lab.status_of(&repo).dirty);

        write(&repo, "apps/api/main.txt", "api by user\n");
        assert!(lab.status_of(&repo).dirty);
        assert!(
            !lab.status_of(&repo.join("apps/web")).dirty,
            "a change outside the thread's folder is not the thread's"
        );
        write(&repo, "apps/web/index.txt", "web by agent\n");
        assert!(lab.status_of(&repo.join("apps/web")).dirty);
    }

    #[test]
    fn a_staged_change_makes_the_folder_dirty() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "staged.txt", "staged\n");
        lab.git(&repo, &["add", "staged.txt"]);
        assert!(lab.status_of(&repo).dirty);
    }

    #[test]
    fn an_unborn_repository_has_a_branch_and_is_dirty_once_it_has_files() {
        let lab = lab();
        let repo = lab.unborn_repo();
        assert!(!lab.status_of(&repo).dirty);
        write(&repo, "first.txt", "first\n");
        let status = lab.status_of(&repo);
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(status.dirty);
    }

    #[test]
    fn a_newjen_worktree_reports_its_own_branch() {
        let lab = lab();
        let repo = lab.repo();
        let wt = lab.worktree(&repo);
        let status = lab.status_of(&wt.workdir);
        assert_eq!(status.branch.as_deref(), Some(wt.branch.as_str()));
        assert!(!status.default_branch);
        assert!(!status.dirty);
    }
}

mod commit {
    use super::*;

    #[test]
    fn commit_stages_the_folder_and_commits_on_the_current_branch() {
        let lab = lab();
        let repo = lab.repo();
        let before = lab.head(&repo);
        write(&repo, "base.txt", "edited\n");
        write(&repo, "new.txt", "new\n");
        std::fs::remove_file(repo.join("other.txt")).unwrap();

        vcs::commit(&lab.tools, &repo, "feat: change things\n\nWith a body.").unwrap();

        assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^"]), before);
        assert_eq!(
            lab.files_of(&repo, "HEAD"),
            names(&["base.txt", "new.txt", "other.txt"])
        );
        assert_eq!(
            lab.git(&repo, &["log", "-1", "--format=%B"]),
            "feat: change things\n\nWith a body."
        );
        assert_eq!(
            lab.git(&repo, &["log", "-1", "--format=%an <%ae>"]),
            "Test <test@example.com>"
        );
        assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(lab.status(&repo), "");
    }

    #[test]
    fn commit_in_a_subfolder_leaves_the_rest_of_the_repository_uncommitted() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "apps/web/index.txt", "web\n");
        write(&repo, "apps/api/main.txt", "api\n");
        lab.commit_all(&repo, "apps");
        write(&repo, "apps/web/index.txt", "web by agent\n");
        write(&repo, "apps/api/main.txt", "api by user\n");
        write(&repo, "top.txt", "top by user\n");

        vcs::commit(&lab.tools, &repo.join("apps/web"), "fix: web").unwrap();

        assert_eq!(lab.files_of(&repo, "HEAD"), names(&["apps/web/index.txt"]));
        assert_eq!(lab.status(&repo), " M apps/api/main.txt\n?? top.txt");
    }

    #[test]
    fn commit_in_a_subfolder_does_not_sweep_in_what_the_user_staged_elsewhere() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "apps/web/index.txt", "web\n");
        lab.commit_all(&repo, "apps");
        write(&repo, "apps/web/index.txt", "web by agent\n");
        write(
            &repo,
            "top.txt",
            "staged by the user for a different commit\n",
        );
        lab.git(&repo, &["add", "top.txt"]);

        vcs::commit(&lab.tools, &repo.join("apps/web"), "fix: web").unwrap();

        assert_eq!(lab.files_of(&repo, "HEAD"), names(&["apps/web/index.txt"]));
        assert_eq!(lab.status(&repo), "A  top.txt");
    }

    #[test]
    fn commit_leaves_embedded_repositories_out() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "agent.txt", "work\n");
        embedded_repo(&lab, &repo.join("newlib"));
        vcs::commit(&lab.tools, &repo, "feat: agent").unwrap();
        assert_eq!(lab.files_of(&repo, "HEAD"), names(&["agent.txt"]));
        assert_eq!(read(&repo, "newlib/lib.txt"), "cloned\n");
    }

    #[test]
    fn commit_makes_the_first_commit_of_an_unborn_repository() {
        let lab = lab();
        let repo = lab.unborn_repo();
        write(&repo, "first.txt", "first\n");
        vcs::commit(&lab.tools, &repo, "chore: start").unwrap();
        assert_eq!(lab.files_of(&repo, "HEAD"), names(&["first.txt"]));
        assert_eq!(lab.git(&repo, &["rev-list", "--count", "HEAD"]), "1");
    }

    #[test]
    fn commit_refuses_an_empty_message_a_clean_folder_and_a_detached_head() {
        let lab = lab();
        let repo = lab.repo();
        let head = lab.head(&repo);

        write(&repo, "agent.txt", "work\n");
        for message in ["", "   \n"] {
            assert!(vcs::commit(&lab.tools, &repo, message).is_err());
        }
        assert_eq!(lab.head(&repo), head);
        assert_eq!(
            lab.status(&repo),
            "?? agent.txt",
            "nothing was staged either"
        );

        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        assert!(error_text(vcs::commit(&lab.tools, &repo, "feat: x")).contains("detached"));
        assert_eq!(lab.head(&repo), head);
        assert_eq!(lab.status(&repo), "?? agent.txt");

        lab.git(&repo, &["switch", "--quiet", "main"]);
        std::fs::remove_file(repo.join("agent.txt")).unwrap();
        assert!(
            error_text(vcs::commit(&lab.tools, &repo, "feat: x")).contains("nothing to commit")
        );
        assert_eq!(lab.head(&repo), head);
    }

    #[test]
    fn commit_runs_the_projects_git_hooks() {
        let lab = lab();
        let repo = lab.repo();
        let hooks = repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let head = lab.head(&repo);
        write(&repo, "agent.txt", "work\n");

        script(&hooks.join("pre-commit"), "echo 'lint says no' >&2\nexit 1");
        let refused = error_text(vcs::commit(&lab.tools, &repo, "feat: agent"));
        assert!(refused.contains("lint says no"), "{refused}");
        assert_eq!(lab.head(&repo), head);
        assert!(
            lab.status_of(&repo).dirty,
            "the work is still there to commit"
        );

        let marker = repo.join("hook-ran");
        script(
            &hooks.join("pre-commit"),
            &format!("touch '{}'\nexit 0", marker.display()),
        );
        vcs::commit(&lab.tools, &repo, "feat: agent").unwrap();
        assert!(marker.exists());
        assert_eq!(lab.count(&repo, &format!("{head}..HEAD")), 1);
    }

    #[test]
    fn commit_needs_the_users_own_git_identity() {
        let lab = lab();
        let repo = lab.repo_without_identity();
        let head = lab.head(&repo);
        write(&repo, "agent.txt", "work\n");

        assert!(vcs::commit(&lab.tools, &repo, "feat: agent").is_err());
        assert_eq!(lab.head(&repo), head);

        lab.git(&repo, &["config", "user.name", "Real Person"]);
        lab.git(&repo, &["config", "user.email", "real@example.com"]);
        vcs::commit(&lab.tools, &repo, "feat: agent").unwrap();
        assert_eq!(
            lab.git(&repo, &["log", "-1", "--format=%an <%ae>"]),
            "Real Person <real@example.com>"
        );
    }

    #[test]
    fn uncommitted_patch_is_what_commit_would_commit() {
        let lab = lab();
        let repo = lab.repo();
        lab.checkpoint(&repo);
        write(&repo, "base.txt", "edited\n");
        write(&repo, "new.txt", "new\n");
        let patch = vcs::uncommitted_patch(&lab.tools, &repo, THREAD).unwrap();
        assert_eq!(patch_files(&patch.patch), names(&["base.txt", "new.txt"]));
        vcs::commit(&lab.tools, &repo, "feat: both").unwrap();
        assert_eq!(
            vcs::uncommitted_patch(&lab.tools, &repo, THREAD)
                .unwrap()
                .patch,
            ""
        );
    }
}

mod push {
    use super::*;

    #[test]
    fn the_first_push_sets_the_upstream_and_later_ones_just_push() {
        let lab = lab();
        let repo = lab.repo();
        let bare = lab.bare_remote(&repo);
        lab.git(&repo, &["switch", "--quiet", "-c", "feature"]);
        write(&repo, "agent.txt", "work\n");
        vcs::commit(&lab.tools, &repo, "feat: agent").unwrap();
        assert!(!lab.status_of(&repo).upstream);

        vcs::push(&lab.tools, &repo).unwrap();

        assert_eq!(lab.remote_head(&bare, "feature"), lab.head(&repo));
        assert_eq!(
            lab.git(&repo, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
            "origin/feature"
        );
        let pushed = lab.status_of(&repo);
        assert!(pushed.upstream);
        assert_eq!((pushed.ahead, pushed.behind), (0, 0));

        write(&repo, "agent.txt", "more\n");
        vcs::commit(&lab.tools, &repo, "feat: more").unwrap();
        vcs::push(&lab.tools, &repo).unwrap();
        assert_eq!(lab.remote_head(&bare, "feature"), lab.head(&repo));
    }

    #[test]
    fn push_refuses_a_detached_head_a_missing_remote_and_a_rejected_update() {
        let lab = lab();
        let repo = lab.repo();
        assert!(
            vcs::push(&lab.tools, &repo).is_err(),
            "no remote to push to"
        );

        let bare = lab.bare_remote(&repo);
        let remote_before = lab.remote_head(&bare, "main");
        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        assert!(error_text(vcs::push(&lab.tools, &repo)).contains("detached"));
        lab.git(&repo, &["switch", "--quiet", "main"]);

        let other = lab.folder("other-clone");
        lab.git(&other, &["clone", "--quiet", bare.to_str().unwrap(), "."]);
        lab.git(&other, &["config", "user.name", "Test"]);
        lab.git(&other, &["config", "user.email", "test@example.com"]);
        write(&other, "remote.txt", "remote\n");
        lab.commit_all(&other, "remote work");
        lab.git(&other, &["push", "--quiet", "origin", "main"]);
        let moved = lab.remote_head(&bare, "main");
        assert_ne!(moved, remote_before);

        write(&repo, "local.txt", "local\n");
        vcs::commit(&lab.tools, &repo, "feat: local").unwrap();
        assert!(vcs::push(&lab.tools, &repo).is_err(), "the remote moved on");
        assert_eq!(lab.remote_head(&bare, "main"), moved, "and was left alone");
    }
}

mod pull_request {
    use super::*;

    fn fake_gh(lab: &Lab, body: &str) -> (PathBuf, PathBuf) {
        let record = lab.dir.path().join("gh-record");
        std::fs::create_dir_all(&record).unwrap();
        let gh = lab.dir.path().join("gh");
        script(
            &gh,
            &format!(
                "printf '%s\\0' \"$@\" > '{record}/args'\npwd -P > '{record}/cwd'\n{body}",
                record = record.display()
            ),
        );
        (gh, record)
    }

    fn recorded_args(record: &Path) -> Vec<String> {
        std::fs::read_to_string(record.join("args"))
            .unwrap()
            .split_terminator('\0')
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn it_runs_gh_for_the_current_branch_and_returns_the_pull_request_url() {
        let lab = lab();
        let repo = lab.repo();
        lab.git(&repo, &["switch", "--quiet", "-c", "feature"]);
        let (gh, record) = fake_gh(
            &lab,
            "echo 'Creating pull request for feature into main'\necho 'https://github.com/acme/widgets/pull/7'",
        );

        let url = vcs::create_pr(
            &lab.tools,
            &gh,
            &repo,
            "feat: add widgets",
            "First line.\n\nSecond paragraph.",
        )
        .unwrap();

        assert_eq!(url, "https://github.com/acme/widgets/pull/7");
        assert_eq!(
            recorded_args(&record),
            [
                "pr",
                "create",
                "--head",
                "feature",
                "--title",
                "feat: add widgets",
                "--body",
                "First line.\n\nSecond paragraph.",
            ]
        );
        assert_eq!(
            std::fs::read_to_string(record.join("cwd")).unwrap().trim(),
            repo.to_str().unwrap()
        );
    }

    #[test]
    fn a_failing_gh_reports_what_it_said() {
        let lab = lab();
        let repo = lab.repo();
        let (gh, _record) = fake_gh(&lab, "echo 'gh: not logged in' >&2\nexit 4");
        let message = error_text(vcs::create_pr(&lab.tools, &gh, &repo, "t", "b"));
        assert!(message.contains("not logged in"), "{message}");
    }

    #[test]
    fn a_detached_head_never_reaches_gh() {
        let lab = lab();
        let repo = lab.repo();
        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        let (gh, record) = fake_gh(&lab, "echo https://github.com/acme/widgets/pull/1");
        assert!(error_text(vcs::create_pr(&lab.tools, &gh, &repo, "t", "b")).contains("branch"));
        assert!(!record.join("args").exists(), "gh was not run");
    }

    #[test]
    fn a_missing_gh_is_an_error_not_a_panic() {
        let lab = lab();
        let repo = lab.repo();
        let gone = lab.dir.path().join("no-gh-here");
        assert!(vcs::create_pr(&lab.tools, &gone, &repo, "t", "b").is_err());
    }
}

mod merge {
    use super::*;

    /// A worktree with `file` committed on its branch.
    fn committed(lab: &Lab, repo: &Path, file: &str, text: &str) -> Worktree {
        let wt = lab.worktree(repo);
        write(&wt.workdir, file, text);
        lab.commit_all(&wt.path, "agent commit");
        wt
    }

    #[test]
    fn the_branch_is_merged_with_a_merge_commit_and_the_worktree_stays() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "owner.txt", "owner's uncommitted file\n");
        let wt = committed(&lab, &repo, "agent.txt", "agent work\n");
        let head = lab.head(&repo);
        let branch_head = lab.git(&wt.path, &["rev-parse", "HEAD"]);

        vcs::merge_worktree(&lab.tools, &wt, "Add the agent's work").unwrap();

        let line = lab.git(&repo, &["rev-list", "--parents", "-n1", "HEAD"]);
        let parents: Vec<&str> = line.split(' ').skip(1).collect();
        assert_eq!(parents, vec![head.as_str(), branch_head.as_str()]);
        assert_eq!(read(&repo, "agent.txt"), "agent work\n");
        assert_eq!(
            lab.git(&repo, &["log", "-1", "--format=%s"]),
            format!("Merge {}: Add the agent's work", wt.branch)
        );
        assert_eq!(lab.git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(
            lab.status(&repo),
            "?? owner.txt",
            "the project's own uncommitted files are left alone"
        );
        assert!(wt.path.exists());
        assert!(!lab.merging(&repo));
    }

    #[test]
    fn merging_again_or_with_nothing_committed_changes_nothing() {
        let lab = lab();
        let repo = lab.repo();
        let wt = lab.worktree(&repo);
        let head = lab.head(&repo);
        vcs::merge_worktree(&lab.tools, &wt, "Nothing").unwrap();
        assert_eq!(lab.head(&repo), head);

        write(&wt.workdir, "agent.txt", "agent work\n");
        lab.commit_all(&wt.path, "agent commit");
        vcs::merge_worktree(&lab.tools, &wt, "Add work").unwrap();
        let merged = lab.head(&repo);
        assert_ne!(merged, head);
        vcs::merge_worktree(&lab.tools, &wt, "Add work").unwrap();
        assert_eq!(lab.head(&repo), merged);

        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        vcs::merge_worktree(&lab.tools, &wt, "Add work").unwrap();
        assert_eq!(
            lab.head(&repo),
            merged,
            "a merged branch is a no-op even on a detached HEAD"
        );
    }

    #[test]
    fn only_committed_work_is_merged() {
        let lab = lab();
        let repo = lab.repo();
        let wt = committed(&lab, &repo, "committed.txt", "committed\n");
        write(&wt.workdir, "loose.txt", "not committed\n");
        write(&wt.workdir, "base.txt", "edited, not committed\n");

        vcs::merge_worktree(&lab.tools, &wt, "Add work").unwrap();

        assert_eq!(read(&repo, "committed.txt"), "committed\n");
        assert!(!repo.join("loose.txt").exists());
        assert_eq!(read(&repo, "base.txt"), "base\n");
    }

    #[test]
    fn after_the_project_moved_on_both_sides_are_kept() {
        let lab = lab();
        let repo = lab.repo();
        let wt = committed(&lab, &repo, "agent.txt", "agent work\n");
        write(&repo, "owner.txt", "owner work\n");
        lab.commit_all(&repo, "owner commit");
        let owner = lab.head(&repo);
        let branch_head = lab.git(&wt.path, &["rev-parse", "HEAD"]);

        vcs::merge_worktree(&lab.tools, &wt, "Add work").unwrap();

        assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^1"]), owner);
        assert_eq!(lab.git(&repo, &["rev-parse", "HEAD^2"]), branch_head);
        assert_eq!(read(&repo, "agent.txt"), "agent work\n");
        assert_eq!(read(&repo, "owner.txt"), "owner work\n");
        assert_eq!(lab.status(&repo), "");
    }

    #[test]
    fn a_project_below_the_repository_root_merges_like_any_other() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, "apps/web/index.txt", "web\n");
        lab.commit_all(&repo, "web");
        let wt = worktree::create(&lab.tools, &repo.join("apps/web"), NAME, |_| {}).unwrap();
        write(&wt.workdir, "page.txt", "page\n");
        lab.commit_all(&wt.path, "agent commit");

        vcs::merge_worktree(&lab.tools, &wt, "Add a page").unwrap();

        assert_eq!(read(&repo, "apps/web/page.txt"), "page\n");
    }

    #[test]
    fn a_predicted_conflict_is_refused_and_leaves_the_project_untouched() {
        let lab = lab();
        let repo = lab.repo();
        let wt = committed(&lab, &repo, "base.txt", "agent's version\n");
        write(&repo, "base.txt", "owner's version\n");
        lab.commit_all(&repo, "owner commit");
        write(&repo, "other.txt", "owner's uncommitted edit\n");
        write(&repo, "scratch.txt", "scratch\n");
        let head = lab.head(&repo);
        let status = lab.status(&repo);

        let message = error_text(vcs::merge_worktree(&lab.tools, &wt, "Add work"));

        assert!(message.contains("conflict"), "{message}");
        assert_eq!(lab.head(&repo), head);
        assert_eq!(lab.status(&repo), status);
        assert!(!lab.merging(&repo));
        assert_eq!(read(&repo, "base.txt"), "owner's version\n");
        assert_eq!(read(&repo, "other.txt"), "owner's uncommitted edit\n");
    }

    #[test]
    fn a_merge_the_projects_uncommitted_changes_would_block_is_undone() {
        let lab = lab();
        let repo = lab.repo();
        let wt = committed(&lab, &repo, "other.txt", "agent's version\n");
        write(&repo, "other.txt", "owner's uncommitted edit\n");
        let head = lab.head(&repo);

        let message = error_text(vcs::merge_worktree(&lab.tools, &wt, "Add work"));

        assert!(message.contains("merge failed"), "{message}");
        assert_eq!(lab.head(&repo), head);
        assert_eq!(read(&repo, "other.txt"), "owner's uncommitted edit\n");
        assert!(!lab.merging(&repo));
        assert_eq!(lab.status(&repo), " M other.txt");
    }

    #[test]
    fn a_detached_project_is_refused_and_no_branch_moves() {
        let lab = lab();
        let repo = lab.repo();
        let wt = committed(&lab, &repo, "agent.txt", "agent work\n");
        lab.git(&repo, &["checkout", "--quiet", "--detach"]);
        let main = lab.git(&repo, &["rev-parse", "main"]);
        let head = lab.head(&repo);

        assert!(error_text(vcs::merge_worktree(&lab.tools, &wt, "Add work")).contains("detached"));

        assert_eq!(lab.head(&repo), head);
        assert_eq!(lab.git(&repo, &["rev-parse", "main"]), main);
        assert!(!repo.join("agent.txt").exists());
    }

    #[test]
    fn a_merge_already_in_progress_in_the_project_is_not_touched() {
        let lab = lab();
        let repo = lab.repo();
        let wt = committed(&lab, &repo, "agent.txt", "agent work\n");

        lab.git(&repo, &["switch", "--quiet", "-c", "elsewhere"]);
        write(&repo, "base.txt", "elsewhere\n");
        lab.commit_all(&repo, "elsewhere");
        lab.git(&repo, &["switch", "--quiet", "main"]);
        write(&repo, "base.txt", "main\n");
        lab.commit_all(&repo, "main");
        assert!(
            !lab.output(&repo, &["merge", "--quiet", "elsewhere"])
                .status
                .success()
        );
        assert!(lab.merging(&repo));
        let head = lab.head(&repo);

        assert!(
            error_text(vcs::merge_worktree(&lab.tools, &wt, "Add work"))
                .contains("middle of a merge")
        );

        assert!(lab.merging(&repo), "the user's merge was not aborted");
        assert_eq!(lab.head(&repo), head);
    }

    #[test]
    fn a_copy_from_an_older_newjen_is_left_to_the_user() {
        let lab = lab();
        let repo = lab.repo();
        let legacy = Worktree {
            path: lab.folder("old-copy"),
            workdir: lab.folder("old-copy"),
            source: repo.clone(),
            branch: String::new(),
            base: String::new(),
        };
        let head = lab.head(&repo);
        assert!(error_text(vcs::merge_worktree(&lab.tools, &legacy, "Add work")).contains("Rift"));
        assert_eq!(lab.head(&repo), head);
    }
}

mod revert {
    use super::*;

    #[test]
    fn a_turn_is_undone_by_putting_its_files_back_to_how_they_were() {
        let lab = lab();
        let repo = lab.repo();
        write(&repo, ".gitignore", "target/\n");
        lab.commit_all(&repo, "ignore");
        let wt = lab.worktree(&repo);
        let dir = &wt.workdir;
        let head = lab.head(&wt.path);

        write(dir, "base.txt", "turn one\n");
        std::fs::remove_file(dir.join("other.txt")).unwrap();
        write(dir, "new1.txt", "one\n");
        lab.checkpoint(dir);

        write(dir, "base.txt", "turn two\n");
        write(dir, "other.txt", "back again\n");
        write(dir, "new2.txt", "two\n");
        write(dir, "target/out.bin", "built\n");
        embedded_repo(&lab, &dir.join("newlib"));
        lab.checkpoint(dir);

        vcs::revert_turn(&lab.tools, &wt, THREAD, 2).unwrap();

        assert_eq!(read(dir, "base.txt"), "turn one\n");
        assert!(!dir.join("other.txt").exists());
        assert_eq!(read(dir, "new1.txt"), "one\n");
        assert!(!dir.join("new2.txt").exists());
        assert_eq!(read(dir, "target/out.bin"), "built\n", "ignored files stay");
        assert_eq!(
            read(dir, "newlib/lib.txt"),
            "cloned\n",
            "so do embedded repositories"
        );
        assert_eq!(lab.head(&wt.path), head);
        assert_eq!(
            lab.git(&wt.path, &["diff", "--cached"]),
            "",
            "nothing is left staged"
        );

        vcs::revert_turn(&lab.tools, &wt, THREAD, 1).unwrap();

        assert_eq!(read(dir, "base.txt"), "base\n");
        assert_eq!(read(dir, "other.txt"), "other\n");
        assert!(!dir.join("new1.txt").exists());
        assert_eq!(lab.status(&wt.path), "?? newlib/");
        assert_eq!(
            lab.status(&repo),
            "",
            "the project itself was never touched"
        );
    }

    #[test]
    fn undoing_a_turn_in_which_the_agent_committed_shows_as_an_uncommitted_undo() {
        let lab = lab();
        let repo = lab.repo();
        let wt = lab.worktree(&repo);
        let dir = &wt.workdir;

        write(dir, "base.txt", "agent edit\n");
        write(dir, "new.txt", "agent file\n");
        lab.commit_all(&wt.path, "agent commit");
        write(dir, "extra.txt", "never committed\n");
        lab.checkpoint(dir);
        let head = lab.head(&wt.path);
        assert_eq!(lab.count(&wt.path, &format!("{}..HEAD", wt.base)), 1);

        vcs::revert_turn(&lab.tools, &wt, THREAD, 1).unwrap();

        assert_eq!(read(dir, "base.txt"), "base\n");
        assert!(!dir.join("new.txt").exists());
        assert!(!dir.join("extra.txt").exists());
        assert_eq!(
            lab.head(&wt.path),
            head,
            "the agent's commit is still there"
        );
        assert_eq!(lab.status(&wt.path), " M base.txt\n D new.txt");
        assert_eq!(
            lab.files(dir, Scope::Uncommitted),
            names(&["base.txt", "new.txt"]),
            "the undo is uncommitted work the user can commit or throw away"
        );
        assert_eq!(lab.files(dir, Scope::All), Vec::<String>::new());
    }

    #[test]
    fn a_turn_that_was_never_recorded_is_refused_and_nothing_changes() {
        let lab = lab();
        let repo = lab.repo();
        let wt = lab.worktree(&repo);
        write(&wt.workdir, "agent.txt", "work\n");
        assert!(error_text(vcs::revert_turn(&lab.tools, &wt, THREAD, 1)).contains("no turn"));
        lab.checkpoint(&wt.workdir);
        for turn in [0, 2] {
            assert!(
                error_text(vcs::revert_turn(&lab.tools, &wt, THREAD, turn)).contains("no turn")
            );
        }
        assert_eq!(read(&wt.workdir, "agent.txt"), "work\n");
    }
}
