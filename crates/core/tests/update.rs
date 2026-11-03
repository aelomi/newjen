//! The update flow without a network: the Atom feed, signatures, staging, the install script
//! and a whole check-download-stage-install round trip against `file://` URLs.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use newjen_core::update::signature::{self, encode_hex};
use newjen_core::update::{
    BUNDLE_ID, Install, InstallOutcome, InstallStatus, InstallTarget, Release, StagedUpdate,
    UpdateError, UpdateState, Updater, newest_after, parse_feed, parse_result, spawn_install,
};
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};
use semver::Version;

fn v(text: &str) -> Version {
    text.parse().unwrap()
}

struct Keys {
    pair: Ed25519KeyPair,
    public: [u8; 32],
}

fn keys() -> Keys {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let public = pair.public_key().as_ref().try_into().unwrap();
    Keys { pair, public }
}

fn sign(keys: &Keys, file: &Path) -> String {
    encode_hex(keys.pair.sign(&fs::read(file).unwrap()).as_ref())
}

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{program} {args:?}");
}

/// A signed `Newjen.app` whose program is a copy of `/usr/bin/true`, zipped with `ditto` the
/// way `scripts/bundle-app.sh` does it.
fn release_zip(dir: &Path, identifier: &str, plist_version: &str, ad_hoc_sign: bool) -> PathBuf {
    let app = dir.join("build/Newjen.app");
    let _ = fs::remove_dir_all(dir.join("build"));
    fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    fs::copy("/usr/bin/true", app.join("Contents/MacOS/newjen")).unwrap();
    fs::write(
        app.join("Contents/Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{identifier}</string>
<key>CFBundleExecutable</key><string>newjen</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NewjenReleaseVersion</key><string>{plist_version}</string>
</dict></plist>"#
        ),
    )
    .unwrap();
    if ad_hoc_sign {
        run(
            "/usr/bin/codesign",
            &["--force", "--deep", "-s", "-", app.to_str().unwrap()],
        );
    }
    let zip = dir.join(format!("Newjen-{plist_version}-arm64.zip"));
    let _ = fs::remove_file(&zip);
    run(
        "/usr/bin/ditto",
        &[
            "-c",
            "-k",
            "--keepParent",
            app.to_str().unwrap(),
            zip.to_str().unwrap(),
        ],
    );
    zip
}

/// An "installed" app, and the updater for it.
fn installed(dir: &Path, keys: &Keys, base: &str) -> Updater {
    let exe = dir.join("apps/Newjen.app/Contents/MacOS/newjen");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, "old").unwrap();
    fs::write(dir.join("apps/Newjen.app/Contents/Info.plist"), "old").unwrap();
    let target = InstallTarget::from_exe(&exe).unwrap();
    Updater::new(base, v("0.1.0"), target, keys.public)
}

fn stage_dir_gone(updater: &Updater, version: &str) -> bool {
    !updater.target().stage_dir(&v(version)).exists()
}

// ---------------------------------------------------------------------------------------------
// Versions and the feed
// ---------------------------------------------------------------------------------------------

#[test]
fn prereleases_order_alpha_beta_rc_final() {
    let order = [
        "0.1.0-alpha.1",
        "0.1.0-alpha.2",
        "0.1.0-alpha.10",
        "0.1.0-beta.1",
        "0.1.0-rc.1",
        "0.1.0",
        "0.1.1-alpha.1",
        "0.2.0",
        "1.0.0",
    ];
    for pair in order.windows(2) {
        assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
    }
}

fn fixture_feed() -> Vec<Release> {
    parse_feed(include_str!("fixtures/update/releases.atom"))
}

#[test]
fn the_feed_yields_every_semver_tag_and_skips_the_rest() {
    let releases = fixture_feed();
    let versions: Vec<String> = releases.iter().map(|r| r.version.to_string()).collect();
    assert_eq!(
        versions,
        [
            "0.1.0-rc.1",
            "0.1.0-beta.1",
            "0.1.0-alpha.10",
            "0.1.0-alpha.1"
        ]
    );
    let alpha = releases.last().unwrap();
    assert_eq!(alpha.tag, "v0.1.0-alpha.1");
    assert_eq!(alpha.published.as_deref(), Some("2026-11-01T09:17:00Z"));
}

#[test]
fn the_newest_release_above_the_running_version_wins() {
    let newest = |current: &str| {
        newest_after(fixture_feed(), &v(current)).map(|release| release.version.to_string())
    };
    assert_eq!(newest("0.1.0-alpha.1").as_deref(), Some("0.1.0-rc.1"));
    assert_eq!(newest("0.1.0-beta.1").as_deref(), Some("0.1.0-rc.1"));
    assert_eq!(newest("0.1.0-rc.1"), None);
    assert_eq!(newest("0.1.0"), None);
    assert_eq!(
        newest("0.0.9"),
        None,
        "a stable install is not offered prereleases"
    );
}

#[test]
fn only_a_prerelease_user_is_offered_a_prerelease() {
    let feed = |tags: &[&str]| {
        tags.iter()
            .map(|tag| Release {
                version: v(tag),
                tag: format!("v{tag}"),
                published: None,
            })
            .collect::<Vec<_>>()
    };
    let releases = feed(&["0.3.0-rc.1", "0.2.0", "0.1.0"]);
    let newest = |current: &str| {
        newest_after(releases.clone(), &v(current)).map(|release| release.version.to_string())
    };
    assert_eq!(newest("0.1.0").as_deref(), Some("0.2.0"));
    assert_eq!(newest("0.2.0"), None);
    assert_eq!(newest("0.2.0-beta.1").as_deref(), Some("0.3.0-rc.1"));
    assert_eq!(newest("0.1.0-alpha.1").as_deref(), Some("0.3.0-rc.1"));
}

#[test]
fn an_empty_or_garbled_feed_has_no_releases() {
    assert!(parse_feed("").is_empty());
    assert!(parse_feed("<html>rate limited</html>").is_empty());
    assert!(parse_feed("<entry><link href=\"https://x/releases/tag/v1.0.0\"").len() <= 1);
}

// ---------------------------------------------------------------------------------------------
// Staging
// ---------------------------------------------------------------------------------------------

#[test]
fn staging_accepts_a_signed_newjen_zip_and_unpacks_it_beside_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let keys = keys();
    let updater = installed(dir.path(), &keys, "file:///unused");
    let zip = release_zip(dir.path(), BUNDLE_ID, "0.2.0", true);
    let stage = updater.target().stage_dir(&v("0.2.0"));
    fs::create_dir_all(&stage).unwrap();
    let in_stage = stage.join("Newjen-0.2.0-arm64.zip");
    fs::copy(&zip, &in_stage).unwrap();

    let staged = updater
        .stage_zip(&v("0.2.0"), &in_stage, &sign(&keys, &zip))
        .unwrap();

    assert_eq!(staged.version, v("0.2.0"));
    assert!(staged.staged_app.join("Contents/Info.plist").is_file());
    assert_eq!(staged.stage_dir, stage);
    assert_eq!(staged.stage_dir.parent(), updater.target().app().parent());
    assert!(!in_stage.exists(), "the zip is deleted once unpacked");
}

#[test]
fn staging_the_same_version_twice_converges() {
    let dir = tempfile::tempdir().unwrap();
    let keys = keys();
    let updater = installed(dir.path(), &keys, "file:///unused");
    let zip = release_zip(dir.path(), BUNDLE_ID, "0.2.0", true);
    let signature = sign(&keys, &zip);
    let stage = updater.target().stage_dir(&v("0.2.0"));
    let mut results = Vec::new();
    for _ in 0..2 {
        fs::create_dir_all(&stage).unwrap();
        let in_stage = stage.join("Newjen-0.2.0-arm64.zip");
        fs::copy(&zip, &in_stage).unwrap();
        results.push(
            updater
                .stage_zip(&v("0.2.0"), &in_stage, &signature)
                .unwrap(),
        );
    }
    assert_eq!(results[0], results[1]);
    assert!(results[1].staged_app.is_dir());
}

fn stage_expecting_rejection(
    keys_for_updater: &Keys,
    zip_keys: &Keys,
    identifier: &str,
    plist_version: &str,
    ad_hoc_sign: bool,
) -> (Updater, UpdateError, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let updater = installed(dir.path(), keys_for_updater, "file:///unused");
    let zip = release_zip(dir.path(), identifier, plist_version, ad_hoc_sign);
    let stage = updater.target().stage_dir(&v("0.2.0"));
    fs::create_dir_all(&stage).unwrap();
    let in_stage = stage.join("Newjen-0.2.0-arm64.zip");
    fs::copy(&zip, &in_stage).unwrap();
    let error = updater
        .stage_zip(&v("0.2.0"), &in_stage, &sign(zip_keys, &zip))
        .unwrap_err();
    assert!(
        stage_dir_gone(&updater, "0.2.0"),
        "a rejected download leaves nothing behind"
    );
    (updater, error, dir)
}

#[test]
fn staging_rejects_a_bad_signature_and_deletes_the_download() {
    let (trusted, attacker) = (keys(), keys());
    let (_, error, _dir) = stage_expecting_rejection(&trusted, &attacker, BUNDLE_ID, "0.2.0", true);
    assert!(matches!(error, UpdateError::Verification(_)), "{error:?}");
}

#[test]
fn staging_rejects_the_wrong_bundle_identifier() {
    let keys = keys();
    let (_, error, _dir) =
        stage_expecting_rejection(&keys, &keys, "com.example.other", "0.2.0", true);
    let UpdateError::Verification(detail) = error else {
        panic!("expected a verification error, got {error:?}");
    };
    assert!(detail.contains("com.example.other"), "{detail}");
}

#[test]
fn staging_rejects_a_bundle_that_claims_another_version() {
    let keys = keys();
    let (_, error, _dir) = stage_expecting_rejection(&keys, &keys, BUNDLE_ID, "0.3.0", true);
    assert!(matches!(error, UpdateError::Verification(_)), "{error:?}");
}

#[test]
fn staging_rejects_a_bundle_whose_code_signature_is_broken() {
    let keys = keys();
    let (_, error, _dir) = stage_expecting_rejection(&keys, &keys, BUNDLE_ID, "0.2.0", false);
    assert!(matches!(error, UpdateError::Verification(_)), "{error:?}");
}

#[test]
fn the_placeholder_key_refuses_everything() {
    let dir = tempfile::tempdir().unwrap();
    let keys = keys();
    let mut updater = installed(dir.path(), &keys, "file:///unused");
    updater = Updater::new(
        "file:///unused",
        v("0.1.0"),
        updater.target().clone(),
        signature::UNSET_PUBLIC_KEY,
    );
    let zip = release_zip(dir.path(), BUNDLE_ID, "0.2.0", true);
    let stage = updater.target().stage_dir(&v("0.2.0"));
    fs::create_dir_all(&stage).unwrap();
    let in_stage = stage.join("Newjen-0.2.0-arm64.zip");
    fs::copy(&zip, &in_stage).unwrap();
    let error = updater
        .stage_zip(&v("0.2.0"), &in_stage, &"0".repeat(128))
        .unwrap_err();
    assert!(matches!(error, UpdateError::Verification(_)), "{error:?}");
}

// ---------------------------------------------------------------------------------------------
// The install script
// ---------------------------------------------------------------------------------------------

/// A staged "update" and an installed "app" that are just folders with one file each.
struct Swap {
    _dir: tempfile::TempDir,
    root: PathBuf,
    staged: StagedUpdate,
}

fn swap() -> Swap {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let exe = root.join("apps/Newjen.app/Contents/MacOS/newjen");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, "old").unwrap();
    let target = InstallTarget::from_exe(&exe).unwrap();
    let version = v("0.2.0");
    let stage_dir = target.stage_dir(&version);
    let staged_app = stage_dir.join("extract/Newjen.app");
    fs::create_dir_all(staged_app.join("Contents/MacOS")).unwrap();
    fs::write(staged_app.join("Contents/MacOS/newjen"), "new").unwrap();
    let staged = StagedUpdate {
        version,
        staged_app,
        stage_dir,
        target,
    };
    Swap {
        _dir: dir,
        root,
        staged,
    }
}

fn installed_program(staged: &StagedUpdate) -> String {
    fs::read_to_string(staged.target.app().join("Contents/MacOS/newjen")).unwrap()
}

const SYSTEM_PATH: &str = "/usr/bin:/bin";

/// A process that has already exited, standing in for the Newjen the script waits for.
fn exited() -> u32 {
    let mut app = Command::new("true").spawn().unwrap();
    app.wait().unwrap();
    app.id()
}

fn plan(wait_for: u32) -> Install {
    let mut plan = Install::new(wait_for, false);
    plan.path = SYSTEM_PATH.into();
    plan.peer_wait_seconds = 1;
    plan
}

fn result_of(staged: &StagedUpdate) -> InstallOutcome {
    let text = fs::read_to_string(staged.target.result_path()).expect("a result file");
    parse_result(&text).expect("a readable result")
}

/// A `bin` folder with an `open` that records its arguments and, when `break_swap_to` is set, a
/// `mv` that refuses to move anything onto that path.
fn shims(root: &Path, break_swap_to: Option<&Path>) -> (String, PathBuf) {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = root.join("opened");
    let write = |name: &str, body: String| {
        let path = bin.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    write(
        "open",
        format!("#!/bin/sh\necho \"$@\" >> '{}'\n", marker.display()),
    );
    if let Some(refused) = break_swap_to {
        write(
            "mv",
            format!(
                "#!/bin/sh\nfor last; do :; done\n[ \"$last\" = '{}' ] && [ \"$#\" = 2 ] && case \"$1\" in *.app.new.*) exit 1;; esac\nexec /bin/mv \"$@\"\n",
                refused.display()
            ),
        );
    }
    (format!("{}:{SYSTEM_PATH}", bin.display()), marker)
}

#[test]
fn the_script_waits_for_the_app_to_exit_then_swaps_and_cleans_up() {
    let swap = swap();
    let mut app = Command::new("sleep").arg("1").spawn().unwrap();
    let mut script = spawn_install(&swap.staged, &plan(app.id())).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(400));
    assert_eq!(
        installed_program(&swap.staged),
        "old",
        "nothing moves while the app runs"
    );
    assert!(swap.staged.staged_app.exists());

    app.wait().unwrap();
    assert!(script.wait().unwrap().success());
    assert_eq!(installed_program(&swap.staged), "new");
    assert!(!swap.staged.stage_dir.exists());
    assert!(!swap.staged.target.old_path(&v("0.2.0"), app.id()).exists());
    assert!(!swap.staged.target.new_path(&v("0.2.0"), app.id()).exists());
    assert_eq!(
        result_of(&swap.staged),
        InstallOutcome {
            version: v("0.2.0"),
            status: InstallStatus::Installed
        }
    );
    assert!(!swap.staged.target.pending_path().exists());
}

#[test]
fn an_old_bundle_that_cannot_be_deleted_never_blocks_the_swap() {
    let swap = swap();
    let target = &swap.staged.target;
    let stale = target.parent().join(".Newjen.app.old");
    fs::create_dir_all(stale.join("Contents/locked")).unwrap();
    fs::write(stale.join("Contents/locked/file"), "").unwrap();
    fs::set_permissions(
        stale.join("Contents/locked"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    let pid = exited();
    let stale_unique = target.old_path(&v("0.2.0"), pid);
    fs::create_dir_all(stale_unique.join("Contents/locked")).unwrap();
    fs::write(stale_unique.join("Contents/locked/file"), "").unwrap();
    fs::set_permissions(
        stale_unique.join("Contents/locked"),
        fs::Permissions::from_mode(0o555),
    )
    .unwrap();

    let mut script = spawn_install(&swap.staged, &plan(pid)).unwrap();
    let status = script.wait().unwrap();
    for left in [&stale, &stale_unique] {
        let _ = fs::set_permissions(
            left.join("Contents/locked"),
            fs::Permissions::from_mode(0o755),
        );
    }

    assert!(status.success());
    assert_eq!(installed_program(&swap.staged), "new");
    assert!(
        !swap.staged.target.app().join("Newjen.app").exists(),
        "the app was not moved into a leftover folder"
    );
    assert_eq!(result_of(&swap.staged).status, InstallStatus::Installed);
}

#[test]
fn a_failed_move_is_written_down_and_leaves_the_old_app() {
    let swap = swap();
    let stage = swap.staged.staged_app.parent().unwrap().to_path_buf();
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o555)).unwrap();
    let mut script = spawn_install(&swap.staged, &plan(exited())).unwrap();
    let status = script.wait().unwrap();
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!status.success());
    assert_eq!(
        installed_program(&swap.staged),
        "old",
        "the user keeps the version they had"
    );
    assert_eq!(
        result_of(&swap.staged),
        InstallOutcome {
            version: v("0.2.0"),
            status: InstallStatus::Failed("Couldn't move the update next to Newjen.".into())
        }
    );
}

#[test]
fn the_old_app_comes_back_when_the_last_rename_fails() {
    let swap = swap();
    let (path, _) = shims(&swap.root, Some(swap.staged.target.app()));
    let mut install = plan(exited());
    install.path = path;
    let mut script = spawn_install(&swap.staged, &install).unwrap();

    assert!(!script.wait().unwrap().success());
    assert_eq!(installed_program(&swap.staged), "old");
    assert_eq!(
        result_of(&swap.staged).status,
        InstallStatus::Failed("Couldn't put the new version in place.".into())
    );
    let beside: Vec<_> = fs::read_dir(swap.staged.target.parent())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".Newjen.app."))
        .collect();
    assert!(beside.is_empty(), "left behind: {beside:?}");
}

#[test]
fn the_script_leaves_the_app_alone_when_nothing_is_staged() {
    let swap = swap();
    fs::remove_dir_all(&swap.staged.stage_dir).unwrap();
    let mut script = spawn_install(&swap.staged, &plan(exited())).unwrap();
    assert!(!script.wait().unwrap().success());
    assert_eq!(installed_program(&swap.staged), "old");
    assert_eq!(
        result_of(&swap.staged).status,
        InstallStatus::Failed("The downloaded update was gone.".into())
    );
}

#[test]
fn relaunch_opens_the_app_with_the_open_projects_after_a_swap_and_after_a_failed_one() {
    for (break_swap, expected_program) in [(false, "new"), (true, "old")] {
        let swap = swap();
        let (path, marker) = shims(&swap.root, None);
        let holder = swap.staged.staged_app.parent().unwrap().to_path_buf();
        if break_swap {
            fs::set_permissions(&holder, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let mut install = plan(exited());
        install.relaunch = true;
        install.path = path;
        install.projects = vec![
            PathBuf::from("/Users/someone/first"),
            PathBuf::from("/Users/someone/second"),
        ];
        let mut script = spawn_install(&swap.staged, &install).unwrap();
        script.wait().unwrap();
        let _ = fs::set_permissions(&holder, fs::Permissions::from_mode(0o755));

        assert_eq!(installed_program(&swap.staged), expected_program);
        assert_eq!(
            fs::read_to_string(&marker).unwrap().trim(),
            format!(
                "-n {} --args /Users/someone/first /Users/someone/second",
                swap.staged.target.app().display()
            )
        );
        assert!(
            swap.staged.target.result_path().exists(),
            "the result is written before the relaunch"
        );
    }
}

#[test]
fn relaunch_without_projects_passes_no_arguments() {
    let swap = swap();
    let (path, marker) = shims(&swap.root, None);
    let mut install = plan(exited());
    install.relaunch = true;
    install.path = path;
    spawn_install(&swap.staged, &install)
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(
        fs::read_to_string(&marker).unwrap().trim(),
        format!("-n {}", swap.staged.target.app().display())
    );
}

/// A program inside the installed bundle that stays alive, like a second Newjen would.
fn peer_in_the_bundle(staged: &StagedUpdate, seconds: &str) -> std::process::Child {
    use std::os::unix::process::CommandExt as _;
    let program = staged.target.app().join("Contents/MacOS/peer");
    Command::new("/bin/sleep")
        .arg0(&program)
        .arg(seconds)
        .spawn()
        .unwrap()
}

#[test]
fn the_swap_waits_for_another_newjen_in_the_same_bundle() {
    let swap = swap();
    let mut peer = peer_in_the_bundle(&swap.staged, "1");
    let mut install = plan(exited());
    install.peer_wait_seconds = 20;
    let mut script = spawn_install(&swap.staged, &install).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(installed_program(&swap.staged), "old");
    peer.wait().unwrap();
    assert!(script.wait().unwrap().success());
    assert_eq!(installed_program(&swap.staged), "new");
}

#[test]
fn the_swap_refuses_while_another_newjen_keeps_running() {
    let swap = swap();
    let mut peer = peer_in_the_bundle(&swap.staged, "30");
    let mut script = spawn_install(&swap.staged, &plan(exited())).unwrap();
    let status = script.wait().unwrap();
    peer.kill().unwrap();
    peer.wait().unwrap();

    assert!(!status.success());
    assert_eq!(installed_program(&swap.staged), "old");
    assert!(swap.staged.staged_app.exists(), "the staged update waits");
    assert_eq!(
        result_of(&swap.staged).status,
        InstallStatus::Failed(
            "Another Newjen is still running from this app. Quit it and update again.".into()
        )
    );
}

#[test]
fn launches_leave_the_staged_files_alone_while_the_script_runs() {
    let swap = swap();
    let mut app = Command::new("sleep").arg("2").spawn().unwrap();
    let mut script = spawn_install(&swap.staged, &plan(app.id())).unwrap();
    assert!(swap.staged.target.install_pending());
    swap.staged.target.clean_leftovers();
    assert!(swap.staged.staged_app.exists());
    app.wait().unwrap();
    assert!(script.wait().unwrap().success());
    assert!(!swap.staged.target.install_pending());
}

// ---------------------------------------------------------------------------------------------
// The whole flow against file:// URLs
// ---------------------------------------------------------------------------------------------

/// A fake release site: `releases.atom` plus the zip and `.sig` for `version`.
fn site(dir: &Path, keys: &Keys, version: &str, extra_entry: &str) -> String {
    let site = dir.join("site");
    let downloads = site.join(format!("releases/download/v{version}"));
    fs::create_dir_all(&downloads).unwrap();
    let zip = release_zip(dir, BUNDLE_ID, version, true);
    fs::copy(&zip, downloads.join(format!("Newjen-{version}-arm64.zip"))).unwrap();
    fs::write(
        downloads.join(format!("Newjen-{version}-arm64.zip.sig")),
        format!("{}\n", sign(keys, &zip)),
    )
    .unwrap();
    fs::write(
        site.join("releases.atom"),
        format!(
            r#"<feed xmlns="http://www.w3.org/2005/Atom"><updated>2026-11-03T00:00:00Z</updated>
<entry><updated>2026-11-03T00:00:00Z</updated>
<link rel="alternate" type="text/html" href="https://github.com/aelomi/newjen/releases/tag/v{version}"/></entry>
{extra_entry}</feed>"#
        ),
    )
    .unwrap();
    format!("file://{}", site.display())
}

fn collect_states(updater: &Updater, cancel: &AtomicBool) -> (UpdateState, Vec<UpdateState>) {
    let mut states = Vec::new();
    let last = updater.run_once(None, &mut |state| states.push(state), cancel);
    (last, states)
}

#[test]
fn run_once_downloads_verifies_stages_and_the_swap_installs_it() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    let updater = installed(&dir_path, &keys, &base);

    let (last, states) = collect_states(&updater, &AtomicBool::new(false));

    let UpdateState::Ready(staged) = &last else {
        panic!("expected Ready, got {last:?}");
    };
    assert_eq!(staged.version, v("0.2.0"));
    assert_eq!(states.first(), Some(&UpdateState::Checking));
    assert!(
        states.iter().any(
            |s| matches!(s, UpdateState::Downloading { version, .. } if *version == v("0.2.0"))
        )
    );
    assert!(
        states
            .iter()
            .any(|s| matches!(s, UpdateState::Preparing { .. }))
    );
    assert_eq!(states.last(), Some(&last));
    let final_progress = states.iter().rev().find_map(|state| match state {
        UpdateState::Downloading {
            received, total, ..
        } => Some((*received, *total)),
        _ => None,
    });
    let (received, total) = final_progress.unwrap();
    assert!(
        received > 0 && total == Some(received),
        "{received} of {total:?}"
    );

    spawn_install(staged, &plan(exited()))
        .unwrap()
        .wait()
        .unwrap();
    let plist = fs::read_to_string(updater.target().app().join("Contents/Info.plist")).unwrap();
    assert!(plist.contains("<string>0.2.0</string>"), "{plist}");
    assert!(stage_dir_gone(&updater, "0.2.0"));
}

#[test]
fn run_once_with_nothing_newer_is_up_to_date() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.1.0", "");
    let updater = installed(&dir_path, &keys, &base);
    let (last, states) = collect_states(&updater, &AtomicBool::new(false));
    assert!(
        matches!(last, UpdateState::UpToDate { checked_at } if checked_at > 0),
        "{last:?}"
    );
    assert_eq!(states.len(), 2);
}

#[test]
fn run_once_discards_a_download_with_a_bad_signature() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let (trusted, attacker) = (keys(), keys());
    let base = site(&dir_path, &attacker, "0.2.0", "");
    let updater = installed(&dir_path, &trusted, &base);
    let (last, _) = collect_states(&updater, &AtomicBool::new(false));
    assert!(
        matches!(last, UpdateState::Failed(UpdateError::Verification(_))),
        "{last:?}"
    );
    assert!(stage_dir_gone(&updater, "0.2.0"));
}

#[test]
fn run_once_says_an_unsigned_release_is_not_signed_yet() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    fs::remove_file(dir_path.join("site/releases/download/v0.2.0/Newjen-0.2.0-arm64.zip.sig"))
        .unwrap();
    let updater = installed(&dir_path, &keys, &base);
    let (last, _) = collect_states(&updater, &AtomicBool::new(false));
    assert_eq!(last, UpdateState::Failed(UpdateError::NotSigned));
    assert_eq!(
        UpdateError::NotSigned.to_string(),
        "The release isn't signed yet. Newjen will try again later."
    );
    assert!(stage_dir_gone(&updater, "0.2.0"));
}

#[test]
fn a_cancelled_download_leaves_nothing_and_returns_to_idle() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    let updater = installed(&dir_path, &keys, &base);
    let (last, _) = collect_states(&updater, &AtomicBool::new(true));
    assert_eq!(last, UpdateState::Idle);
    assert!(stage_dir_gone(&updater, "0.2.0"));
}

#[test]
fn an_unreachable_server_is_offline() {
    let dir = tempfile::tempdir().unwrap();
    let keys = keys();
    let updater = installed(dir.path(), &keys, "http://127.0.0.1:1");
    let (last, _) = collect_states(&updater, &AtomicBool::new(false));
    assert_eq!(last, UpdateState::Failed(UpdateError::Offline));
}

#[test]
fn an_update_that_cannot_be_installed_in_place_is_reported_with_its_version() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    let updater = installed(&dir_path, &keys, &base);
    let folder = updater.target().parent().to_path_buf();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555)).unwrap();
    let (last, _) = collect_states(&updater, &AtomicBool::new(false));
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        last,
        UpdateState::Blocked {
            version: v("0.2.0"),
            reason: UpdateError::CantWrite { folder },
        }
    );
}

#[test]
fn the_stage_folder_is_private_to_the_user() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    let updater = installed(&dir_path, &keys, &base);
    let (last, _) = collect_states(&updater, &AtomicBool::new(false));
    let UpdateState::Ready(staged) = last else {
        panic!("expected Ready");
    };
    let mode = fs::metadata(&staged.stage_dir)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
}

fn run_cancelling_at(updater: &Updater, cancel_when: impl Fn(&UpdateState) -> bool) -> UpdateState {
    let cancel = AtomicBool::new(false);
    updater.run_once(
        None,
        &mut |state| {
            if cancel_when(&state) {
                cancel.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        },
        &cancel,
    )
}

#[test]
fn cancelling_is_honoured_between_the_phases_not_only_during_the_download() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    let updater = installed(&dir_path, &keys, &base);

    let after_the_feed = run_cancelling_at(&updater, |state| *state == UpdateState::Checking);
    assert_eq!(after_the_feed, UpdateState::Idle);
    assert!(stage_dir_gone(&updater, "0.2.0"));

    let after_the_signature = run_cancelling_at(&updater, |state| {
        matches!(state, UpdateState::Downloading { received: 0, .. })
    });
    assert_eq!(after_the_signature, UpdateState::Idle);
    assert!(stage_dir_gone(&updater, "0.2.0"));

    let before_staging = run_cancelling_at(&updater, |state| {
        matches!(
            state,
            UpdateState::Downloading { received, total: Some(total), .. } if received == total
        )
    });
    assert_eq!(before_staging, UpdateState::Idle, "no Preparing, no Ready");
    assert!(stage_dir_gone(&updater, "0.2.0"));
}

#[test]
fn a_quiet_check_skips_a_version_that_already_failed_and_a_manual_one_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = fs::canonicalize(dir.path()).unwrap();
    let keys = keys();
    let base = site(&dir_path, &keys, "0.2.0", "");
    let updater = installed(&dir_path, &keys, &base);
    let run = |skip: Option<&Version>| updater.run_once(skip, &mut |_| {}, &AtomicBool::new(false));

    let skipped = run(Some(&v("0.2.0")));
    assert!(
        matches!(skipped, UpdateState::UpToDate { .. }),
        "{skipped:?}"
    );
    assert!(stage_dir_gone(&updater, "0.2.0"), "nothing was downloaded");
    let lower = run(Some(&v("0.1.5")));
    assert!(
        matches!(lower, UpdateState::Ready(_)),
        "newer than the skip"
    );
    let manual = run(None);
    assert!(
        matches!(manual, UpdateState::Ready(_)),
        "a manual check retries"
    );
}

#[test]
fn startup_reports_a_failed_install_once_and_keeps_nothing_else() {
    let swap = swap();
    let target = swap.staged.target.clone();
    let updater = Updater::new("file:///nowhere", v("0.1.0"), target.clone(), [1; 32]);
    fs::create_dir_all(target.new_path(&v("0.2.0"), 1)).unwrap();
    fs::write(
        target.result_path(),
        "version=0.2.0\nstatus=failed\nreason=Couldn't move it.\n",
    )
    .unwrap();

    let failure = updater.startup().expect("a failed install");
    assert_eq!(failure.version, v("0.2.0"));
    assert_eq!(
        failure.to_string(),
        "Couldn't install 0.2.0: Couldn't move it."
    );
    assert!(!target.result_path().exists());
    assert!(
        !target.new_path(&v("0.2.0"), 1).exists(),
        "leftovers are cleaned"
    );
    assert_eq!(updater.startup(), None, "told once");
}

#[test]
fn startup_ignores_successes_and_failures_for_versions_already_passed() {
    let swap = swap();
    let target = swap.staged.target.clone();
    let updater = Updater::new("file:///nowhere", v("0.2.0"), target.clone(), [1; 32]);
    for text in [
        "version=0.2.0\nstatus=ok\nreason=\n",
        "version=0.1.5\nstatus=failed\nreason=Old news.\n",
        "garbage",
    ] {
        fs::write(target.result_path(), text).unwrap();
        assert_eq!(updater.startup(), None, "{text}");
        assert!(!target.result_path().exists());
    }
}

/// Reads the real feed on github.com. Run with `cargo test -p newjen-core --test update -- --ignored`.
#[test]
#[ignore = "reads github.com"]
fn live_feed_lists_a_release_newer_than_an_ancient_version() {
    let dir = tempfile::tempdir().unwrap();
    let updater = installed(dir.path(), &keys(), newjen_core::update::DEFAULT_REPOSITORY);
    let updater = Updater::new(
        newjen_core::update::DEFAULT_REPOSITORY,
        v("0.0.1"),
        updater.target().clone(),
        [1; 32],
    );
    let release = updater.check().unwrap().expect("a published release");
    assert!(release.version >= v("0.1.0-alpha.1"));
    assert_eq!(release.tag, format!("v{}", release.version));
}
