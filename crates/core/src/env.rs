//! Resolve the login shell's PATH once, without changing the process environment.
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::probe;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cli {
    Claude,
    Codex,
}

impl Cli {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
    fn override_var(self) -> &'static str {
        match self {
            Self::Claude => "NEWJEN_CLAUDE",
            Self::Codex => "NEWJEN_CODEX",
        }
    }
}

static PATH: OnceLock<OsString> = OnceLock::new();

pub fn resolved_path() -> &'static OsStr {
    PATH.get_or_init(|| {
        let marker = format!("NEWJEN_PATH_{}", uuid::Uuid::new_v4().simple());
        let shell_path = std::env::var_os("SHELL").and_then(|shell| {
            let script = format!("printf '\\n{marker}_START\\n%s\\n{marker}_END\\n' \"$PATH\"");
            let mut command = Command::new(shell);
            command.args(["-ilc", &script]);
            strip_session_env(&mut command);
            probe::output(&mut command, Instant::now() + Duration::from_secs(5))
                .ok()
                .filter(|(status, _)| status.success())
                .and_then(|(_, bytes)| parse_path(&bytes, &marker))
        });
        let path = shell_path
            .or_else(|| {
                probe::output(
                    Command::new("/bin/launchctl").args(["getenv", "PATH"]),
                    Instant::now() + Duration::from_secs(1),
                )
                .ok()
                .filter(|(status, _)| status.success())
                .and_then(|(_, bytes)| {
                    let value = String::from_utf8(bytes).ok()?;
                    (!value.trim().is_empty()).then(|| OsString::from(value.trim()))
                })
            })
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        merge_paths(&path, known_dirs())
    })
}

fn parse_path(bytes: &[u8], marker: &str) -> Option<OsString> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    lines.find(|line| *line == format!("{marker}_START"))?;
    let path = lines.next()?;
    if path.is_empty() || lines.next()? != format!("{marker}_END") {
        return None;
    }
    Some(path.into())
}

fn known_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.extend([home.join(".local/bin"), home.join(".claude/local")]);
    }
    dirs.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    dirs
}

fn merge_paths(path: &OsStr, extra: Vec<PathBuf>) -> OsString {
    let mut dirs: Vec<_> = std::env::split_paths(path).collect();
    for dir in extra {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).unwrap_or_else(|_| path.to_owned())
}

/// An explicit override is authoritative, including when it points to a broken executable.
/// Returns the bare CLI name when no installation is found, for a useful spawn error.
pub fn find_cli(cli: Cli) -> PathBuf {
    locate(
        cli,
        std::env::var_os(cli.override_var()).as_deref(),
        resolved_path(),
    )
}

fn locate(cli: Cli, override_path: Option<&OsStr>, path: &OsStr) -> PathBuf {
    if let Some(program) = override_path.filter(|path| !path.is_empty()) {
        let program = PathBuf::from(program);
        if program.components().count() > 1 {
            return program;
        }
        return std::env::split_paths(path)
            .map(|dir| dir.join(&program))
            .find(|candidate| is_executable(candidate))
            .unwrap_or(program);
    }
    std::env::split_paths(path)
        .map(|dir| dir.join(cli.name()))
        .find(|path| is_executable(path))
        .unwrap_or_else(|| cli.name().into())
}

/// Finds the executable `name`.
///
/// Order: `$<override_var>`, then the login shell's PATH, then `home_dirs` (relative to
/// `$HOME`), `/opt/homebrew/bin` and `/usr/local/bin`. The fallbacks matter because apps
/// started from Finder get a minimal `PATH` that usually does not include where the program
/// was installed. If nothing is found, returns plain `name` so the spawn error names it.
pub(crate) fn find_program(override_var: &str, name: &str, home_dirs: &[&str]) -> PathBuf {
    if let Some(path) = std::env::var_os(override_var).filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    let on_path = std::env::split_paths(resolved_path());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let in_home = home_dirs
        .iter()
        .filter_map(|dir| Some(home.as_ref()?.join(dir)));
    on_path
        .chain(in_home)
        .chain(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
        .unwrap_or_else(|| PathBuf::from(name))
}

pub(crate) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Removes the variables an agent CLI leaves in the environment when Newjen itself runs
/// inside one, so a child never looks like a nested session. Shared by sessions and the
/// terminal (which builds its command through a PTY, hence [`SESSION_ENV`] is public too).
pub fn strip_session_env(command: &mut Command) {
    for name in SESSION_ENV {
        command.env_remove(name);
    }
}

pub const SESSION_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_AGENT_SDK_CLIENT_APP",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_TOOL_CALL_ID",
    "CODEX_SANDBOX",
    "CODEX_SANDBOX_NETWORK_DISABLED",
    "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
    "CODEX_PERMISSION_PROFILE",
    "CODEX_VERSION",
    "CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS",
    "CODEX_PLUGIN_METRICS_OUTPUT",
];

pub(crate) fn configure(command: &mut Command) {
    command.env("PATH", resolved_path());
    strip_session_env(command);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn markers_ignore_shell_noise_and_reject_incomplete_output() {
        assert_eq!(
            parse_path(b"hello\nm_START\n/a:/b\nm_END\nbye", "m"),
            Some("/a:/b".into())
        );
        for text in [
            "/a:/b",
            "m_START\n/a\n",
            "m_START\n\nm_END",
            "m_START\n/a\nnoise\nm_END",
        ] {
            assert!(parse_path(text.as_bytes(), "m").is_none());
        }
    }
    #[test]
    fn shell_timeout_kills_the_process_group() {
        let start = Instant::now();
        let error = probe::output(
            Command::new("/bin/sh").args(["-c", "sleep 30 & wait"]),
            start + Duration::from_millis(60),
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn lookup_prefers_override_then_path_and_merge_keeps_order() {
        assert_eq!(
            locate(
                Cli::Codex,
                Some(OsStr::new("/missing/override")),
                OsStr::new("/bin")
            ),
            PathBuf::from("/missing/override")
        );
        assert_eq!(
            locate(Cli::Claude, None, OsStr::new("/missing")),
            PathBuf::from("claude")
        );
        assert_eq!(
            merge_paths(OsStr::new("/a:/b"), vec!["/b".into(), "/c".into()]),
            OsString::from("/a:/b:/c")
        );
    }
    #[test]
    fn finds_real_executables_in_path_order_including_bare_overrides() {
        use std::os::unix::fs::PermissionsExt;
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for dir in [&first, &second] {
            let binary = dir.path().join("codex");
            std::fs::write(&binary, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();
        assert_eq!(locate(Cli::Codex, None, &path), first.path().join("codex"));
        assert_eq!(
            locate(Cli::Codex, Some(OsStr::new("codex")), &path),
            first.path().join("codex")
        );
        assert_eq!(
            locate(
                Cli::Codex,
                Some(second.path().join("codex").as_os_str()),
                &path
            ),
            second.path().join("codex")
        );
    }

    #[test]
    fn strips_context_but_preserves_user_configuration() {
        let mut command = Command::new("claude");
        for key in [
            "CODEX_THREAD_ID",
            "CLAUDECODE",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
        ] {
            command.env(key, "test");
        }
        strip_session_env(&mut command);
        let vars: std::collections::HashMap<_, _> = command.get_envs().collect();
        assert_eq!(vars[OsStr::new("CODEX_THREAD_ID")], None);
        assert_eq!(vars[OsStr::new("CLAUDECODE")], None);
        assert_eq!(vars[OsStr::new("CODEX_HOME")], Some(OsStr::new("test")));
        assert_eq!(
            vars[OsStr::new("CLAUDE_CONFIG_DIR")],
            Some(OsStr::new("test"))
        );
    }
}
