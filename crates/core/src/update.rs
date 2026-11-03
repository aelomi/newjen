//! In-app updates. Newjen reads the releases Atom feed on github.com (never the GitHub API),
//! downloads the release zip and its Ed25519 signature with the system `curl`, checks the
//! signature, unpacks the zip beside the installed app, and swaps it in after Newjen exits.
//!
//! - [`Updater`] holds the feed, the running version and the install location. Its methods
//!   block, so the app runs them on a background executor: [`FIRST_CHECK`] after launch, then
//!   every [`CHECK_INTERVAL`]. [`Updater::run_once`] does check, download and staging in one
//!   call and reports each [`UpdateState`] on the way.
//! - [`install`] hands the swap to a detached script that writes down how it went; the next
//!   launch reads that with [`Updater::startup`]. [`signature`] holds the compiled-in public
//!   key. The `newjen-sign` binary makes and checks signatures.
//! - [`kill_helpers`] stops curl, ditto and codesign if Newjen quits while they run.
//! - Nothing here touches the `claude` or `codex` CLIs or their credentials, and an update
//!   never starts a turn.

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use semver::Version;

mod download;
mod feed;
mod install;
mod location;
mod process;
pub mod signature;
mod stage;

pub use feed::{Release, newest_after, parse_feed};
pub use install::{
    Install, InstallFailure, InstallOutcome, InstallStatus, install, parse_result, spawn_install,
    take_install_result,
};
pub use location::InstallTarget;
pub use process::kill_helpers;
pub use stage::{BUNDLE_ID, StagedUpdate};

use download::{Curl, Fetched};

/// The release version this binary was built as, set by `scripts/bundle-app.sh`. `None` in
/// every other build.
pub const RELEASE_VERSION: Option<&str> = option_env!("NEWJEN_RELEASE_VERSION");

/// The repository whose releases feed and assets are used.
pub const DEFAULT_REPOSITORY: &str = "https://github.com/aelomi/newjen";

/// Replaces [`DEFAULT_REPOSITORY`] when set, such as to `file:///tmp/fake-releases` in a local
/// end-to-end test. The signature check still applies.
pub const FEED_ENV: &str = "NEWJEN_UPDATE_FEED";

/// How long after launch the first check runs.
pub const FIRST_CHECK: Duration = Duration::from_secs(15);

/// How often the app checks again.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The running version, or `None` for a build that was not made by the release script.
pub fn running_version() -> Option<Version> {
    RELEASE_VERSION.and_then(|version| version.parse().ok())
}

/// Why updating is not offered at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// Not a release build, such as `cargo run`.
    DevBuild,
    /// A release binary that is not inside an `.app` bundle.
    NotBundled,
}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unavailable::DevBuild => "Updates work in the released app.",
            Unavailable::NotBundled => {
                "Updates work in the released app, and this copy isn't running from a Newjen app bundle."
            }
        })
    }
}

/// Why an update did not happen. [`fmt::Display`] is a sentence the app can show as it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// GitHub could not be reached.
    Offline,
    /// The download is not Newjen's own signed release. It was deleted.
    Verification(String),
    /// The release has no signature file yet, so nothing was downloaded.
    NotSigned,
    /// The last install did not finish. The old version is still the one running.
    InstallFailed(InstallFailure),
    /// Newjen cannot replace itself in `folder`.
    CantWrite {
        folder: PathBuf,
    },
    /// macOS runs the app from a temporary read-only copy.
    Translocated,
    Other(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::Offline => f.write_str(
                "Newjen couldn't reach GitHub. Check your connection and try again.",
            ),
            UpdateError::Verification(detail) => write!(
                f,
                "The update failed Newjen's safety check and was thrown away, so nothing was installed. {detail}"
            ),
            UpdateError::NotSigned => {
                f.write_str("The release isn't signed yet. Newjen will try again later.")
            }
            UpdateError::InstallFailed(failure) => failure.fmt(f),
            UpdateError::CantWrite { folder } => write!(
                f,
                "Newjen can't replace itself in {}. Move it to your Applications folder, or update it from the releases page.",
                folder.display()
            ),
            UpdateError::Translocated => f.write_str(
                "macOS is running Newjen from a temporary location. Move Newjen to your Applications folder, open it from there, and update again.",
            ),
            UpdateError::Other(detail) => f.write_str(detail),
        }
    }
}

impl std::error::Error for UpdateError {}

/// Where the update flow is. The app keeps one of these and renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate {
        /// Unix milliseconds.
        checked_at: i64,
    },
    Downloading {
        version: Version,
        received: u64,
        /// `None` until the server says how big the file is.
        total: Option<u64>,
    },
    /// Downloaded; checking the signature and unpacking.
    Preparing {
        version: Version,
    },
    /// Ready for [`install`].
    Ready(StagedUpdate),
    /// A newer version exists but cannot be installed in place. The app can offer
    /// [`Updater::releases_page`] instead.
    Blocked {
        version: Version,
        reason: UpdateError,
    },
    Failed(UpdateError),
    Unavailable(Unavailable),
}

/// The feed, the running version and the install location.
#[derive(Debug, Clone)]
pub struct Updater {
    base: String,
    current: Version,
    target: InstallTarget,
    public_key: [u8; 32],
}

impl Updater {
    /// The updater for this process: the compiled-in version, the running bundle, the
    /// compiled-in key, and [`FEED_ENV`] if set.
    pub fn from_env() -> Result<Updater, Unavailable> {
        let current = running_version().ok_or(Unavailable::DevBuild)?;
        let target = InstallTarget::current()?;
        let base = std::env::var(FEED_ENV)
            .ok()
            .filter(|base| !base.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_REPOSITORY.into());
        Ok(Updater::new(
            &base,
            current,
            target,
            signature::UPDATE_PUBLIC_KEY,
        ))
    }

    pub fn new(base: &str, current: Version, target: InstallTarget, public_key: [u8; 32]) -> Self {
        Updater {
            base: base.trim_end_matches('/').to_owned(),
            current,
            target,
            public_key,
        }
    }

    pub fn current_version(&self) -> &Version {
        &self.current
    }

    pub fn target(&self) -> &InstallTarget {
        &self.target
    }

    /// The page to send a user to when an update can't be installed in place.
    pub fn releases_page(&self) -> String {
        format!("{}/releases", self.base)
    }

    /// Removes what an earlier update left beside the app. Call it at launch, after
    /// [`Updater::startup`].
    pub fn clean_leftovers(&self) {
        self.target.clean_leftovers();
    }

    /// What to do at launch: reads and clears the last install's result, then removes what it
    /// left beside the app. Returns the failure of an install that did not finish, if it is for
    /// a version above the one running. Blocks briefly, so call it off the main thread.
    pub fn startup(&self) -> Option<InstallFailure> {
        let outcome = take_install_result(&self.target);
        self.target.clean_leftovers();
        match outcome?.into_failure() {
            Some(failure) if failure.version > self.current => Some(failure),
            _ => None,
        }
    }

    /// Reads the feed and returns the newest release above the running version, if any.
    /// Blocks for up to about 30 seconds.
    pub fn check(&self) -> Result<Option<Release>, UpdateError> {
        let xml = self.curl().text(&format!("{}/releases.atom", self.base))?;
        Ok(newest_after(parse_feed(&xml), &self.current))
    }

    /// Downloads and stages `release`, reporting [`UpdateState::Downloading`] and
    /// [`UpdateState::Preparing`] as it goes. Returns `Ok(None)` when `cancel` was set; a
    /// failure or a cancel leaves nothing behind. Blocks for the whole download.
    pub fn prepare(
        &self,
        release: &Release,
        report: &mut dyn FnMut(UpdateState),
        cancel: &AtomicBool,
    ) -> Result<Option<StagedUpdate>, UpdateError> {
        self.target.check_writable()?;
        self.target.clean_leftovers();
        let stage_dir = self.target.stage_dir(&release.version);
        let outcome = self.download_and_stage(release, &stage_dir, report, cancel);
        if !matches!(outcome, Ok(Some(_))) {
            let _ = fs::remove_dir_all(&stage_dir);
        }
        outcome
    }

    fn download_and_stage(
        &self,
        release: &Release,
        stage_dir: &std::path::Path,
        report: &mut dyn FnMut(UpdateState),
        cancel: &AtomicBool,
    ) -> Result<Option<StagedUpdate>, UpdateError> {
        let version = &release.version;
        InstallTarget::create_private_dir(stage_dir).map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                UpdateError::CantWrite {
                    folder: self.target.parent().to_path_buf(),
                }
            } else {
                UpdateError::Other(format!("Couldn't make a folder for the update: {error}"))
            }
        })?;
        let url = format!(
            "{}/releases/download/{}/{}",
            self.base,
            release.tag,
            release.zip_name()
        );
        report(UpdateState::Downloading {
            version: version.clone(),
            received: 0,
            total: None,
        });
        let signature = self
            .curl()
            .fetch_text(&format!("{url}.sig"))
            .map_err(|error| match error {
                UpdateError::Other(detail) => UpdateError::Verification(format!(
                    "Newjen couldn't get this release's signature. {detail}"
                )),
                other => other,
            })?
            .ok_or(UpdateError::NotSigned)?;
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let zip = stage_dir.join(release.zip_name());
        let fetched = self.curl().file(
            &url,
            &zip,
            &mut |received, total| {
                report(UpdateState::Downloading {
                    version: version.clone(),
                    received,
                    total,
                });
            },
            cancel,
        )?;
        if fetched == Fetched::Cancelled || cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        report(UpdateState::Preparing {
            version: version.clone(),
        });
        stage::stage_zip(
            &self.target,
            version,
            &zip,
            &signature,
            &self.public_key,
            cancel,
        )
    }

    /// Verifies and unpacks a zip that is already on disk. `zip` must be inside the stage
    /// folder, which a failure removes. Staging the same version again replaces what is there.
    pub fn stage_zip(
        &self,
        version: &Version,
        zip: &std::path::Path,
        signature_hex: &str,
    ) -> Result<StagedUpdate, UpdateError> {
        stage::stage_zip(
            &self.target,
            version,
            zip,
            signature_hex,
            &self.public_key,
            &AtomicBool::new(false),
        )?
        .ok_or_else(|| UpdateError::Other("The update was cancelled.".into()))
    }

    /// Check, download, verify and stage in one blocking call, reporting every state
    /// including the last, which is also returned. Setting `cancel` ends the work at the next
    /// step and returns [`UpdateState::Idle`]. A release at or below `skip` counts as up to
    /// date: the quiet hourly check passes the version that failed to install or to download,
    /// so it does not try the same release over and over, while a check the user asked for
    /// passes `None` and tries again.
    pub fn run_once(
        &self,
        skip: Option<&Version>,
        report: &mut dyn FnMut(UpdateState),
        cancel: &AtomicBool,
    ) -> UpdateState {
        report(UpdateState::Checking);
        let last = self.run_inner(skip, report, cancel);
        report(last.clone());
        last
    }

    fn run_inner(
        &self,
        skip: Option<&Version>,
        report: &mut dyn FnMut(UpdateState),
        cancel: &AtomicBool,
    ) -> UpdateState {
        let release = match self.check() {
            Ok(Some(release)) => release,
            Ok(None) => {
                return UpdateState::UpToDate {
                    checked_at: crate::store::now_ms(),
                };
            }
            Err(error) => return UpdateState::Failed(error),
        };
        if cancel.load(Ordering::Relaxed) {
            return UpdateState::Idle;
        }
        if skip.is_some_and(|skip| release.version <= *skip) {
            return UpdateState::UpToDate {
                checked_at: crate::store::now_ms(),
            };
        }
        if let Err(reason) = self.target.check_writable() {
            return UpdateState::Blocked {
                version: release.version,
                reason,
            };
        }
        match self.prepare(&release, report, cancel) {
            Ok(Some(staged)) => UpdateState::Ready(staged),
            Ok(None) => UpdateState::Idle,
            Err(error) => UpdateState::Failed(error),
        }
    }

    fn curl(&self) -> Curl {
        Curl {
            https_only: self.base.starts_with("https://"),
        }
    }
}
