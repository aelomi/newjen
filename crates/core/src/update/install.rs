//! The swap itself. A running app cannot replace its own bundle, so a small detached shell
//! script waits for Newjen to exit, swaps the folders, writes down how it went, and relaunches
//! if asked.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use semver::Version;

use super::UpdateError;
use super::location::InstallTarget;
use super::stage::StagedUpdate;

/// What the script's `PATH` is outside of tests: the system tools only.
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// How long the script waits for another Newjen running from the same bundle to quit before it
/// gives up and leaves the old version in place.
const PEER_WAIT_SECONDS: u32 = 60;

/// Arguments, in order: the pid to wait for, the staged bundle, the installed bundle, where the
/// installed one waits meanwhile, where the staged one waits next to it, the stage folder, the
/// result file, the pending marker, the version, `1` to relaunch, how many seconds to wait for
/// other Newjens, then the project folders to reopen.
///
/// The swap is two sibling renames: the staged bundle moves next to the app, the app moves
/// aside, and the new one takes its place. The old bundle is only deleted once the new one is in
/// place. If anything fails before that, the old bundle is put back and the app is still
/// relaunched when asked, so a failed update leaves the user with the version they had. Either
/// way the script writes the result file before it relaunches, so the next launch can say what
/// happened.
pub(super) const INSTALL_SCRIPT: &str = r#"
trap '' HUP
pid=$1 staged=$2 app=$3 old=$4 new=$5 stage=$6 result=$7 pending=$8 version=$9
relaunch=${10} peerwait=${11}
shift 11
status=failed reason="" relaunching=$relaunch
peers() {
    ps -axww -o pid=,command= | while read -r other command; do
        case "$command" in
            "$app/Contents/MacOS/"*) [ "$other" != "$$" ] && echo "$other" ;;
        esac
    done
}
clear_path() {
    [ -e "$1" ] || return 0
    rm -rf "$1"
    [ -e "$1" ] && mv "$1" "$1.$$"
    [ ! -e "$1" ]
}
attempt() {
    waited=0
    while kill -0 "$pid" 2>/dev/null; do
        waited=$((waited + 1))
        if [ "$waited" -gt 1500 ]; then
            reason="Newjen didn't quit in time."
            relaunching=0
            return
        fi
        sleep 0.2
    done
    waited=0
    while [ -n "$(peers)" ]; do
        waited=$((waited + 1))
        if [ "$waited" -gt $((peerwait * 5)) ]; then
            reason="Another Newjen is still running from this app. Quit it and update again."
            return
        fi
        sleep 0.2
    done
    [ -d "$staged" ] || { reason="The downloaded update was gone."; return; }
    clear_path "$old" && clear_path "$new" || { reason="Couldn't clear the way for the update."; return; }
    mv "$staged" "$new" || { reason="Couldn't move the update next to Newjen."; return; }
    mv "$app" "$old" || { reason="Couldn't move the current Newjen aside."; rm -rf "$new"; return; }
    if mv "$new" "$app"; then
        status=ok
        rm -rf "$old" "$stage"
        return
    fi
    reason="Couldn't put the new version in place."
    mv "$old" "$app" || reason="$reason The previous version is saved at $old."
    rm -rf "$new" "$stage"
}
attempt
printf 'version=%s\nstatus=%s\nreason=%s\n' "$version" "$status" "$reason" > "$result.tmp" \
    && mv "$result.tmp" "$result"
rm -f "$pending"
if [ "$relaunching" = 1 ]; then
    if [ "$#" -gt 0 ]; then open -n "$app" --args "$@"; else open -n "$app"; fi
fi
[ "$status" = ok ]
"#;

/// How an install is run. [`install`] uses the defaults; tests choose their own.
#[derive(Debug, Clone)]
pub struct Install {
    /// The process the script waits for, normally Newjen itself.
    pub wait_for: u32,
    pub relaunch: bool,
    /// The folders to reopen after a relaunch.
    pub projects: Vec<PathBuf>,
    pub path: String,
    pub peer_wait_seconds: u32,
}

impl Install {
    pub fn new(wait_for: u32, relaunch: bool) -> Self {
        Install {
            wait_for,
            relaunch,
            projects: Vec::new(),
            path: SYSTEM_PATH.into(),
            peer_wait_seconds: PEER_WAIT_SECONDS,
        }
    }
}

/// Hands the swap to a detached script and returns at once. Call it as the last step before
/// the app exits: the script waits for this process to end, then replaces the bundle. With
/// `relaunch` the new version opens afterwards, with `projects` as its arguments. Without it
/// the new version is simply there at the next launch ("install when I quit").
pub fn install(
    staged: &StagedUpdate,
    relaunch: bool,
    projects: &[PathBuf],
) -> Result<(), UpdateError> {
    let mut plan = Install::new(std::process::id(), relaunch);
    plan.projects = projects.to_vec();
    spawn_install(staged, &plan)
        .map(|_| ())
        .map_err(|error| UpdateError::Other(format!("Couldn't start the installer: {error}")))
}

/// [`install`] with a chosen plan, returning the script's process. For tests, which stand in a
/// short-lived process for Newjen. A marker with the script's pid tells launches in the
/// meantime to leave the staged files alone.
pub fn spawn_install(staged: &StagedUpdate, plan: &Install) -> std::io::Result<Child> {
    let target = &staged.target;
    let (version, pid) = (&staged.version, plan.wait_for);
    let child = Command::new("/bin/sh")
        .arg("-c")
        .arg(INSTALL_SCRIPT)
        .arg("newjen-install")
        .arg(pid.to_string())
        .args([
            staged.staged_app.as_path(),
            target.app(),
            target.old_path(version, pid).as_path(),
            target.new_path(version, pid).as_path(),
            staged.stage_dir.as_path(),
            target.result_path().as_path(),
            target.pending_path().as_path(),
        ])
        .arg(version.to_string())
        .arg(if plan.relaunch { "1" } else { "0" })
        .arg(plan.peer_wait_seconds.to_string())
        .args(&plan.projects)
        .env("PATH", &plan.path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    let _ = std::fs::write(target.pending_path(), child.id().to_string());
    Ok(child)
}

/// How the last swap ended, as the script wrote it down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub version: Version,
    pub status: InstallStatus,
}

impl InstallOutcome {
    pub fn into_failure(self) -> Option<InstallFailure> {
        match self.status {
            InstallStatus::Installed => None,
            InstallStatus::Failed(reason) => Some(InstallFailure {
                version: self.version,
                reason,
            }),
        }
    }
}

/// An install that did not finish: the version that was meant to go in, and why it didn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallFailure {
    pub version: Version,
    pub reason: String,
}

impl std::fmt::Display for InstallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Couldn't install {}: {}", self.version, self.reason)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStatus {
    Installed,
    /// A sentence saying why, such as "Couldn't put the new version in place."
    Failed(String),
}

/// Reads the script's result file text. `None` when it isn't one.
pub fn parse_result(text: &str) -> Option<InstallOutcome> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
    };
    let version = field("version")?.parse().ok()?;
    let status = match field("status")? {
        "ok" => InstallStatus::Installed,
        "failed" => InstallStatus::Failed(
            field("reason")
                .filter(|reason| !reason.is_empty())
                .unwrap_or("The install didn't finish.")
                .to_owned(),
        ),
        _ => return None,
    };
    Some(InstallOutcome { version, status })
}

/// Reads and removes the result file, so each outcome is reported once. A file that can't be
/// parsed is removed too.
pub fn take_install_result(target: &InstallTarget) -> Option<InstallOutcome> {
    let path = target.result_path();
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    parse_result(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_parse_and_garbage_does_not() {
        assert_eq!(
            parse_result("version=0.2.0\nstatus=ok\nreason=\n"),
            Some(InstallOutcome {
                version: Version::new(0, 2, 0),
                status: InstallStatus::Installed
            })
        );
        assert_eq!(
            parse_result("version=0.2.0-alpha.1\nstatus=failed\nreason=Couldn't move it.\n"),
            Some(InstallOutcome {
                version: "0.2.0-alpha.1".parse().unwrap(),
                status: InstallStatus::Failed("Couldn't move it.".into())
            })
        );
        assert_eq!(
            parse_result("version=0.2.0\nstatus=failed\n").map(|outcome| outcome.status),
            Some(InstallStatus::Failed("The install didn't finish.".into()))
        );
        assert_eq!(parse_result("status=ok\n"), None);
        assert_eq!(parse_result("version=nope\nstatus=ok\n"), None);
        assert_eq!(parse_result("version=0.2.0\nstatus=maybe\n"), None);
        assert_eq!(parse_result(""), None);
    }
}
