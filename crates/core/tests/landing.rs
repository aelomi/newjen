use std::path::{Path, PathBuf};
use std::process::Command;

use newjen_core::landing::{self, Integration, LandingMode};
use newjen_core::rift::{self, Tools};

const THREAD: &str = "6444bd65-a528-420c-b1d7-f18b0287b755";

struct Lab {
    dir: tempfile::TempDir,
    tools: Tools,
}

impl Lab {
    fn new() -> Option<Self> {
        let mut tools = Tools::find();
        if !tools.jj.is_absolute() || !tools.rift.is_absolute() {
            eprintln!("skipped landing test because real jj or Rift is unavailable");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir(&home).unwrap();
        tools.env = vec![
            ("HOME".to_string(), home.display().to_string()),
            ("JJ_USER".to_string(), "Test".to_string()),
            ("JJ_EMAIL".to_string(), "test@example.com".to_string()),
        ];
        Some(Self { dir, tools })
    }

    fn repo(&self) -> PathBuf {
        let repo = self.dir.path().join("project");
        std::fs::create_dir(&repo).unwrap();
        let repo = repo.canonicalize().unwrap();
        self.jj(&repo, &["git", "init", "--colocate"]);
        std::fs::write(repo.join("base.txt"), "base\n").unwrap();
        self.jj(&repo, &["commit", "-m", "base"]);
        repo
    }

    fn jj(&self, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new(&self.tools.jj)
            .args(args)
            .current_dir(cwd)
            .envs(self.tools.env.iter().map(|(key, value)| (key, value)))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn snap(&self, cwd: &Path) {
        landing::snapshot(
            &self.tools,
            cwd,
            LandingMode::HarnessManaged,
            "Update work",
            THREAD,
        )
        .unwrap();
    }
}

#[test]
fn project_review_land_and_discard_keep_the_preexisting_files() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    std::fs::write(repo.join("owner.txt"), "owner work\n").unwrap();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("agent.txt"), "agent work\n").unwrap();
    lab.snap(&repo);
    let reviewed = landing::review(&lab.tools, &repo, &base).unwrap();
    assert!(reviewed.patch.contains("agent.txt"));
    assert!(!reviewed.patch.contains("owner.txt"));
    landing::discard_project(
        &lab.tools,
        &repo,
        LandingMode::HarnessManaged,
        THREAD,
        &reviewed,
    )
    .unwrap();
    assert!(!repo.join("agent.txt").exists());
    assert_eq!(
        std::fs::read_to_string(repo.join("owner.txt")).unwrap(),
        "owner work\n"
    );
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("landed.txt"), "landed\n").unwrap();
    lab.snap(&repo);
    let reviewed = landing::review(&lab.tools, &repo, &base).unwrap();
    landing::land_project(
        &lab.tools,
        &repo,
        LandingMode::HarnessManaged,
        THREAD,
        &reviewed,
    )
    .unwrap();
    assert!(repo.join("landed.txt").exists());
    assert!(
        !lab.jj(&repo, &["bookmark", "list", "--ignore-working-copy"])
            .is_empty()
    );
}

#[test]
fn review_is_read_only_and_a_stale_review_cannot_discard() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("agent.txt"), "first\n").unwrap();
    lab.snap(&repo);
    let first = landing::review(&lab.tools, &repo, &base).unwrap();
    std::fs::write(repo.join("agent.txt"), "second\n").unwrap();
    let second = landing::review(&lab.tools, &repo, &base).unwrap();
    assert_eq!(
        first, second,
        "UI reads leave the live working copy unsnapshotted"
    );
    let error = landing::discard_project(
        &lab.tools,
        &repo,
        LandingMode::HarnessManaged,
        THREAD,
        &first,
    )
    .unwrap_err();
    assert!(error.to_string().contains("changed since"));
    assert_eq!(
        std::fs::read_to_string(repo.join("agent.txt")).unwrap(),
        "second\n"
    );
}

#[test]
fn another_change_cannot_be_discarded_as_thread_work() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("foreign.txt"), "not owned\n").unwrap();
    lab.jj(&repo, &["commit", "-m", "external commit"]);
    let reviewed = landing::review(&lab.tools, &repo, &base).unwrap();
    assert!(
        landing::discard_project(
            &lab.tools,
            &repo,
            LandingMode::HarnessManaged,
            THREAD,
            &reviewed
        )
        .is_err()
    );
    assert!(repo.join("foreign.txt").exists());
}

#[test]
fn real_rift_work_lands_by_rebase_and_merge_without_divergence() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    for (name, integration) in [
        ("rebase", Integration::Rebase),
        ("merge", Integration::Merge),
    ] {
        let copy = rift::create(&lab.tools, &repo, name, |_| {}).unwrap();
        std::fs::write(copy.workdir.join(format!("{name}.txt")), "agent work\n").unwrap();
        lab.snap(&copy.workdir);
        std::fs::write(repo.join("owner.txt"), format!("owner {name}\n")).unwrap();
        let reviewed =
            landing::review(&lab.tools, &copy.path, copy.base.as_deref().unwrap()).unwrap();
        landing::land_copy(
            &lab.tools,
            &copy,
            LandingMode::HarnessManaged,
            THREAD,
            integration,
            &reviewed,
        )
        .unwrap();
        let landed = landing::head(&lab.tools, &repo).unwrap();
        landing::land_copy(
            &lab.tools,
            &copy,
            LandingMode::HarnessManaged,
            THREAD,
            integration,
            &reviewed,
        )
        .unwrap();
        assert_eq!(
            landing::head(&lab.tools, &repo).unwrap(),
            landed,
            "retry does not land a second copy"
        );
        assert!(repo.join(format!("{name}.txt")).exists());
        assert_eq!(
            std::fs::read_to_string(repo.join("owner.txt")).unwrap(),
            format!("owner {name}\n")
        );
        assert_eq!(
            lab.jj(
                &repo,
                &[
                    "log",
                    "--ignore-working-copy",
                    "--no-graph",
                    "-r",
                    "divergent()",
                    "-T",
                    "commit_id"
                ]
            ),
            ""
        );
        landing::cleanup_remote(&lab.tools, &repo, THREAD).unwrap();
        landing::cleanup_remote(&lab.tools, &repo, THREAD).unwrap();
        rift::remove(&lab.tools, &copy.source, &copy.path, &copy.rift_id).unwrap();
    }
}

#[test]
fn conflicts_leave_main_files_and_working_change_untouched() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    let copy = rift::create(&lab.tools, &repo, "conflict", |_| {}).unwrap();
    std::fs::write(copy.path.join("base.txt"), "agent edit\n").unwrap();
    lab.snap(&copy.path);
    std::fs::write(repo.join("base.txt"), "owner edit\n").unwrap();
    lab.jj(&repo, &["status"]);
    let main = landing::head(&lab.tools, &repo).unwrap();
    let reviewed = landing::review(&lab.tools, &copy.path, copy.base.as_deref().unwrap()).unwrap();
    for integration in [Integration::Rebase, Integration::Merge] {
        let error = landing::land_copy(
            &lab.tools,
            &copy,
            LandingMode::HarnessManaged,
            THREAD,
            integration,
            &reviewed,
        )
        .unwrap_err();
        assert!(error.to_string().contains("conflict"), "{error:#}");
        assert_eq!(landing::head(&lab.tools, &repo).unwrap(), main);
        assert_eq!(
            std::fs::read_to_string(repo.join("base.txt")).unwrap(),
            "owner edit\n"
        );
    }
    rift::remove(&lab.tools, &copy.source, &copy.path, &copy.rift_id).unwrap();
}

#[test]
fn agent_managed_snapshots_do_nothing_and_only_bookmarked_work_lands() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    let copy = rift::create(&lab.tools, &repo, "agent", |_| {}).unwrap();
    std::fs::write(copy.path.join("agent.txt"), "agent\n").unwrap();
    let before = landing::head(&lab.tools, &copy.path).unwrap();
    landing::snapshot(
        &lab.tools,
        &copy.path,
        LandingMode::AgentManaged,
        "unused",
        THREAD,
    )
    .unwrap();
    assert_eq!(landing::head(&lab.tools, &copy.path).unwrap(), before);
    lab.jj(&copy.path, &["commit", "-m", "agent work"]);
    let base = copy.base.as_deref().unwrap();
    let reviewed = landing::review(&lab.tools, &copy.path, base).unwrap();
    assert!(
        landing::land_copy(
            &lab.tools,
            &copy,
            LandingMode::AgentManaged,
            THREAD,
            Integration::Rebase,
            &reviewed
        )
        .is_err()
    );
    lab.jj(&copy.path, &["bookmark", "set", "agent/work", "-r", "@-"]);
    let op = lab.jj(
        &copy.path,
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
    );
    let reviewed = landing::review(&lab.tools, &copy.path, base).unwrap();
    landing::land_copy(
        &lab.tools,
        &copy,
        LandingMode::AgentManaged,
        THREAD,
        Integration::Rebase,
        &reviewed,
    )
    .unwrap();
    assert!(repo.join("agent.txt").exists());
    assert_eq!(
        lab.jj(
            &copy.path,
            &[
                "op",
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "--limit",
                "1",
                "-T",
                "id"
            ]
        ),
        op
    );
    rift::remove(&lab.tools, &copy.source, &copy.path, &copy.rift_id).unwrap();
}

#[test]
fn checks_run_before_landing_and_cannot_change_the_reviewed_work() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    std::fs::create_dir(repo.join(".newjen")).unwrap();
    std::fs::write(repo.join(".newjen/checks.json"), r#"[["/usr/bin/false"]]"#).unwrap();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("agent.txt"), "agent\n").unwrap();
    lab.snap(&repo);
    let reviewed = landing::review(&lab.tools, &repo, &base).unwrap();
    let error = landing::land_project(
        &lab.tools,
        &repo,
        LandingMode::HarnessManaged,
        THREAD,
        &reviewed,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Landing check"));
    assert!(repo.join("agent.txt").exists());
}

#[test]
fn agent_bookmarks_cannot_move_an_existing_project_bookmark() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    lab.jj(&repo, &["bookmark", "set", "master", "-r", "@-"]);
    let master = lab.jj(
        &repo,
        &[
            "log",
            "--ignore-working-copy",
            "--no-graph",
            "-r",
            "master",
            "-T",
            "commit_id",
        ],
    );
    let copy = rift::create(&lab.tools, &repo, "collision", |_| {}).unwrap();
    std::fs::write(copy.path.join("agent.txt"), "agent\n").unwrap();
    lab.jj(&copy.path, &["commit", "-m", "agent work"]);
    lab.jj(&copy.path, &["bookmark", "set", "master", "-r", "@-"]);
    let reviewed = landing::review(&lab.tools, &copy.path, copy.base.as_deref().unwrap()).unwrap();
    let error = landing::land_copy(
        &lab.tools,
        &copy,
        LandingMode::AgentManaged,
        THREAD,
        Integration::Rebase,
        &reviewed,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("conflicts with an existing"),
        "{error:#}"
    );
    assert_eq!(
        lab.jj(
            &repo,
            &[
                "log",
                "--ignore-working-copy",
                "--no-graph",
                "-r",
                "master",
                "-T",
                "commit_id"
            ]
        ),
        master
    );
    assert!(!repo.join("agent.txt").exists());
    rift::remove(&lab.tools, &copy.source, &copy.path, &copy.rift_id).unwrap();
}

#[test]
fn oversized_reviews_and_checks_that_edit_files_require_another_review() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    std::fs::create_dir(repo.join(".newjen")).unwrap();
    std::fs::write(
        repo.join(".newjen/checks.json"),
        r#"[["/usr/bin/touch","generated.txt"]]"#,
    )
    .unwrap();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("agent.txt"), "agent\n").unwrap();
    lab.snap(&repo);
    let reviewed = landing::review(&lab.tools, &repo, &base).unwrap();
    let error = landing::land_project(
        &lab.tools,
        &repo,
        LandingMode::HarnessManaged,
        THREAD,
        &reviewed,
    )
    .unwrap_err();
    assert!(error.to_string().contains("changed since"), "{error:#}");
    assert!(repo.join("generated.txt").exists());
    std::fs::write(repo.join("large.txt"), "x\n".repeat(60_000)).unwrap();
    lab.snap(&repo);
    let reviewed = landing::review(&lab.tools, &repo, &base).unwrap();
    assert!(reviewed.truncated);
    assert!(reviewed.patch.len() <= landing::MAX_PATCH_BYTES);
    assert!(
        landing::land_project(
            &lab.tools,
            &repo,
            LandingMode::HarnessManaged,
            THREAD,
            &reviewed
        )
        .unwrap_err()
        .to_string()
        .contains("too large")
    );
}

#[test]
fn explicit_agent_managed_project_discard_requires_a_current_recorded_review() {
    let Some(lab) = Lab::new() else { return };
    let repo = lab.repo();
    std::fs::write(repo.join("owner.txt"), "owner work\n").unwrap();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("agent.txt"), "agent\n").unwrap();
    let old = landing::review(&lab.tools, &repo, &base).unwrap();
    assert!(
        landing::discard_project(&lab.tools, &repo, LandingMode::AgentManaged, THREAD, &old)
            .is_err()
    );
    assert!(repo.join("agent.txt").exists());
    let current = landing::review(&lab.tools, &repo, &base).unwrap();
    assert!(current.patch.contains("agent.txt"));
    landing::discard_project(
        &lab.tools,
        &repo,
        LandingMode::AgentManaged,
        THREAD,
        &current,
    )
    .unwrap();
    assert!(!repo.join("agent.txt").exists());
    assert_eq!(
        std::fs::read_to_string(repo.join("owner.txt")).unwrap(),
        "owner work\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("base.txt")).unwrap(),
        "base\n"
    );
}

#[test]
fn a_project_edit_after_preflight_is_preserved_by_the_native_jj_transaction() {
    use std::os::unix::fs::PermissionsExt;
    for integration in [Integration::Rebase, Integration::Merge] {
        let Some(lab) = Lab::new() else {
            return;
        };
        let repo = lab.repo();
        std::fs::write(repo.join("shared"), "base\n").unwrap();
        lab.jj(&repo, &["commit", "-m", "shared base"]);
        let copy = rift::create(&lab.tools, &repo, "concurrent", |_| {}).unwrap();
        std::fs::write(copy.path.join("shared"), "agent\n").unwrap();
        lab.snap(&copy.path);
        let reviewed =
            landing::review(&lab.tools, &copy.path, copy.base.as_deref().unwrap()).unwrap();
        let counter = lab.dir.path().join("counter");
        let wrapper = lab.dir.path().join("jj-wrapper");
        let quoted = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
        let code = format!(
            r#"#!/usr/bin/python3
import os,sys,subprocess,pathlib
real={real}
root=pathlib.Path({root}).resolve()
copy=pathlib.Path({copy}).resolve()
count=pathlib.Path({count})
args=sys.argv[1:]
if pathlib.Path.cwd()==root and 'op' in args and 'log' in args:
 n=int(count.read_text())+1 if count.exists() else 1
 count.write_text(str(n))
if pathlib.Path.cwd()==copy and count.exists() and count.read_text()=='2':
 count.write_text('injected')
 (root/'shared').write_text('owner concurrent edit\n')
 subprocess.run([real,'status'],cwd=root,check=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
os.execv(real,[real]+args)
"#,
            real = quoted(&lab.tools.jj),
            root = quoted(&repo),
            copy = quoted(&copy.path),
            count = quoted(&counter)
        );
        std::fs::write(&wrapper, code).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut tools = lab.tools.clone();
        tools.jj = wrapper;
        let result = landing::land_copy(
            &tools,
            &copy,
            LandingMode::HarnessManaged,
            THREAD,
            integration,
            &reviewed,
        );
        assert_eq!(std::fs::read_to_string(counter).unwrap(), "injected");
        assert!(result.is_err(), "a concurrent conflict must be reported");
        let content = std::fs::read_to_string(repo.join("shared")).unwrap();
        assert!(content.contains("owner concurrent edit"), "{content}");
        assert!(content.contains("agent"), "{content}");
        rift::remove(&lab.tools, &copy.source, &copy.path, &copy.rift_id).unwrap();
    }
}

#[test]
fn project_landing_refuses_conflicts_without_resolving_the_thread() {
    let Some(lab) = Lab::new() else {
        return;
    };
    let repo = lab.repo();
    let base = landing::prepare_project(&lab.tools, &repo, THREAD).unwrap();
    std::fs::write(repo.join("base.txt"), "first\n").unwrap();
    let first = lab.jj(&repo, &["log", "--no-graph", "-r", "@", "-T", "commit_id"]);
    lab.jj(&repo, &["new", &base]);
    std::fs::write(repo.join("base.txt"), "second\n").unwrap();
    let second = lab.jj(&repo, &["log", "--no-graph", "-r", "@", "-T", "commit_id"]);
    lab.jj(&repo, &["new", &first, &second]);
    let review = landing::review(&lab.tools, &repo, &base).unwrap();
    let before = landing::head(&lab.tools, &repo).unwrap();
    let error = landing::land_project(
        &lab.tools,
        &repo,
        LandingMode::AgentManaged,
        THREAD,
        &review,
    )
    .unwrap_err();
    assert!(error.to_string().contains("conflict"), "{error}");
    assert_eq!(landing::head(&lab.tools, &repo).unwrap(), before);
    assert!(
        lab.jj(&repo, &["bookmark", "list", "--ignore-working-copy"])
            .is_empty()
    );
}

#[test]
fn unsupported_repositories_decline_landing_without_preparing_a_change() {
    let Some(lab) = Lab::new() else {
        return;
    };
    let repo = lab.dir.path().join("noncolocated");
    std::fs::create_dir(&repo).unwrap();
    lab.jj(&repo, &["git", "init", "--no-colocate"]);
    let before = landing::head(&lab.tools, &repo).unwrap();
    assert!(landing::prepare_project(&lab.tools, &repo, THREAD).is_err());
    assert_eq!(landing::head(&lab.tools, &repo).unwrap(), before);
    assert!(landing::review(&lab.tools, &repo, &before).is_err());
}
