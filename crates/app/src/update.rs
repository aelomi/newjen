//! In-app updates, app side: one model that every window reads. It owns the state of the
//! update flow ([`UpdateState`]), runs the quiet hourly check on the background executor, and
//! holds the notes to show after an update. `newjen_core::update` does the blocking work behind
//! [`UpdateSource`], so tests and the debug scenarios swap in a scripted source.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, AppContext as _, Context, Entity, Global, Task};
use newjen_core::Store;
use newjen_core::changelog::{self, ReleaseNotes, WhatsNew};
use newjen_core::update::{
    self, InstallFailure, StagedUpdate, Unavailable, UpdateError, UpdateState, Updater,
    running_version,
};
use semver::Version;

pub trait UpdateSource: Send + Sync + 'static {
    fn releases_page(&self) -> String;
    /// What to do at launch: reads how the last install ended and cleans up after it.
    fn startup(&self) -> Option<InstallFailure>;
    fn run_once(
        &self,
        skip: Option<&Version>,
        report: &mut dyn FnMut(UpdateState),
        cancel: &AtomicBool,
    ) -> UpdateState;
    fn install(
        &self,
        staged: &StagedUpdate,
        relaunch: bool,
        projects: &[PathBuf],
    ) -> Result<(), UpdateError>;
}

impl UpdateSource for Updater {
    fn releases_page(&self) -> String {
        Updater::releases_page(self)
    }

    fn startup(&self) -> Option<InstallFailure> {
        Updater::startup(self)
    }

    fn run_once(
        &self,
        skip: Option<&Version>,
        report: &mut dyn FnMut(UpdateState),
        cancel: &AtomicBool,
    ) -> UpdateState {
        Updater::run_once(self, skip, report, cancel)
    }

    fn install(
        &self,
        staged: &StagedUpdate,
        relaunch: bool,
        projects: &[PathBuf],
    ) -> Result<(), UpdateError> {
        update::install(staged, relaunch, projects)
    }
}

/// The notes the What's New sheet shows, and where the rest of the history lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhatsNewSheet {
    pub notes: Option<ReleaseNotes>,
    pub skipped: Vec<Version>,
    pub releases_url: String,
}

pub struct UpdateModel {
    source: Option<Arc<dyn UpdateSource>>,
    current: Option<Version>,
    state: UpdateState,
    run: Option<Arc<AtomicBool>>,
    rerun: bool,
    whats_new: Option<WhatsNew>,
    card_dismissed: bool,
    store: Option<Store>,
    installed: bool,
    ready_notes: Option<ReleaseNotes>,
    /// The last install that did not finish, until the user puts it away.
    install_failure: Option<InstallFailure>,
    failure_announced: bool,
    /// The quiet hourly check leaves this version and everything below it alone: it failed to
    /// install, or to download, and trying again until the feed offers something newer would
    /// only repeat the failure. A check the user asks for ignores it.
    skip: Option<Version>,
    /// Unix milliseconds when the app started, so a project the first launch opens itself
    /// does not make a fresh install look used.
    launched_at: i64,
    _quiet_checks: Option<Task<()>>,
}

pub struct UpdateHandle(pub Entity<UpdateModel>);

impl Global for UpdateHandle {}

pub fn handle(cx: &App) -> Option<Entity<UpdateModel>> {
    cx.try_global::<UpdateHandle>()
        .map(|handle| handle.0.clone())
}

/// The suffix the sidebar pill shows: `alpha.2` for `0.1.0-alpha.2`, the whole version for a
/// stable release.
pub fn short_version(version: &Version) -> String {
    if version.pre.is_empty() {
        version.to_string()
    } else {
        version.pre.to_string()
    }
}

/// The notes of the update's own version, from the CHANGELOG.md that `bundle-app.sh` puts in
/// its Resources folder. A release made before that has none, and the sheets leave notes out.
fn notes_from_bundle(staged: &StagedUpdate) -> Option<ReleaseNotes> {
    let text =
        std::fs::read_to_string(staged.staged_app.join("Contents/Resources/CHANGELOG.md")).ok()?;
    changelog::Changelog::parse(&text)
        .notes_for(&staged.version)
        .cloned()
}

/// Makes the app's update model, starts its quiet checks and shares it with every window.
pub fn install(store: &Store, cx: &mut App) {
    #[cfg(debug_assertions)]
    if let Ok(scenario) = std::env::var("NEWJEN_UPDATE_DEBUG") {
        let model = cx.new(|_| scenarios::model(&scenario));
        cx.set_global(UpdateHandle(model));
        return;
    }
    let (source, current) = match Updater::from_env() {
        Ok(updater) => {
            let current = updater.current_version().clone();
            let source: Arc<dyn UpdateSource> = Arc::new(updater);
            (Ok(source), Some(current))
        }
        Err(unavailable) => (Err(unavailable), running_version()),
    };
    let store = store.clone();
    let model = cx.new(|cx| {
        let mut model = UpdateModel::new(source, current, Some(store));
        model.start(cx);
        model
    });
    cx.set_global(UpdateHandle(model));
}

impl UpdateModel {
    /// Reads nothing from disk; [`UpdateModel::start`] does that off the main thread.
    pub fn new(
        source: Result<Arc<dyn UpdateSource>, Unavailable>,
        current: Option<Version>,
        store: Option<Store>,
    ) -> Self {
        let (source, state) = match source {
            Ok(source) => (Some(source), UpdateState::Idle),
            Err(unavailable) => (None, UpdateState::Unavailable(unavailable)),
        };
        Self {
            source,
            current,
            state,
            run: None,
            rerun: false,
            whats_new: None,
            card_dismissed: false,
            store,
            installed: false,
            ready_notes: None,
            install_failure: None,
            failure_announced: false,
            skip: None,
            launched_at: newjen_core::store::now_ms(),
            _quiet_checks: None,
        }
    }

    /// Reads how the last install ended, cleans up after it and loads what's new, all on the
    /// background executor. Then waits [`update::FIRST_CHECK`] and checks every
    /// [`update::CHECK_INTERVAL`], never while a download or a ready update exists.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        let source = self.source.clone();
        let store = self.store.clone();
        let current = self.current.clone();
        let launched_at = self.launched_at;
        let launch = cx.background_spawn(async move {
            let failure = source.as_ref().and_then(|source| source.startup());
            let (Some(store), Some(current)) = (store, current) else {
                return (failure, None, None);
            };
            if let Some(failure) = &failure {
                store.set_failed_update_version(&failure.version);
            }
            let news = changelog::whats_new_on_launch(&store, &current, launched_at);
            let failed = store.failed_update_version().ok().flatten();
            (failure, news, failed)
        });
        self._quiet_checks = Some(cx.spawn(async move |this, cx| {
            let (failure, news, failed) = launch.await;
            let checks = this.update(cx, |model, cx| {
                model.whats_new = news;
                model.install_failure = failure;
                model.skip = failed;
                cx.notify();
                model.source.is_some()
            });
            if !checks.unwrap_or(false) {
                return;
            }
            cx.background_executor().timer(update::FIRST_CHECK).await;
            loop {
                if this.update(cx, |model, cx| model.quiet_check(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(update::CHECK_INTERVAL).await;
            }
        }));
    }

    #[cfg(debug_assertions)]
    pub fn with_whats_new(mut self, news: WhatsNew) -> Self {
        self.whats_new = Some(news);
        self
    }

    pub fn state(&self) -> &UpdateState {
        &self.state
    }

    pub fn current_version(&self) -> Option<&Version> {
        self.current.as_ref()
    }

    pub fn ready(&self) -> Option<&StagedUpdate> {
        match &self.state {
            UpdateState::Ready(staged) => Some(staged),
            _ => None,
        }
    }

    /// The release notes of the ready update, read from the bundle it brings.
    pub fn ready_notes(&self) -> Option<&ReleaseNotes> {
        self.ready_notes.as_ref()
    }

    /// The notes to offer in the sidebar. A ready update takes the place first.
    pub fn card(&self) -> Option<&WhatsNew> {
        self.whats_new
            .as_ref()
            .filter(|_| !self.card_dismissed && self.ready().is_none())
    }

    pub fn releases_page(&self) -> String {
        self.source.as_ref().map_or_else(
            || format!("{}/releases", update::DEFAULT_REPOSITORY),
            |source| source.releases_page(),
        )
    }

    /// What Help › What's New in Newjen shows: the offered notes while they are unread, else
    /// the notes of the version you run (the newest released one in a build with no version).
    pub fn whats_new_sheet(&self) -> WhatsNewSheet {
        let releases_url = self.releases_page();
        if let Some(news) = self.whats_new.as_ref().filter(|_| !self.card_dismissed) {
            return WhatsNewSheet {
                notes: Some(news.notes.clone()),
                skipped: news.skipped.clone(),
                releases_url,
            };
        }
        let notes = self
            .current
            .as_ref()
            .and_then(changelog::notes_for)
            .or_else(changelog::newest_released_notes);
        WhatsNewSheet {
            notes,
            skipped: Vec::new(),
            releases_url,
        }
    }

    /// The sheet for the ready update's notes. Reading it does not mark anything as seen.
    pub fn incoming_sheet(&self) -> WhatsNewSheet {
        WhatsNewSheet {
            notes: self.ready_notes.clone(),
            skipped: Vec::new(),
            releases_url: self.releases_page(),
        }
    }

    /// The install that did not finish, until it is put away.
    pub fn install_failure(&self) -> Option<&InstallFailure> {
        self.install_failure.as_ref()
    }

    /// The failed install, once: the window opens its sheet for it the first time this is
    /// asked.
    pub fn take_failure_notice(&mut self) -> Option<InstallFailure> {
        if self.failure_announced {
            return None;
        }
        let failure = self.install_failure.clone()?;
        self.failure_announced = true;
        Some(failure)
    }

    pub fn dismiss_failure(&mut self, cx: &mut Context<Self>) {
        self.install_failure = None;
        cx.notify();
    }

    /// Check for Updates… Starts a check unless one is under way. A check the user cancelled
    /// is still finishing, so the new one waits for it.
    pub fn check_now(&mut self, cx: &mut Context<Self>) {
        if self.source.is_none() {
            return;
        }
        if self.cancelled() {
            self.rerun = true;
            self.set_state(UpdateState::Checking, None, cx);
            return;
        }
        if !self.working() && self.run.is_none() {
            self.start_run(None, cx);
        }
    }

    /// Cancel on the Checking sheet. The download stops, nothing is staged, and the result of
    /// a check that was already under way says nothing.
    pub fn cancel(&mut self) {
        if let Some(run) = &self.run {
            run.store(true, Ordering::SeqCst);
        }
    }

    /// Quitting: stops a check or download that is still running, and the programs it started,
    /// which would otherwise outlive the app.
    pub fn shutdown(&mut self) {
        self.cancel();
        update::kill_helpers();
    }

    pub fn dismiss_card(&mut self, cx: &mut Context<Self>) {
        self.card_dismissed = true;
        if let (Some(store), Some(current)) = (&self.store, &self.current) {
            changelog::mark_seen(store, current);
        }
        cx.notify();
    }

    /// Hands the ready update to the installer script, once. It waits for this process to exit.
    /// With `relaunch` the new version opens again on `projects`.
    pub fn install(&mut self, relaunch: bool, projects: &[PathBuf]) -> Result<(), UpdateError> {
        let (UpdateState::Ready(staged), Some(source)) = (&self.state, &self.source) else {
            return Ok(());
        };
        if self.installed {
            return Ok(());
        }
        source.install(staged, relaunch, projects)?;
        self.installed = true;
        Ok(())
    }

    fn working(&self) -> bool {
        matches!(
            self.state,
            UpdateState::Checking
                | UpdateState::Downloading { .. }
                | UpdateState::Preparing { .. }
                | UpdateState::Ready(_)
        )
    }

    fn cancelled(&self) -> bool {
        self.run
            .as_ref()
            .is_some_and(|run| run.load(Ordering::SeqCst))
    }

    fn quiet_check(&mut self, cx: &mut Context<Self>) {
        if !self.working() && self.run.is_none() {
            self.start_run(self.skip.clone(), cx);
        }
    }

    fn set_state(
        &mut self,
        state: UpdateState,
        ready_notes: Option<ReleaseNotes>,
        cx: &mut Context<Self>,
    ) {
        if self.state != state {
            if matches!(state, UpdateState::Ready(_)) {
                self.ready_notes = ready_notes;
                #[cfg(debug_assertions)]
                if self.ready_notes.is_none() && std::env::var_os("NEWJEN_UPDATE_DEBUG").is_some() {
                    self.ready_notes = Some(scenarios::notes());
                }
            }
            self.state = state;
            cx.notify();
        }
    }

    /// Runs one check on the background executor. `skip` is the quiet check's version to leave
    /// alone. A panic in the work ends as a failed check and never reaches the app.
    fn start_run(&mut self, skip: Option<Version>, cx: &mut Context<Self>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.run = Some(cancel.clone());
        self.set_state(UpdateState::Checking, None, cx);
        let (reports, received) = async_channel::unbounded();
        let work = cx.background_spawn(async move {
            let mut attempting: Option<Version> = None;
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                source.run_once(
                    skip.as_ref(),
                    &mut |state| {
                        if let UpdateState::Downloading { version, .. }
                        | UpdateState::Preparing { version } = &state
                        {
                            attempting = Some(version.clone());
                        }
                        let _ = reports.try_send(state);
                    },
                    &cancel,
                )
            }));
            let last = outcome.unwrap_or_else(|_| {
                let failed = UpdateState::Failed(UpdateError::Other(
                    "Newjen's updater ran into an unexpected problem. It will try again later."
                        .into(),
                ));
                let _ = reports.try_send(failed.clone());
                failed
            });
            (last, attempting)
        });
        cx.spawn(async move |this, cx| {
            while let Ok(mut latest) = received.recv().await {
                while let Ok(newer) = received.try_recv() {
                    latest = newer;
                }
                let notes = match &latest {
                    UpdateState::Ready(staged) => {
                        let staged = staged.clone();
                        cx.background_executor()
                            .spawn(async move { notes_from_bundle(&staged) })
                            .await
                    }
                    _ => None,
                };
                if this
                    .update(cx, |model, cx| model.set_state(latest, notes, cx))
                    .is_err()
                {
                    return;
                }
            }
            let (last, attempting) = work.await;
            let _ = this.update(cx, |model, cx| model.run_finished(last, attempting, cx));
        })
        .detach();
    }

    /// A finished run. A download or check that failed for a reason trying again won't change
    /// puts its version on the skip list for the quiet check. No network and no signature yet
    /// are not that: the next hourly check tries again.
    fn run_finished(
        &mut self,
        last: UpdateState,
        attempting: Option<Version>,
        cx: &mut Context<Self>,
    ) {
        self.run = None;
        if let (UpdateState::Failed(error), Some(version)) = (&last, attempting)
            && !matches!(error, UpdateError::Offline | UpdateError::NotSigned)
            && self.skip.as_ref().is_none_or(|skip| *skip < version)
        {
            self.skip = Some(version);
        }
        if std::mem::take(&mut self.rerun) && !self.working() {
            self.start_run(None, cx);
        }
    }
}

/// Test and debug-build sources: a scripted updater that never touches the network or the
/// installed app.
#[cfg(debug_assertions)]
pub mod scripted {
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    use newjen_core::update::InstallTarget;

    use super::*;

    pub fn staged(version: &str) -> StagedUpdate {
        let target =
            InstallTarget::from_exe(Path::new("/Applications/Newjen.app/Contents/MacOS/newjen"))
                .expect("a bundle path");
        StagedUpdate {
            version: version.parse().expect("a version"),
            staged_app: PathBuf::from("/nonexistent/Newjen.app"),
            stage_dir: PathBuf::from("/nonexistent"),
            target,
        }
    }

    /// Each call of `run_once` replays the next list of states, one per `step`, and returns
    /// the last. When the lists run out the last one repeats.
    pub struct Scripted {
        runs: Mutex<VecDeque<Vec<UpdateState>>>,
        last: Mutex<Vec<UpdateState>>,
        step: Duration,
        installs: Mutex<Vec<(Version, bool, Vec<PathBuf>)>>,
        skips: Mutex<Vec<Option<Version>>>,
        startup: Mutex<Option<InstallFailure>>,
        ignore_cancel: bool,
        panic_in_run: bool,
        pub runs_started: Mutex<usize>,
    }

    impl Scripted {
        pub fn new(runs: Vec<Vec<UpdateState>>, step: Duration) -> Arc<Self> {
            Arc::new(Self::build(runs, step))
        }

        fn build(runs: Vec<Vec<UpdateState>>, step: Duration) -> Self {
            Self {
                runs: Mutex::new(runs.into()),
                last: Mutex::new(vec![UpdateState::Idle]),
                step,
                installs: Mutex::new(Vec::new()),
                skips: Mutex::new(Vec::new()),
                startup: Mutex::new(None),
                ignore_cancel: false,
                panic_in_run: false,
                runs_started: Mutex::new(0),
            }
        }

        /// A source that finishes its work even when it is cancelled, as a download that was
        /// already past its last cancel check does.
        #[cfg(test)]
        pub fn ignoring_cancel(runs: Vec<Vec<UpdateState>>, step: Duration) -> Arc<Self> {
            Arc::new(Self {
                ignore_cancel: true,
                ..Self::build(runs, step)
            })
        }

        /// A source whose work panics.
        #[cfg(test)]
        pub fn panicking() -> Arc<Self> {
            Arc::new(Self {
                panic_in_run: true,
                ..Self::build(Vec::new(), Duration::ZERO)
            })
        }

        #[cfg(test)]
        pub fn with_startup_failure(self: Arc<Self>, failure: InstallFailure) -> Arc<Self> {
            *self.startup.lock().unwrap() = Some(failure);
            self
        }

        #[cfg(test)]
        pub fn installs(&self) -> Vec<(Version, bool, Vec<PathBuf>)> {
            self.installs.lock().unwrap().clone()
        }

        #[cfg(test)]
        pub fn skips(&self) -> Vec<Option<Version>> {
            self.skips.lock().unwrap().clone()
        }
    }

    impl UpdateSource for Scripted {
        fn releases_page(&self) -> String {
            "https://example.test/newjen/releases".into()
        }

        fn startup(&self) -> Option<InstallFailure> {
            self.startup.lock().unwrap().take()
        }

        fn run_once(
            &self,
            skip: Option<&Version>,
            report: &mut dyn FnMut(UpdateState),
            cancel: &AtomicBool,
        ) -> UpdateState {
            *self.runs_started.lock().unwrap() += 1;
            self.skips.lock().unwrap().push(skip.cloned());
            assert!(!self.panic_in_run, "the scripted updater panicked");
            let script = {
                let mut runs = self.runs.lock().unwrap();
                let mut last = self.last.lock().unwrap();
                if let Some(next) = runs.pop_front() {
                    *last = next;
                }
                last.clone()
            };
            let mut final_state = UpdateState::Idle;
            for state in script {
                if !self.step.is_zero() {
                    std::thread::sleep(self.step);
                }
                if !self.ignore_cancel && cancel.load(Ordering::SeqCst) {
                    report(UpdateState::Idle);
                    return UpdateState::Idle;
                }
                report(state.clone());
                final_state = state;
            }
            final_state
        }

        fn install(
            &self,
            staged: &StagedUpdate,
            relaunch: bool,
            projects: &[PathBuf],
        ) -> Result<(), UpdateError> {
            self.installs.lock().unwrap().push((
                staged.version.clone(),
                relaunch,
                projects.to_vec(),
            ));
            Ok(())
        }
    }
}

/// `NEWJEN_UPDATE_DEBUG=<words>` in a debug build shows the update states without a release:
/// `ready`, `whats-new`, `skipped`, and for what Check for Updates… does `uptodate`,
/// `offline`, `walk` (checking, downloading, ready), `cantwrite`, `translocated`. The
/// installer is never run.
#[cfg(debug_assertions)]
mod scenarios {
    use std::time::Duration;

    use super::scripted::{Scripted, staged};
    use super::*;

    pub(super) fn notes() -> ReleaseNotes {
        let mut notes = changelog::bundled()
            .releases()
            .first()
            .cloned()
            .expect("a changelog section");
        notes.version = "0.1.0-alpha.2".parse().ok();
        notes.label = "0.1.0-alpha.2".into();
        notes.date = Some("3 November 2026".into());
        notes
    }

    pub(super) fn model(scenario: &str) -> UpdateModel {
        let has = |word: &str| scenario.split([',', ' ']).any(|part| part == word);
        let current: Version = "0.1.0-alpha.2".parse().expect("a version");
        let next: Version = "0.1.0-alpha.3".parse().expect("a version");
        let walk = (0..=8)
            .map(|step| UpdateState::Downloading {
                version: next.clone(),
                received: step * 1_850_000,
                total: Some(14_800_000),
            })
            .chain([
                UpdateState::Preparing {
                    version: next.clone(),
                },
                UpdateState::Ready(staged("0.1.0-alpha.3")),
            ])
            .collect::<Vec<_>>();
        let run = if has("walk") {
            walk
        } else if has("offline") {
            vec![UpdateState::Failed(UpdateError::Offline)]
        } else if has("cantwrite") {
            vec![UpdateState::Blocked {
                version: next.clone(),
                reason: UpdateError::CantWrite {
                    folder: "/Applications".into(),
                },
            }]
        } else if has("translocated") {
            vec![UpdateState::Blocked {
                version: next.clone(),
                reason: UpdateError::Translocated,
            }]
        } else {
            vec![UpdateState::UpToDate {
                checked_at: newjen_core::store::now_ms(),
            }]
        };
        let source: Arc<dyn UpdateSource> = Scripted::new(vec![run], Duration::from_millis(900));
        let mut model = UpdateModel::new(Ok(source), Some(current), None);
        if has("ready") {
            model.state = UpdateState::Ready(staged("0.1.0-alpha.3"));
            model.ready_notes = Some(notes());
        }
        if has("whats-new") || has("skipped") {
            let skipped = if has("skipped") {
                vec![
                    "0.1.0-alpha.1".parse().expect("a version"),
                    "0.1.0-alpha.0".parse().expect("a version"),
                ]
            } else {
                Vec::new()
            };
            model = model.with_whats_new(WhatsNew {
                notes: notes(),
                skipped,
            });
        }
        model
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::TestAppContext;
    use newjen_core::update::InstallFailure;

    use super::scripted::{Scripted, staged};
    use super::*;

    fn up_to_date() -> UpdateState {
        UpdateState::UpToDate { checked_at: 1 }
    }

    fn model(cx: &mut TestAppContext, source: Arc<Scripted>) -> Entity<UpdateModel> {
        let source: Arc<dyn UpdateSource> = source;
        cx.new(|_| UpdateModel::new(Ok(source), Some("0.1.0-alpha.2".parse().unwrap()), None))
    }

    #[gpui::test]
    async fn a_check_walks_to_up_to_date(cx: &mut TestAppContext) {
        let source = Scripted::new(vec![vec![up_to_date()]], Duration::ZERO);
        let model = model(cx, source);
        model.update(cx, |model, cx| model.check_now(cx));
        assert_eq!(
            model.read_with(cx, |m, _| m.state().clone()),
            UpdateState::Checking
        );
        cx.run_until_parked();
        assert_eq!(model.read_with(cx, |m, _| m.state().clone()), up_to_date());
    }

    #[gpui::test]
    async fn a_check_that_finds_a_release_ends_ready(cx: &mut TestAppContext) {
        let ready = UpdateState::Ready(staged("0.1.0-alpha.3"));
        let source = Scripted::new(vec![vec![ready.clone()]], Duration::ZERO);
        let model = model(cx, source);
        model.update(cx, |model, cx| model.check_now(cx));
        cx.run_until_parked();
        assert_eq!(model.read_with(cx, |m, _| m.state().clone()), ready);
        assert!(model.read_with(cx, |m, _| m.ready().is_some()));
    }

    #[gpui::test]
    async fn quiet_checks_start_after_the_first_wait_and_never_over_a_ready_update(
        cx: &mut TestAppContext,
    ) {
        let source = Scripted::new(
            vec![
                vec![up_to_date()],
                vec![UpdateState::Ready(staged("0.1.0-alpha.3"))],
            ],
            Duration::ZERO,
        );
        let model = model(cx, source.clone());
        model.update(cx, |model, cx| model.start(cx));
        cx.run_until_parked();
        assert_eq!(*source.runs_started.lock().unwrap(), 0);
        cx.executor().advance_clock(update::FIRST_CHECK);
        cx.run_until_parked();
        assert_eq!(*source.runs_started.lock().unwrap(), 1);
        cx.executor().advance_clock(update::CHECK_INTERVAL);
        cx.run_until_parked();
        assert!(model.read_with(cx, |m, _| m.ready().is_some()));
        assert_eq!(*source.runs_started.lock().unwrap(), 2);
        cx.executor().advance_clock(update::CHECK_INTERVAL * 3);
        cx.run_until_parked();
        assert_eq!(
            *source.runs_started.lock().unwrap(),
            2,
            "a ready update is left alone"
        );
    }

    #[gpui::test]
    async fn install_runs_once_with_the_relaunch_choice(cx: &mut TestAppContext) {
        let source = Scripted::new(
            vec![vec![UpdateState::Ready(staged("0.1.0-alpha.3"))]],
            Duration::ZERO,
        );
        let model = model(cx, source.clone());
        model.update(cx, |model, cx| model.check_now(cx));
        cx.run_until_parked();
        let projects = vec![PathBuf::from("/work/one"), PathBuf::from("/work/two")];
        model.update(cx, |model, _| {
            model.install(true, &projects).unwrap();
            model.install(false, &[]).unwrap();
        });
        assert_eq!(
            source.installs(),
            vec![("0.1.0-alpha.3".parse().unwrap(), true, projects)]
        );
    }

    #[gpui::test]
    async fn a_dev_build_is_unavailable_and_never_checks(cx: &mut TestAppContext) {
        let model = cx.new(|_| UpdateModel::new(Err(Unavailable::DevBuild), None, None));
        model.update(cx, |model, cx| model.check_now(cx));
        cx.run_until_parked();
        assert_eq!(
            model.read_with(cx, |m, _| m.state().clone()),
            UpdateState::Unavailable(Unavailable::DevBuild)
        );
    }

    fn failing_download(version: &str, error: UpdateError) -> Vec<UpdateState> {
        vec![
            UpdateState::Downloading {
                version: version.parse().unwrap(),
                received: 1,
                total: Some(2),
            },
            UpdateState::Failed(error),
        ]
    }

    async fn pump(cx: &mut TestAppContext, model: &Entity<UpdateModel>) {
        model.update(cx, |model, cx| model.check_now(cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    async fn a_download_that_failed_is_not_tried_again_by_the_quiet_check(cx: &mut TestAppContext) {
        let source = Scripted::new(
            vec![
                failing_download(
                    "0.1.0-alpha.3",
                    UpdateError::Verification("The update's signature doesn't match.".into()),
                ),
                vec![up_to_date()],
            ],
            Duration::ZERO,
        );
        let model = model(cx, source.clone());
        pump(cx, &model).await;
        model.update(cx, |model, cx| model.quiet_check(cx));
        cx.run_until_parked();
        pump(cx, &model).await;
        assert_eq!(
            source.skips(),
            vec![None, Some("0.1.0-alpha.3".parse().unwrap()), None],
            "the user's own check ignores the skip"
        );
    }

    #[gpui::test]
    async fn offline_and_unsigned_failures_are_tried_again(cx: &mut TestAppContext) {
        for error in [UpdateError::Offline, UpdateError::NotSigned] {
            let source = Scripted::new(
                vec![failing_download("0.1.0-alpha.3", error), vec![up_to_date()]],
                Duration::ZERO,
            );
            let model = model(cx, source.clone());
            pump(cx, &model).await;
            model.update(cx, |model, cx| model.quiet_check(cx));
            cx.run_until_parked();
            assert_eq!(source.skips(), vec![None, None]);
        }
    }

    #[gpui::test]
    async fn a_finished_ready_update_is_never_wiped_by_a_rerun(cx: &mut TestAppContext) {
        let ready = UpdateState::Ready(staged("0.1.0-alpha.3"));
        let source = Scripted::ignoring_cancel(vec![vec![ready.clone()]], Duration::ZERO);
        let model = model(cx, source.clone());
        model.update(cx, |model, cx| {
            model.check_now(cx);
            model.cancel();
            model.check_now(cx);
        });
        cx.run_until_parked();
        assert_eq!(model.read_with(cx, |m, _| m.state().clone()), ready);
        assert_eq!(
            *source.runs_started.lock().unwrap(),
            1,
            "no second download"
        );
    }

    #[gpui::test]
    async fn a_cancelled_check_followed_by_another_runs_again(cx: &mut TestAppContext) {
        let source = Scripted::new(vec![vec![up_to_date()]], Duration::from_millis(1));
        let model = model(cx, source.clone());
        model.update(cx, |model, cx| {
            model.check_now(cx);
            model.cancel();
            model.check_now(cx);
        });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(*source.runs_started.lock().unwrap(), 2);
        assert_eq!(model.read_with(cx, |m, _| m.state().clone()), up_to_date());
    }

    #[gpui::test]
    async fn a_panic_in_the_work_is_a_failed_check_not_a_crash(cx: &mut TestAppContext) {
        let source = Scripted::panicking();
        let model = model(cx, source.clone());
        pump(cx, &model).await;
        let state = model.read_with(cx, |m, _| m.state().clone());
        assert!(
            matches!(state, UpdateState::Failed(UpdateError::Other(_))),
            "{state:?}"
        );
        pump(cx, &model).await;
        assert_eq!(*source.runs_started.lock().unwrap(), 2, "checks still work");
    }

    #[gpui::test]
    async fn a_failed_install_is_announced_once_and_remembered(cx: &mut TestAppContext) {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in(home.path()).unwrap();
        let failure = InstallFailure {
            version: "0.1.0-alpha.3".parse().unwrap(),
            reason: "Couldn't put the new version in place.".into(),
        };
        let source = Scripted::new(vec![vec![up_to_date()]], Duration::ZERO)
            .with_startup_failure(failure.clone());
        let model = {
            let source: Arc<dyn UpdateSource> = source.clone();
            cx.new(|_| {
                UpdateModel::new(
                    Ok(source),
                    Some("0.1.0-alpha.2".parse().unwrap()),
                    Some(store.clone()),
                )
            })
        };
        model.update(cx, |model, cx| model.start(cx));
        cx.run_until_parked();
        assert_eq!(
            model.update(cx, |m, _| m.take_failure_notice()),
            Some(failure.clone())
        );
        assert_eq!(model.update(cx, |m, _| m.take_failure_notice()), None);
        assert_eq!(
            model.read_with(cx, |m, _| m.install_failure().cloned()),
            Some(failure)
        );
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(
            store.failed_update_version().unwrap(),
            Some("0.1.0-alpha.3".parse().unwrap())
        );

        cx.executor().advance_clock(update::FIRST_CHECK);
        cx.run_until_parked();
        assert_eq!(
            source.skips(),
            vec![Some("0.1.0-alpha.3".parse().unwrap())],
            "the quiet check leaves the version that failed to install"
        );
    }

    #[gpui::test]
    async fn whats_new_is_read_off_the_main_thread_and_an_old_data_folder_gets_it(
        cx: &mut TestAppContext,
    ) {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open_in(home.path()).unwrap();
        store
            .open_project(std::path::Path::new("/tmp/used-before"))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let source: Arc<dyn UpdateSource> = Scripted::new(vec![], Duration::ZERO);
        let model = cx.new(|_| {
            UpdateModel::new(
                Ok(source),
                Some("0.1.0-alpha.2".parse().unwrap()),
                Some(store),
            )
        });
        assert!(
            model.read_with(cx, |m, _| m.card().is_none()),
            "nothing is read in new()"
        );
        model.update(cx, |model, cx| model.start(cx));
        cx.run_until_parked();
        let card = model.read_with(cx, |m, _| m.card().map(|news| news.notes.label.clone()));
        assert_eq!(card.as_deref(), Some("0.1.0-alpha.2"));
    }

    #[test]
    fn the_pill_shows_the_prerelease_suffix() {
        assert_eq!(short_version(&"0.1.0-alpha.2".parse().unwrap()), "alpha.2");
        assert_eq!(short_version(&"0.2.0".parse().unwrap()), "0.2.0");
    }
}
