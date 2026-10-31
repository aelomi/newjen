//! Makes and removes real Rift copies with the installed `rift` and `jj`, in temp folders.
//! Each test gets its own `HOME`, so its own Rift registry. Skipped, with the reason printed,
//! when `rift` or `jj` is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::rift::{self, Copy, CopyError, Tools};

/// A temp folder with its own `HOME`, and the tools pointed at it.
struct Lab {
    dir: tempfile::TempDir,
    tools: Tools,
}

fn lab() -> Option<Lab> {
    let mut tools = Tools::find();
    for (name, program) in [("rift", &tools.rift), ("jj", &tools.jj)] {
        if !program.is_absolute() {
            eprintln!("skipped: `{name}` is not installed");
            return None;
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    tools.env = vec![
        ("HOME".to_string(), home.display().to_string()),
        ("JJ_USER".to_string(), "Test".to_string()),
        ("JJ_EMAIL".to_string(), "test@example.com".to_string()),
    ];
    Some(Lab { dir, tools })
}

impl Lab {
    /// An empty folder `name` (canonical, as Rift reports paths).
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    /// A colocated jj repo `name` with one commit, "base", holding `a.txt`, and an empty `@`.
    fn jj_repo(&self, name: &str) -> PathBuf {
        let repo = self.folder(name);
        self.jj(&repo, &["git", "init", "--colocate"]);
        write(&repo, "a.txt", "a\n");
        self.jj(&repo, &["commit", "-m", "base"]);
        repo
    }

    fn jj(&self, cwd: &Path, args: &[&str]) -> String {
        self.run(&self.tools.jj, cwd, args)
    }

    fn rift(&self, cwd: &Path, args: &[&str]) -> String {
        self.run(&self.tools.rift, cwd, args)
    }

    fn run(&self, program: &Path, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .envs(self.tools.env.iter().map(|(name, value)| (name, value)))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{} {args:?} failed: {}",
            program.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    /// `jj log` of one revision with a template, without snapshotting.
    fn show(&self, repo: &Path, revision: &str, template: &str) -> String {
        self.jj(
            repo,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                revision,
                "-T",
                template,
            ],
        )
    }

    fn create(&self, project: &Path, name: &str) -> Copy {
        rift::create(&self.tools, project, name, |_| {}).unwrap()
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

/// Every change in the repo except the root, oldest first, as change ids.
fn changes(lab: &Lab, repo: &Path) -> Vec<String> {
    let mut ids: Vec<String> = lab
        .show(repo, "all() ~ root()", r#"change_id ++ "\n""#)
        .lines()
        .map(str::to_string)
        .collect();
    ids.reverse();
    ids
}

#[test]
fn a_plain_folder_is_copied_whole_and_edits_stay_in_the_copy() {
    let Some(lab) = lab() else { return };
    let project = lab.folder("plain");
    write(&project, "a.txt", "a\n");
    write(&project, "sub/b.txt", "b\n");

    let copy = lab.create(&project, "newjen-plain");
    assert_eq!(
        copy.path,
        project.parent().unwrap().join(".rifts/plain/newjen-plain")
    );
    assert_eq!(copy.workdir, copy.path);
    assert_eq!(copy.source, project);
    assert_eq!(copy.base, None, "not a jj repo");
    assert_eq!(read(&copy.path, ".rift").trim(), copy.rift_id);
    assert_eq!(read(&copy.path, "sub/b.txt"), "b\n");

    write(&copy.path, "a.txt", "changed in the copy\n");
    assert_eq!(read(&project, "a.txt"), "a\n");
}

#[test]
fn a_clean_jj_repo_is_left_alone_and_the_copy_starts_a_new_change_on_its_parent() {
    let Some(lab) = lab() else { return };
    let repo = lab.jj_repo("clean");
    let main_at = lab.show(&repo, "@", "commit_id");
    let parent = lab.show(&repo, "@-", "commit_id");

    let copy = lab.create(&repo, "newjen-clean");
    assert_eq!(
        lab.show(&repo, "@", "commit_id"),
        main_at,
        "no `jj new` in main"
    );
    assert_eq!(copy.base.as_deref(), Some(parent.as_str()));
    assert_eq!(lab.show(&copy.path, "@-", "commit_id"), parent);
    assert_eq!(lab.show(&copy.path, "@", "empty"), "true");
    assert_ne!(
        lab.show(&copy.path, "@", "change_id"),
        lab.show(&repo, "@", "change_id"),
        "main and the copy never share a working-copy change"
    );
}

#[test]
fn uncommitted_edits_get_one_new_change_in_main_and_land_back_without_divergence() {
    let Some(lab) = lab() else { return };
    let repo = lab.jj_repo("dirty");
    write(&repo, "wip.txt", "half done\n");
    let before = changes(&lab, &repo);

    let copy = lab.create(&repo, "newjen-dirty");
    let after = changes(&lab, &repo);
    assert_eq!(
        after.len(),
        before.len() + 1,
        "exactly one `jj new` in main"
    );
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(lab.show(&repo, "@", "empty"), "true");
    let base = copy.base.clone().expect("a jj repo");
    assert_eq!(lab.show(&repo, "@-", "commit_id"), base);
    assert_eq!(
        lab.jj(&repo, &["file", "show", "-r", &base, "wip.txt"]),
        "half done",
        "the base holds the edits"
    );
    assert_eq!(read(&copy.workdir, "wip.txt"), "half done\n");
    assert_eq!(lab.show(&copy.path, "@-", "commit_id"), base);
    assert_eq!(lab.show(&copy.path, "@", "empty"), "true");

    write(&copy.path, "agent.txt", "from the agent\n");
    lab.jj(&copy.path, &["commit", "-m", "agent work"]);
    lab.jj(&copy.path, &["bookmark", "set", "t1", "-r", "@-"]);
    write(&repo, "wip.txt", "half done\nand more\n");
    lab.jj(&repo, &["status"]);
    lab.jj(
        &repo,
        &["git", "remote", "add", "t1", copy.path.to_str().unwrap()],
    );
    lab.jj(&repo, &["git", "fetch", "--remote", "t1"]);
    assert_eq!(lab.show(&repo, "divergent()", "change_id"), "");
    assert_eq!(lab.show(&repo, "t1@t1-", "commit_id"), base);
    assert_eq!(
        lab.jj(&repo, &["file", "show", "-r", "t1@t1", "agent.txt"]),
        "from the agent"
    );
}

#[test]
fn a_project_in_a_subfolder_works_in_the_same_subfolder_of_the_copy() {
    let Some(lab) = lab() else { return };
    let repo = lab.jj_repo("mono");
    write(&repo, "apps/web/page.txt", "page\n");
    let project = repo.join("apps/web");

    let copy = lab.create(&project, "newjen-sub");
    assert_eq!(copy.source, repo, "the whole repo is copied");
    assert_eq!(copy.workdir, copy.path.join("apps/web"));
    assert_eq!(read(&copy.workdir, "page.txt"), "page\n");
}

#[test]
fn gitignored_files_are_copied_too() {
    let Some(lab) = lab() else { return };
    let repo = lab.jj_repo("ignored");
    write(&repo, ".gitignore", "target/\nnode_modules/\n");
    write(&repo, "target/debug/app", "binary\n");
    write(&repo, "node_modules/left-pad/index.js", "module\n");

    let copy = lab.create(&repo, "newjen-ignored");
    assert_eq!(read(&copy.path, "target/debug/app"), "binary\n");
    assert_eq!(
        read(&copy.path, "node_modules/left-pad/index.js"),
        "module\n"
    );
}

#[test]
fn a_failing_hook_leaves_no_copy_and_its_output_is_in_the_error() {
    let Some(lab) = lab() else { return };
    let project = lab.folder("hooked");
    write(
        &project,
        ".rift.toml",
        "version = 1\n\n[[hooks.postcreate]]\nrun = \"echo installing deps; exit 3\"\n",
    );

    let mut seen = Vec::new();
    let error = rift::create(&lab.tools, &project, "newjen-hook", |line| {
        seen.push(line.to_string())
    })
    .unwrap_err();
    let CopyError::Failed { detail, output, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(detail.contains("postcreate hook failed"), "{detail}");
    assert!(
        output.iter().any(|line| line == "installing deps"),
        "{output:?}"
    );
    assert!(
        seen.iter().any(|line| line == "installing deps"),
        "{seen:?}"
    );

    let copies = project.parent().unwrap().join(".rifts/hooked");
    assert!(!copies.join("newjen-hook").exists());
    assert_eq!(lab.rift(&project, &["list", project.to_str().unwrap()]), "");
    let trash = std::fs::read_dir(copies.join(".trash")).map_or(0, |entries| entries.count());
    assert_eq!(trash, 0, "its trash entry is deleted too");
}

#[test]
fn hooks_see_the_copys_rift_id_and_source() {
    let Some(lab) = lab() else { return };
    let project = lab.folder("env");
    write(
        &project,
        ".rift.toml",
        "version = 1\n\n[[hooks.postcreate]]\nrun = 'printf \"%s %s\" \"$RIFT_ID\" \"$RIFT_SOURCE\" > rift-env.txt'\n",
    );

    let copy = lab.create(&project, "newjen-env");
    assert_eq!(
        read(&copy.path, "rift-env.txt"),
        format!("{} {}", copy.rift_id, copy.source.display())
    );
    assert_eq!(
        copy.env(),
        [
            ("RIFT_ID".to_string(), copy.rift_id.clone()),
            ("RIFT_SOURCE".to_string(), copy.source.display().to_string()),
        ]
    );
}

#[test]
fn removing_a_copy_deletes_only_its_own_trash_entry() {
    let Some(lab) = lab() else { return };
    let project = lab.folder("many");
    write(&project, "a.txt", "a\n");
    let gone = lab.create(&project, "newjen-gone");
    let kept = lab.create(&project, "newjen-kept");
    let other = lab.create(&project, "other");
    lab.rift(&project, &["remove", other.path.to_str().unwrap()]);
    let trash = project.parent().unwrap().join(".rifts/many/.trash");
    let others_entry = trash.join(format!("{}-other", other.rift_id));
    assert!(
        others_entry.exists(),
        "removed outside Newjen, still in the trash"
    );

    rift::remove(&lab.tools, &gone.path, &gone.rift_id).unwrap();
    assert!(!gone.path.exists());
    assert!(!trash.join(format!("{}-newjen-gone", gone.rift_id)).exists());
    assert!(others_entry.exists(), "another copy's trash is left alone");
    assert_eq!(read(&kept.path, "a.txt"), "a\n");
    assert_eq!(
        lab.rift(&project, &["list", project.to_str().unwrap()]),
        kept.path.display().to_string()
    );

    rift::remove(&lab.tools, &gone.path, &gone.rift_id).unwrap();
}

#[test]
fn a_copy_already_at_the_path_is_reported_with_its_id() {
    let Some(lab) = lab() else { return };
    let project = lab.folder("twice");
    let first = lab.create(&project, "newjen-twice");
    let again = rift::create(&lab.tools, &project, "newjen-twice", |_| {}).unwrap_err();
    assert_eq!(
        again,
        CopyError::Exists {
            path: first.path.clone(),
            rift_id: Some(first.rift_id.clone()),
        }
    );
    assert!(first.path.exists(), "the existing copy is untouched");
}

#[test]
fn review_remove_replacement() {
    let lab = lab().expect("real tools required");
    let project = lab.jj_repo("replacement");
    let old = lab.create(&project, "same-name");
    rift::remove(&lab.tools, &old.path, &old.rift_id).unwrap();
    let replacement = lab.create(&project, "same-name");
    write(&replacement.path, "valuable.txt", "replacement's work");
    let result = rift::remove(&lab.tools, &old.path, &old.rift_id);
    println!(
        "stale removal: {result:?}; replacement exists: {}",
        replacement.path.exists()
    );
    assert!(
        replacement.path.exists(),
        "stale removal removed a different Rift id"
    );
}

#[test]
fn review_symlink_trash() {
    let lab = lab().expect("real tools required");
    let parent = lab.folder("copies");
    let unrelated = lab.folder("unrelated");
    write(
        &unrelated,
        "abc123-newjen-thread/work.txt",
        "unrelated work",
    );
    std::os::unix::fs::symlink(&unrelated, parent.join(".trash")).unwrap();
    let result = rift::remove(&lab.tools, &parent.join("newjen-thread"), "abc123");
    println!(
        "removal: {result:?}; unrelated work exists: {}",
        unrelated.join("abc123-newjen-thread/work.txt").exists()
    );
    assert!(
        unrelated.join("abc123-newjen-thread/work.txt").exists(),
        "cleanup followed .trash symlink"
    );
}

#[test]
fn review_retry_unowned_copy() {
    use newjen_core::{
        session::SessionConfig,
        store::Store,
        thread::{Sent, Thread, WorksIn},
    };
    let lab = lab().expect("real tools required");
    let project = lab.jj_repo("retry");
    let store = Store::open_in(&lab.folder("db")).unwrap();
    let project = store.open_project(&project).unwrap();
    let mut thread = Thread::new_draft(store, &project, SessionConfig::new(&project.path));
    thread.set_works_in(WorksIn::RiftCopy).unwrap();
    let name = format!("newjen-{}", &thread.id()[..8]);
    let unrelated = lab.create(&project.path, &name);
    write(&unrelated.path, "valuable.txt", "external copy work");
    let Sent::MakingCopy(job) = thread.send("hello").unwrap() else {
        panic!()
    };
    let result = job.run(&lab.tools, |_| {});
    println!(
        "job succeeded: {}; external work exists: {}",
        result.is_ok(),
        unrelated.path.join("valuable.txt").exists()
    );
    assert!(
        unrelated.path.join("valuable.txt").exists(),
        "retry destroyed a copy without ownership proof"
    );
}
