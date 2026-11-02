use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use newjen_core::commit_message;

const PATCH: &str = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-a\n+b\n";

struct Fake {
    dir: tempfile::TempDir,
    claude: PathBuf,
    project: PathBuf,
}

impl Fake {
    fn new(body: &str) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        let claude = root.join("claude");
        let record = root.display();
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\nprintf '%s\\0' \"$@\" > '{record}/args'\npwd -P > '{record}/cwd'\n/bin/cat > '{record}/stdin'\n{body}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake {
            dir,
            claude,
            project,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("args"))
            .unwrap()
            .split_terminator('\0')
            .map(str::to_string)
            .collect()
    }

    fn generate(&self, patch: &str) -> anyhow::Result<String> {
        commit_message::generate(&self.claude, &self.project, patch)
    }
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == flag)?;
    args.get(at + 1).map(String::as_str)
}

#[test]
fn the_message_comes_from_a_small_model_with_no_tools_and_the_patch_on_stdin() {
    let fake = Fake::new(
        "printf '```text\\nfeat(core): add checkpoints\\n\\nKeep a private ref per turn.\\n```\\n'",
    );

    let message = fake.generate(PATCH).unwrap();

    assert_eq!(
        message,
        "feat(core): add checkpoints\n\nKeep a private ref per turn."
    );
    let args = fake.args();
    assert!(args.contains(&"-p".to_string()), "{args:?}");
    assert_eq!(value_after(&args, "--model"), Some("haiku"));
    assert_eq!(value_after(&args, "--tools"), Some(""), "no tools at all");
    assert!(
        args.contains(&"--no-session-persistence".to_string()),
        "{args:?}"
    );
    assert!(
        args.last().unwrap().contains("Conventional Commits"),
        "the prompt is the last argument"
    );
    assert_eq!(std::fs::read_to_string(fake.path("stdin")).unwrap(), PATCH);
    assert_eq!(
        std::fs::read_to_string(fake.path("cwd")).unwrap().trim(),
        fake.project.to_str().unwrap(),
        "claude runs in the project"
    );
}

#[test]
fn a_plain_reply_is_returned_trimmed() {
    let fake = Fake::new("printf '\\n  fix: keep stdin open  \\n\\n'");
    assert_eq!(fake.generate(PATCH).unwrap(), "fix: keep stdin open");
}

#[test]
fn only_the_first_fifty_thousand_bytes_of_a_huge_patch_are_sent() {
    let fake = Fake::new("echo 'feat: big change'");
    let patch = format!("{PATCH}{}", "+a line of added text\n".repeat(10_000));
    assert!(patch.len() > 200_000);

    assert_eq!(fake.generate(&patch).unwrap(), "feat: big change");

    let sent = std::fs::read(fake.path("stdin")).unwrap();
    assert_eq!(sent.len(), 50_000);
    assert!(patch.as_bytes().starts_with(&sent));
}

#[test]
fn an_empty_patch_is_refused_without_starting_claude() {
    let fake = Fake::new("echo 'feat: never'");
    for patch in ["", "  \n\t\n"] {
        let error = fake.generate(patch).unwrap_err();
        assert!(error.to_string().contains("nothing to commit"), "{error}");
    }
    assert!(!fake.path("args").exists(), "claude was not started");
}

#[test]
fn a_failing_claude_reports_what_it_said() {
    let fake = Fake::new("echo 'Please run /login' >&2\nexit 1");
    let error = format!("{:#}", fake.generate(PATCH).unwrap_err());
    assert!(
        error.contains("could not write the commit message"),
        "{error}"
    );
    assert!(error.contains("Please run /login"), "{error}");
}

#[test]
fn an_empty_or_fence_only_reply_is_an_error() {
    for body in ["true", "printf '   \\n'", "printf '```\\n\\n```\\n'"] {
        let fake = Fake::new(body);
        let error = fake.generate(PATCH).unwrap_err();
        assert!(error.to_string().contains("empty"), "{body}: {error}");
    }
}

#[test]
fn a_claude_that_cannot_be_started_is_an_error() {
    let fake = Fake::new("true");
    let missing = fake.path("no-claude-here");
    let error = commit_message::generate(&missing, Path::new(&fake.project), PATCH).unwrap_err();
    assert!(
        format!("{error:#}").contains("Cannot start claude"),
        "{error:#}"
    );
}
