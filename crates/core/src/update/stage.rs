//! Unpacks a downloaded, signed zip next to the installed app and checks that it really is
//! Newjen, ready to be moved into place.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use semver::Version;

use super::UpdateError;
use super::location::InstallTarget;
use super::process;
use super::signature::{self, SignatureError};

/// The bundle identifier every Newjen release carries.
pub const BUNDLE_ID: &str = "com.aelomi.newjen";

/// An update that is downloaded, verified and unpacked. Nothing is installed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUpdate {
    pub version: Version,
    /// The unpacked bundle, waiting to replace [`StagedUpdate::target`].
    pub staged_app: PathBuf,
    /// The folder that holds `staged_app`; it is removed after the swap.
    pub stage_dir: PathBuf,
    pub target: InstallTarget,
}

/// Verifies `zip` against `signature_hex` and `public_key`, unpacks it under the target's
/// stage folder and checks the bundle. Any failure removes the whole stage folder, the zip
/// included, so a rejected download never stays on disk. `Ok(None)` means `cancel` was set,
/// which cleans up the same way.
pub(super) fn stage_zip(
    target: &InstallTarget,
    version: &Version,
    zip: &Path,
    signature_hex: &str,
    public_key: &[u8; 32],
    cancel: &AtomicBool,
) -> Result<Option<StagedUpdate>, UpdateError> {
    let stage_dir = target.stage_dir(version);
    let result = stage_inner(
        target,
        version,
        zip,
        signature_hex,
        public_key,
        &stage_dir,
        cancel,
    );
    if !matches!(result, Ok(Some(_))) {
        let _ = fs::remove_file(zip);
        let _ = fs::remove_dir_all(&stage_dir);
    }
    result
}

fn stage_inner(
    target: &InstallTarget,
    version: &Version,
    zip: &Path,
    signature_hex: &str,
    public_key: &[u8; 32],
    stage_dir: &Path,
    cancel: &AtomicBool,
) -> Result<Option<StagedUpdate>, UpdateError> {
    let bytes = fs::read(zip)
        .map_err(|error| UpdateError::Other(format!("Couldn't read the download: {error}")))?;
    signature::verify_with(public_key, &bytes, signature_hex)
        .map_err(|error| UpdateError::Verification(signature_message(&error).into()))?;
    drop(bytes);
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }

    let extract = stage_dir.join("extract");
    let _ = fs::remove_dir_all(&extract);
    let unpacked = run(
        Command::new("/usr/bin/ditto")
            .arg("-x")
            .arg("-k")
            .arg(zip)
            .arg(&extract),
        "unpack the update",
        cancel,
    )
    .map_err(UpdateError::Other)?;
    if unpacked.is_none() {
        return Ok(None);
    }
    let staged_app = only_app(&extract)?;

    if check_bundle(&staged_app, version, cancel)?.is_none() {
        return Ok(None);
    }
    let _ = Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&staged_app)
        .output();
    let _ = fs::remove_file(zip);
    Ok(Some(StagedUpdate {
        version: version.clone(),
        staged_app,
        stage_dir: stage_dir.to_path_buf(),
        target: target.clone(),
    }))
}

fn signature_message(error: &SignatureError) -> &'static str {
    match error {
        SignatureError::KeyNotSet => {
            "This copy of Newjen has no update signing key, so it can't check updates."
        }
        SignatureError::Malformed => "The update's signature file is malformed.",
        SignatureError::Mismatch => "The update's signature doesn't match its contents.",
    }
}

fn only_app(extract: &Path) -> Result<PathBuf, UpdateError> {
    let mut apps = fs::read_dir(extract)
        .map_err(|error| UpdateError::Other(format!("Couldn't open the unpacked update: {error}")))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"));
    match (apps.next(), apps.next()) {
        (Some(app), None) => Ok(app),
        _ => Err(UpdateError::Verification(
            "The update doesn't contain exactly one app.".into(),
        )),
    }
}

fn check_bundle(
    app: &Path,
    version: &Version,
    cancel: &AtomicBool,
) -> Result<Option<()>, UpdateError> {
    let plist = app.join("Contents/Info.plist");
    let key = |name: &str| plist_value(&plist, name);
    let identifier = key("CFBundleIdentifier");
    if identifier.as_deref() != Some(BUNDLE_ID) {
        return Err(UpdateError::Verification(format!(
            "The update is not Newjen (bundle identifier {}).",
            identifier.as_deref().unwrap_or("missing")
        )));
    }
    let release = key("NewjenReleaseVersion");
    if release.as_deref() != Some(version.to_string().as_str()) {
        return Err(UpdateError::Verification(format!(
            "The update says it is version {}, not {version}.",
            release.as_deref().unwrap_or("(none)")
        )));
    }
    let executable = key("CFBundleExecutable").unwrap_or_default();
    if executable.is_empty() || !app.join("Contents/MacOS").join(&executable).is_file() {
        return Err(UpdateError::Verification(
            "The update has no program to run.".into(),
        ));
    }
    run(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app),
        "check the update's code signature",
        cancel,
    )
    .map(|done| done.map(drop))
    .map_err(UpdateError::Verification)
}

fn plist_value(plist: &Path, key: &str) -> Option<String> {
    let output = Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Print :{key}"))
        .arg(plist)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Runs a command, turning a failure into a sentence with the tool's own words. `Ok(None)`
/// when `cancel` stopped it.
fn run(command: &mut Command, what: &str, cancel: &AtomicBool) -> Result<Option<()>, String> {
    let output = match process::run(command, cancel) {
        Ok(output) => output,
        Err(error) if error.kind() == ErrorKind::Interrupted => return Ok(None),
        Err(error) => return Err(format!("Couldn't {what}: {error}")),
    };
    if output.status.success() {
        return Ok(Some(()));
    }
    let detail = String::from_utf8_lossy(&output.stderr);
    let detail = detail.trim();
    Err(if detail.is_empty() {
        format!("Couldn't {what}.")
    } else {
        format!("Couldn't {what}: {detail}")
    })
}
