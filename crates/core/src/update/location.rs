//! Where the running app lives, and whether Newjen can replace it there.

use std::fs;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use semver::Version;

use super::{Unavailable, UpdateError};

/// The `.app` bundle that is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallTarget {
    app: PathBuf,
}

impl InstallTarget {
    /// The bundle around the running executable.
    pub fn current() -> Result<InstallTarget, Unavailable> {
        let exe = std::env::current_exe().map_err(|_| Unavailable::NotBundled)?;
        InstallTarget::from_exe(&exe)
    }

    /// The `X.app` that holds `exe` as `X.app/Contents/MacOS/<name>`. Symlinks are resolved,
    /// so the real bundle is replaced and not a link to it.
    pub fn from_exe(exe: &Path) -> Result<InstallTarget, Unavailable> {
        let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        let macos = exe.parent().ok_or(Unavailable::NotBundled)?;
        let contents = macos.parent().ok_or(Unavailable::NotBundled)?;
        let app = contents.parent().ok_or(Unavailable::NotBundled)?;
        let shaped = macos.file_name().is_some_and(|name| name == "MacOS")
            && contents.file_name().is_some_and(|name| name == "Contents")
            && app.extension().is_some_and(|extension| extension == "app");
        if shaped {
            Ok(InstallTarget {
                app: app.to_path_buf(),
            })
        } else {
            Err(Unavailable::NotBundled)
        }
    }

    pub fn app(&self) -> &Path {
        &self.app
    }

    pub fn parent(&self) -> &Path {
        self.app.parent().unwrap_or(Path::new("/"))
    }

    /// Fails when macOS runs the app from a read-only copy (App Translocation, which happens
    /// to a quarantined app opened straight from a download), or when the folder that holds it
    /// is not writable by this user.
    pub fn check_writable(&self) -> Result<(), UpdateError> {
        if self.app.to_string_lossy().contains("/AppTranslocation/") {
            return Err(UpdateError::Translocated);
        }
        let probe = self.parent().join(format!(
            ".{}-update-probe-{}",
            self.stem(),
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(_) => {
                let _ = fs::remove_file(&probe);
                Ok(())
            }
            Err(_) => Err(UpdateError::CantWrite {
                folder: self.parent().to_path_buf(),
            }),
        }
    }

    /// Where a version is unzipped, a sibling of the app so the final rename never crosses
    /// volumes.
    pub fn stage_dir(&self, version: &Version) -> PathBuf {
        self.parent()
            .join(format!(".{}-update-{version}", self.stem()))
    }

    /// Creates `dir` and its missing parents readable by this user only, so nothing else can
    /// swap files in between the signature check and the unpack.
    pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
    }

    /// Where the running bundle waits while its replacement moves into place. Each attempt has
    /// its own name, so an old bundle that could not be deleted never blocks the next update.
    pub fn old_path(&self, version: &Version, pid: u32) -> PathBuf {
        self.parent()
            .join(format!(".{}.app.old.{version}.{pid}", self.stem()))
    }

    /// Where the staged bundle sits next to the app just before the two renames.
    pub fn new_path(&self, version: &Version, pid: u32) -> PathBuf {
        self.parent()
            .join(format!(".{}.app.new.{version}.{pid}", self.stem()))
    }

    /// Written by the install script: whether the last swap worked.
    pub fn result_path(&self) -> PathBuf {
        self.parent()
            .join(format!(".{}-update-result", self.stem()))
    }

    /// Present while an install script may still be running. It holds the script's pid.
    pub fn pending_path(&self) -> PathBuf {
        self.parent()
            .join(format!(".{}-update-pending", self.stem()))
    }

    /// Whether an install script started by a Newjen is still alive.
    pub fn install_pending(&self) -> bool {
        fs::read_to_string(self.pending_path())
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            .is_some_and(|pid| pid > 0 && process_alive(pid))
    }

    /// Removes what an earlier update attempt left next to the app: staged folders, probes,
    /// and moved-aside bundles once the app itself is back in place. It keeps everything while
    /// an install script is still running, and keeps the script's result file, which
    /// [`take_install_result`] reads. Errors are ignored, because the next attempt tries
    /// again.
    pub fn clean_leftovers(&self) {
        if self.install_pending() {
            return;
        }
        let _ = fs::remove_file(self.pending_path());
        let stem = self.stem();
        let staged = format!(".{stem}-update-");
        let beside = [format!(".{stem}.app.old"), format!(".{stem}.app.new")];
        let result = self.result_path();
        let app_present = self.app.exists();
        let Ok(entries) = fs::read_dir(self.parent()) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let ours = name.starts_with(&staged)
                || (app_present && beside.iter().any(|prefix| name.starts_with(prefix)));
            if !ours || path == result {
                continue;
            }
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }

    pub(super) fn stem(&self) -> String {
        self.app.file_stem().map_or_else(
            || "Newjen".into(),
            |stem| stem.to_string_lossy().into_owned(),
        )
    }
}

pub(super) fn process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let sent = unsafe { libc::kill(pid, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn bundle(root: &Path, name: &str) -> PathBuf {
        let macos = root.join(name).join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("newjen");
        fs::write(&exe, "").unwrap();
        exe
    }

    #[test]
    fn finds_the_bundle_around_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path(), "Newjen.app");
        let target = InstallTarget::from_exe(&exe).unwrap();
        assert_eq!(
            target.app(),
            fs::canonicalize(dir.path()).unwrap().join("Newjen.app")
        );
        assert_eq!(target.check_writable(), Ok(()));
    }

    #[test]
    fn a_bare_binary_is_not_bundled() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("target/debug/newjen");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, "").unwrap();
        assert_eq!(InstallTarget::from_exe(&exe), Err(Unavailable::NotBundled));
        assert_eq!(
            InstallTarget::from_exe(&dir.path().join("missing")),
            Err(Unavailable::NotBundled)
        );
    }

    #[test]
    fn translocated_and_read_only_locations_are_typed() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(&dir.path().join("AppTranslocation/ABC/d"), "Newjen.app");
        let target = InstallTarget::from_exe(&exe).unwrap();
        assert_eq!(target.check_writable(), Err(UpdateError::Translocated));

        let exe = bundle(&dir.path().join("locked"), "Newjen.app");
        let target = InstallTarget::from_exe(&exe).unwrap();
        let folder = target.parent().to_path_buf();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
        let result = target.check_writable();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result, Err(UpdateError::CantWrite { folder }));
    }

    #[test]
    fn cleaning_removes_leftovers_and_keeps_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path(), "Newjen.app");
        let target = InstallTarget::from_exe(&exe).unwrap();
        let version = Version::new(0, 2, 0);
        fs::create_dir_all(target.stage_dir(&version).join("extract")).unwrap();
        let old = target.old_path(&version, 7);
        let spare = target.parent().join(".Newjen.app.old");
        fs::create_dir_all(old.join("Contents")).unwrap();
        fs::create_dir_all(&spare).unwrap();
        fs::create_dir_all(target.new_path(&version, 7)).unwrap();
        fs::write(target.parent().join("keep.txt"), "").unwrap();
        fs::write(target.result_path(), "version=0.2.0\nstatus=failed\n").unwrap();
        target.clean_leftovers();
        assert!(!target.stage_dir(&version).exists());
        assert!(!old.exists());
        assert!(!spare.exists());
        assert!(!target.new_path(&version, 7).exists());
        assert!(target.app().exists());
        assert!(target.parent().join("keep.txt").exists());
        assert!(target.result_path().exists(), "the result waits to be read");
    }

    #[test]
    fn a_running_install_script_keeps_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundle(dir.path(), "Newjen.app");
        let target = InstallTarget::from_exe(&exe).unwrap();
        let version = Version::new(0, 2, 0);
        fs::create_dir_all(target.stage_dir(&version)).unwrap();
        fs::write(target.pending_path(), std::process::id().to_string()).unwrap();
        assert!(target.install_pending());
        target.clean_leftovers();
        assert!(target.stage_dir(&version).exists());

        fs::write(target.pending_path(), "2147483646").unwrap();
        assert!(!target.install_pending());
        target.clean_leftovers();
        assert!(!target.stage_dir(&version).exists());
        assert!(!target.pending_path().exists());
    }

    #[test]
    fn private_dirs_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join(".Newjen-update-0.2.0");
        InstallTarget::create_private_dir(&stage).unwrap();
        let mode = fs::metadata(&stage).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}
