use super::changes::{FileRow, counts, file_row, files_label, totals};
use super::git::{self, GitAction, GitFacts, Job, Plan};
use super::*;
use gpui::{KeyDownEvent, WeakEntity};
use gpui_kit::component::button::DropdownButton;
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable as _};
use newjen_core::edits::{EditPreview, ReadEdit};
use newjen_core::vcs::{self, Scope};
use newjen_core::worktree::Worktree;

pub(super) struct ApprovalPreview {
    request: newjen_core::ApprovalKey,
    diff: Option<Entity<DiffState>>,
    message: String,
    expected: Option<ReadEdit>,
    validating: bool,
}

impl ApprovalPreview {
    fn reading(request: newjen_core::ApprovalKey) -> Self {
        Self {
            request,
            diff: None,
            message: "Reading the proposed edit…".into(),
            expected: None,
            validating: false,
        }
    }

    pub(super) fn loading(&self) -> bool {
        self.expected.is_none() || self.validating
    }

    /// The proposed edit as read from the disk: a diff to show, or why there is none. An
    /// oversized or unavailable preview still allows once, as the message says.
    fn fill(&mut self, read: ReadEdit, cx: &mut App) {
        self.expected = Some(read.clone());
        match read.preview {
            EditPreview::Patch(patch) => match DiffFile::parse(&patch) {
                Ok(files) => {
                    self.diff = Some(cx.new(|cx| DiffState::new(files, cx)));
                    self.message.clear();
                }
                Err(error) => self.message = format!("Couldn't display the proposed edit: {error}"),
            },
            EditPreview::TooLarge => {
                self.message = "This edit is too large to preview. Allow once is the only \
                                available approval."
                    .into()
            }
            EditPreview::Unavailable(reason) => self.message = reason,
        }
    }

    pub(super) fn render(&self, height: f32, cx: &App) -> AnyElement {
        match &self.diff {
            Some(diff) => div()
                .h(px((height - 390.).clamp(64., 150.)))
                .w_full()
                .child(Diff::new(diff).soft_wrap(true))
                .into_any_element(),
            None => ui::caption(self.message.clone(), cx).into_any_element(),
        }
    }
}

/// Which diff the panel shows and what git says about the thread's folder.
pub(super) struct ReviewPanel {
    thread: String,
    scope: Scope,
    truncated: bool,
    /// The patch split by file, in patch order, and the row each file has in the list.
    files: Vec<DiffFile>,
    rows: Vec<FileRow>,
    selected: usize,
    /// Shows the selected file only.
    diff: Entity<DiffState>,
    facts: Option<GitFacts>,
    /// A diff is being read; only the newest read (`generation`) is kept.
    loading: bool,
    generation: u64,
    /// The git action that is running.
    running: Option<GitAction>,
    reverting: bool,
    message: Option<PanelMessage>,
    sheet: Option<CommitSheet>,
}

struct PanelMessage {
    text: String,
    ok: bool,
    /// A pull request to open from the message.
    link: Option<String>,
}

impl PanelMessage {
    fn error(text: String) -> Self {
        Self {
            text,
            ok: false,
            link: None,
        }
    }

    fn done(text: String) -> Self {
        Self {
            text,
            ok: true,
            link: None,
        }
    }

    fn link(mut self, link: Option<String>) -> Self {
        self.link = link;
        self
    }
}

struct CommitSheet {
    action: GitAction,
    field: Entity<TextareaState>,
    /// The message is being written.
    writing: bool,
}

/// What one read of the repository found.
pub(super) struct Loaded {
    pub diff: vcs::Diff,
    pub facts: Option<GitFacts>,
}

impl Loaded {
    /// Reads the diff for `scope` and the facts the git button needs. Runs git, so never on
    /// the UI thread.
    fn read(
        tools: &Tools,
        id: &str,
        cwd: &Path,
        worktree: Option<&Worktree>,
        scope: Scope,
    ) -> anyhow::Result<Loaded> {
        Ok(Loaded {
            diff: vcs::diff(tools, cwd, id, scope)?,
            facts: GitFacts::load(tools, id, cwd, worktree).ok(),
        })
    }
}

impl ReviewPanel {
    pub(super) fn busy(&self) -> bool {
        self.running.is_some() || self.reverting
    }

    fn new(thread: String, scope: Scope, cx: &mut App) -> Self {
        Self {
            diff: cx.new(|cx| DiffState::new(Vec::new(), cx)),
            thread,
            scope,
            truncated: false,
            files: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            facts: None,
            loading: false,
            generation: 0,
            running: None,
            reverting: false,
            message: None,
            sheet: None,
        }
    }

    /// Takes a read of the repository. The file that was selected stays selected if it is
    /// still in the diff, otherwise `wanted` or the first.
    fn fill(
        &mut self,
        loaded: Loaded,
        wanted: Option<String>,
        cx: &mut Context<ChatView>,
    ) -> Result<(), String> {
        let files = if loaded.diff.truncated {
            Vec::new()
        } else {
            DiffFile::parse(&loaded.diff.patch).map_err(|error| error.to_string())?
        };
        let keep = wanted.or_else(|| {
            self.files
                .get(self.selected)
                .map(|file| file.path().to_string())
        });
        let selected = keep
            .and_then(|path| files.iter().position(|file| file.path().as_ref() == path))
            .unwrap_or(0);
        let shown: Vec<DiffFile> = files.get(selected).cloned().into_iter().collect();
        self.diff = cx.new(|cx| DiffState::new(shown, cx));
        self.rows = files.iter().map(FileRow::from_diff).collect();
        self.files = files;
        self.selected = selected;
        self.truncated = loaded.diff.truncated;
        if loaded.facts.is_some() {
            self.facts = loaded.facts;
        }
        Ok(())
    }

    fn empty_text(&self) -> String {
        if self.truncated {
            return "The diff is too large to show.".into();
        }
        match self.scope {
            Scope::All => "No changes.".into(),
            Scope::Uncommitted => "Nothing uncommitted.".into(),
            Scope::Turn(turn) => format!("Turn {turn} changed nothing."),
        }
    }
}

fn scope_label(scope: Scope) -> String {
    match scope {
        Scope::All => "All changes".into(),
        Scope::Uncommitted => "Uncommitted".into(),
        Scope::Turn(turn) => format!("Turn {turn}"),
    }
}

fn working(thread: &Thread) -> bool {
    is_busy(thread.status())
        || thread.preparing().is_some()
        || thread.work_pending()
        || !thread.background_tasks().is_empty()
}

impl ChatView {
    /// The dock's answer. An allowed file edit is first checked against the file as it is
    /// now: if the file changed since the preview was made, the preview is made again and
    /// nothing is sent (see `edits::EditProposal::preview`).
    pub(super) fn answer(
        &mut self,
        request_id: Option<newjen_core::ApprovalKey>,
        choice: ApprovalIntent,
        rule_cut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        if choice.is_deny() || self.edit_preview.is_none() {
            self.send_answer(request_id, choice, rule_cut, window, cx);
            return;
        }
        let preview = self.edit_preview.as_mut().unwrap();
        if preview.loading() || request_id.is_some_and(|id| id != preview.request) {
            return;
        }
        let expected = preview.expected.clone().unwrap();
        let request = preview.request;
        preview.validating = true;
        let Some(proposal) = self.thread().read(cx).pending_edit() else {
            return;
        };
        let task = cx.background_spawn(async move { proposal.read() });
        cx.spawn_in(window, async move |view, cx| {
            let current = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                if view.quit_saving {
                    return;
                }
                let Some(preview) = view
                    .edit_preview
                    .as_mut()
                    .filter(|preview| preview.request == request)
                else {
                    return;
                };
                preview.validating = false;
                if current == expected {
                    view.send_answer(Some(request), choice, true, window, cx);
                } else {
                    view.prepare_approval(window, cx);
                    view.slots[view.active].notice = Some(Notice::held(
                        "The file changed",
                        "Review the updated edit before allowing it.".into(),
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn prepare_approval(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.denial_note = Some(cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 2)
                .placeholder("Optional reason for denying…")
        }));
        self.edit_preview = None;
        let thread = self.thread().read(cx);
        let Some(request) = thread.transcript().pending_approval() else {
            return;
        };
        let Some(proposal) = thread.pending_edit() else {
            return;
        };
        let request = request.key();
        self.edit_preview = Some(ApprovalPreview::reading(request));
        let task = cx.background_spawn(async move { proposal.read() });
        cx.spawn_in(window, async move |view, cx| {
            let preview = task.await;
            let _ = view.update_in(cx, |view, _, cx| {
                let Some(current) = view
                    .edit_preview
                    .as_mut()
                    .filter(|current| current.request == request)
                else {
                    return;
                };
                current.fill(preview, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn render_edit_preview(&self, height: f32, cx: &App) -> Option<AnyElement> {
        Some(self.edit_preview.as_ref()?.render(height, cx))
    }

    pub(super) fn run_snapshot(
        &mut self,
        thread: &Entity<Thread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        let job = thread.update(cx, |thread, _| thread.take_snapshot_job());
        let Some(job) = job else {
            return;
        };
        let tools = self.tools.clone();
        let task = cx.background_spawn(async move { job.run(&tools) });
        let thread = thread.downgrade();
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let mut finished = None;
            let _ = thread.update(cx, |thread, cx| {
                thread.finish_snapshot(result);
                finished = Some(thread.id().to_string());
                cx.notify();
            });
            if let Some(id) = finished {
                let _ = view.update_in(cx, |view, window, cx| view.review_follows(&id, window, cx));
            }
        })
        .detach();
    }

    pub(super) fn prepare_project(
        &mut self,
        thread: &Entity<Thread>,
        job: thread::ProjectJob,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tools = self.tools.clone();
        let task = cx.background_spawn(async move { job.run(&tools) });
        let thread = thread.downgrade();
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let Some(thread) = thread.upgrade() else {
                return;
            };
            let _ = view.update_in(cx, |view, window, cx| {
                if view.quit_saving {
                    return;
                }
                if thread.read(cx).preparing().is_none() {
                    return;
                }
                let result = thread.update(cx, |thread, cx| {
                    let result = thread.finish_project(result);
                    cx.notify();
                    result
                });
                if let Err(error) = result
                    && let Some(ix) = view.slots.iter().position(|slot| slot.thread == thread)
                {
                    view.slots[ix].notice = Some(Notice::held(
                        "Couldn't prepare the project",
                        error.error.to_string(),
                    ));
                    if ix == view.active {
                        let field = view.ensure_field(window, cx);
                        field.update(cx, |field, cx| field.set_value(error.message, window, cx));
                    }
                }
                view.sync_field(window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.review_loading || self.review_panel.as_ref().is_some_and(ReviewPanel::busy) {
            return;
        }
        let thread = self.thread().read(cx);
        let held = if thread.preparing().is_some() || thread.work_pending() {
            Some(Notice::held(
                "Wait for the thread",
                "Its snapshot must finish before you review changes.".into(),
            ))
        } else if thread.copy().is_some_and(Worktree::is_legacy) {
            Some(Notice::held(
                "Review it in your terminal",
                "This thread works in a Rift copy from an older Newjen, which can't show its changes."
                    .into(),
            ))
        } else if thread.review_base().is_none() {
            Some(Notice::held(
                "Changes unavailable",
                "Changes need a git repository and a thread that has started.".into(),
            ))
        } else {
            None
        };
        if let Some(held) = held {
            self.slots[self.active].notice = Some(held);
            cx.notify();
            return;
        }
        let id = thread.id().to_string();
        let cwd = thread.cwd().to_path_buf();
        let worktree = thread.copy().cloned();
        let tools = self.tools.clone();
        self.review_loading = true;
        let read_id = id.clone();
        let task = cx.background_spawn(async move {
            Loaded::read(&tools, &read_id, &cwd, worktree.as_ref(), Scope::All)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.review_loading = false;
                if view.quit_saving {
                    return;
                }
                match result {
                    Ok(loaded) => view.show_review(id, Scope::All, loaded, window, cx),
                    Err(error) => {
                        view.slots[view.active].notice =
                            Some(Notice::held("Couldn't load changes", format!("{error:#}")))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Puts the review panel for thread `id` on screen. The panel replaces the composer, so
    /// the root takes the keyboard: ⌘1–9 and the other shortcuts keep working while the
    /// diff is open.
    pub(super) fn show_review(
        &mut self,
        id: String,
        scope: Scope,
        loaded: Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.take_pending_file();
        let mut panel = ReviewPanel::new(id, scope, cx);
        match panel.fill(loaded, target, cx) {
            Ok(()) => {
                self.review_panel = Some(panel);
                window.focus(&self.root, cx);
            }
            Err(error) => {
                self.slots[self.active].notice =
                    Some(Notice::held("Couldn't display the diff", error))
            }
        }
    }

    /// Reads the repository again for the panel on screen, in `scope` if one is given. Only
    /// the newest read is kept, and a failed one puts the old scope back.
    pub(super) fn reload_review(
        &mut self,
        scope: Option<Scope>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        let Some(panel) = self.review_panel.as_mut().filter(|panel| !panel.busy()) else {
            return;
        };
        let id = panel.thread.clone();
        let previous = panel.scope;
        panel.scope = scope.unwrap_or(previous);
        panel.generation += 1;
        panel.loading = true;
        let (generation, scope) = (panel.generation, panel.scope);
        let Some(ix) = self.index_of(&id, cx) else {
            return;
        };
        let thread = self.slots[ix].thread.read(cx);
        let cwd = thread.cwd().to_path_buf();
        let worktree = thread.copy().cloned();
        let tools = self.tools.clone();
        let read_id = id.clone();
        let task = cx.background_spawn(async move {
            Loaded::read(&tools, &read_id, &cwd, worktree.as_ref(), scope)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, _, cx| {
                if view.quit_saving {
                    return;
                }
                let Some(panel) = view
                    .review_panel
                    .as_mut()
                    .filter(|panel| panel.thread == id && panel.generation == generation)
                else {
                    return;
                };
                panel.loading = false;
                let failure = match result {
                    Ok(loaded) => panel.fill(loaded, None, cx).err(),
                    Err(error) => Some(format!("{error:#}")),
                };
                if let Some(error) = failure {
                    panel.scope = previous;
                    panel.message = Some(PanelMessage::error(format!(
                        "Couldn't load changes: {error}"
                    )));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn select_scope(
        &mut self,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .review_panel
            .as_ref()
            .is_some_and(|panel| panel.scope != scope && !panel.busy())
        {
            self.reload_review(Some(scope), window, cx);
        }
    }

    /// A turn of thread `id` just finished and was checkpointed.
    fn review_follows(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .review_panel
            .as_ref()
            .is_some_and(|panel| panel.thread == id)
        {
            self.reload_review(None, window, cx);
        }
    }

    /// The selected row, how many rows there are, and the paths the diff shows.
    #[cfg(test)]
    pub(super) fn review_shown(&self, cx: &App) -> Option<(usize, usize, Vec<String>)> {
        let panel = self.review_panel.as_ref()?;
        let shown = panel
            .diff
            .read(cx)
            .files()
            .iter()
            .map(|file| file.path().to_string())
            .collect();
        Some((panel.selected, panel.rows.len(), shown))
    }

    /// The label of the git button's main action, or why it has none.
    #[cfg(test)]
    pub(super) fn review_primary(&self) -> Option<Result<String, &'static str>> {
        let panel = self.review_panel.as_ref()?;
        let facts = panel.facts.as_ref()?;
        let target = target_name(panel);
        Some(match git::plan(facts) {
            Plan::Actions(actions) => Ok(actions[0].label(&target)),
            Plan::Idle(reason) => Err(reason),
        })
    }

    /// The text of the strip under the panel's header, and whether it reports success.
    #[cfg(test)]
    pub(super) fn review_message_text(&self) -> Option<(String, bool)> {
        let message = self.review_panel.as_ref()?.message.as_ref()?;
        Some((message.text.clone(), message.ok))
    }

    /// Whether the commit message is still being written, and its field.
    #[cfg(test)]
    pub(super) fn review_sheet(&self) -> Option<(bool, Entity<TextareaState>)> {
        let sheet = self.review_panel.as_ref()?.sheet.as_ref()?;
        Some((sheet.writing, sheet.field.clone()))
    }

    #[cfg(test)]
    pub(super) fn review_turns(&self) -> usize {
        self.review_panel
            .as_ref()
            .and_then(|panel| panel.facts.as_ref())
            .map_or(0, |facts| facts.turns)
    }

    #[cfg(test)]
    pub(super) fn review_settled(&self) -> bool {
        self.review_panel
            .as_ref()
            .is_some_and(|panel| !panel.loading && !panel.busy())
    }

    pub(super) fn select_review_file(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(panel) = self.review_panel.as_mut() else {
            return;
        };
        if ix >= panel.files.len() || ix == panel.selected {
            return;
        }
        panel.selected = ix;
        let file = panel.files[ix].clone();
        panel.diff.update(cx, |diff, cx| diff.set_files([file], cx));
        cx.notify();
    }

    /// Why git actions can't run now. A commit takes every file in the folder, so it must not
    /// run while this thread, or another thread working in the same folder, is writing files.
    pub(super) fn git_gate(&self, id: &str, cx: &App) -> Option<&'static str> {
        let Some(ix) = self.index_of(id, cx) else {
            return Some("This thread is gone");
        };
        let target = &self.slots[ix].thread;
        if working(target.read(cx)) {
            return Some("Wait for the turn to finish");
        }
        let mut roots = vec![worktree::project_root(target.read(cx).cwd())];
        roots.extend(target.read(cx).copy().map(|copy| copy.source.clone()));
        let shared = |view: &ChatView| {
            view.slots.iter().any(|slot| {
                slot.thread != *target && {
                    let other = slot.thread.read(cx);
                    working(other) && roots.contains(&worktree::project_root(other.cwd()))
                }
            })
        };
        (shared(self)
            || self
                .copy_peers
                .iter()
                .filter_map(|peer| peer.upgrade())
                .any(|peer| shared(peer.read(cx))))
        .then_some("Another thread is working in this folder")
    }

    /// ⌘↵ in the review: the git button's main action.
    pub(super) fn run_primary_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let primary = self
            .review_panel
            .as_ref()
            .and_then(|panel| panel.facts.as_ref())
            .and_then(|facts| git::plan(facts).primary());
        if let Some(action) = primary {
            self.start_git_action(action, window, cx);
        }
    }

    pub(super) fn start_git_action(
        &mut self,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        let Some(panel) = self
            .review_panel
            .as_ref()
            .filter(|panel| !panel.busy() && panel.facts.is_some())
        else {
            return;
        };
        if let Some(reason) = self.git_gate(&panel.thread, cx) {
            self.review_panel.as_mut().expect("checked above").message =
                Some(PanelMessage::error(reason.to_string()));
            cx.notify();
            return;
        }
        if action.commits() {
            self.open_commit_sheet(action, window, cx);
        } else {
            self.run_git_action(action, String::new(), window, cx);
        }
    }

    /// Asks for the commit message. The user's own `claude` writes it from the diff when the
    /// user clicks a commit action; the thread's title stands in if that fails. Nothing is
    /// committed until the user confirms.
    fn open_commit_sheet(
        &mut self,
        action: GitAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.review_panel.as_ref() else {
            return;
        };
        let id = panel.thread.clone();
        let Some(ix) = self.index_of(&id, cx) else {
            return;
        };
        let field = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(4, 10)
                .placeholder("Commit message")
        });
        self.review_panel.as_mut().expect("checked above").sheet = Some(CommitSheet {
            action,
            field: field.clone(),
            writing: true,
        });
        let thread = self.slots[ix].thread.read(cx);
        let cwd = thread.cwd().to_path_buf();
        let title = thread.title().to_string();
        let tools = self.tools.clone();
        let claude = self.base_config.program.clone();
        let task = cx.background_spawn(async move {
            let patch = vcs::uncommitted_patch(&tools, &cwd, &id)?;
            anyhow::ensure!(git::installed(&claude), "claude is not installed");
            newjen_core::commit_message::generate(&claude, &cwd, &patch.patch)
        });
        let watched = field.clone();
        cx.spawn_in(window, async move |view, cx| {
            let written = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                let Some(sheet) = view
                    .review_panel
                    .as_mut()
                    .and_then(|panel| panel.sheet.as_mut())
                    .filter(|sheet| sheet.field == watched)
                else {
                    return;
                };
                sheet.writing = false;
                if watched.read(cx).value().trim().is_empty() {
                    let message = written.unwrap_or(title);
                    watched.update(cx, |field, cx| field.set_value(message, window, cx));
                }
                window.refresh();
                cx.notify();
            });
        })
        .detach();
        let view = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, cx| commit_dialog(dialog, &view, cx));
        let handle = field.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    pub(super) fn confirm_commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self
            .review_panel
            .as_mut()
            .and_then(|panel| panel.sheet.take())
        else {
            return;
        };
        let message = sheet.field.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.review_panel.as_mut().expect("had a sheet").sheet = Some(sheet);
            return;
        }
        self.run_git_action(sheet.action, message, window, cx);
    }

    fn close_commit_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.review_panel.as_mut() {
            panel.sheet = None;
        }
        cx.notify();
    }

    fn run_git_action(
        &mut self,
        action: GitAction,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        let Some(panel) = self.review_panel.as_mut().filter(|panel| !panel.busy()) else {
            return;
        };
        let id = panel.thread.clone();
        let Some(ix) = self.index_of(&id, cx) else {
            return;
        };
        let panel = self.review_panel.as_mut().expect("checked above");
        let target = panel
            .facts
            .as_ref()
            .and_then(|facts| facts.source.as_ref()?.branch.clone())
            .unwrap_or_else(|| "the project".into());
        panel.running = Some(action);
        panel.message = None;
        panel.sheet = None;
        let thread = self.slots[ix].thread.clone();
        let completion = thread.update(cx, |thread, _| thread.track_background_work());
        let job = {
            let thread = thread.read(cx);
            Job {
                tools: self.tools.clone(),
                gh: vcs::find_gh(),
                cwd: thread.cwd().to_path_buf(),
                worktree: thread.copy().cloned(),
                title: thread.title().to_string(),
            }
        };
        let task = cx.background_spawn(async move {
            let result = job.run(action, &message);
            let _ = completion.send(());
            result
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish_git_action(&id, action, &target, result, window, cx)
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_git_action(
        &mut self,
        id: &str,
        action: GitAction,
        target: &str,
        result: Result<Option<String>, git::Failed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self
            .review_panel
            .as_mut()
            .filter(|panel| panel.thread == id)
        {
            panel.running = None;
            panel.message = Some(match &result {
                Ok(url) => {
                    let (title, detail) = action.done(target, url.as_deref());
                    PanelMessage::done(if detail.is_empty() {
                        title.to_string()
                    } else {
                        format!("{title} · {detail}")
                    })
                    .link(url.clone())
                }
                Err(failed) => {
                    PanelMessage::error(format!("{}: {:#}", failed.step.failed(), failed.error))
                }
            });
        }
        if let (Ok(url), Some(ix)) = (&result, self.index_of(id, cx)) {
            let (title, detail) = action.done(target, url.as_deref());
            self.slots[ix].notice = Some(Notice::done(title, detail));
        }
        self.reload_review(None, window, cx);
        cx.notify();
    }

    /// "Revert turn N" in a worktree thread, after the user confirms.
    fn revert_turn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.review_panel.as_ref().filter(|panel| !panel.busy()) else {
            return;
        };
        let Scope::Turn(turn) = panel.scope else {
            return;
        };
        let id = panel.thread.clone();
        if self.git_gate(&id, cx).is_some() {
            return;
        }
        let question = window.prompt(
            gpui::PromptLevel::Warning,
            &format!("Revert turn {turn}?"),
            Some(
                "The files in this worktree go back to how they were before the turn. \
                 Edits from later turns to those files are lost. Commits stay.",
            ),
            &["Cancel", "Revert turn"],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            if question.await.ok() != Some(1) {
                return;
            }
            let _ = view.update_in(cx, |view, window, cx| {
                view.perform_revert(&id, turn, window, cx)
            });
        })
        .detach();
    }

    fn perform_revert(
        &mut self,
        id: &str,
        turn: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving || self.git_gate(id, cx).is_some() {
            return;
        }
        let Some(ix) = self.index_of(id, cx) else {
            return;
        };
        let Some(worktree) = self.slots[ix]
            .thread
            .read(cx)
            .copy()
            .filter(|copy| !copy.is_legacy())
            .cloned()
        else {
            return;
        };
        let Some(panel) = self
            .review_panel
            .as_mut()
            .filter(|panel| panel.thread == id && !panel.busy())
        else {
            return;
        };
        panel.reverting = true;
        panel.message = None;
        let completion = self.slots[ix]
            .thread
            .update(cx, |thread, _| thread.track_background_work());
        let tools = self.tools.clone();
        let thread_id = id.to_string();
        let task = cx.background_spawn(async move {
            let result = vcs::revert_turn(&tools, &worktree, &thread_id, turn);
            let _ = completion.send(());
            result
        });
        let id = id.to_string();
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                if let Some(panel) = view
                    .review_panel
                    .as_mut()
                    .filter(|panel| panel.thread == id)
                {
                    panel.reverting = false;
                    panel.message = Some(match &result {
                        Ok(()) => PanelMessage::done(format!("Reverted turn {turn}")),
                        Err(error) => {
                            PanelMessage::error(format!("Couldn't revert turn {turn}: {error:#}"))
                        }
                    });
                }
                view.reload_review(result.is_ok().then_some(Scope::All), window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn render_review_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let thread = self.thread().read(cx);
        if thread.is_draft() {
            return div().into_any_element();
        }
        h_flex()
            .px_4()
            .gap_2()
            .children(
                (self.changes.has_changes
                    && !self.changes.on_screen
                    && self.review_panel.is_none())
                .then(|| {
                    ui::button(
                        "review-work",
                        if self.review_loading {
                            "Loading changes…"
                        } else {
                            "Review changes"
                        },
                        ui::Look::Ghost,
                        ui::Size::Sm,
                        cx,
                    )
                    .on_click(cx.listener(|view, _, window, cx| view.open_review(window, cx)))
                }),
            )
            .children(
                thread
                    .snapshot_waits_for_background()
                    .then(|| ui::caption("Stop background tasks to save this turn.", cx)),
            )
            .children(
                thread
                    .snapshot_error()
                    .map(|error| ui::caption(format!("Snapshot failed: {error}"), cx)),
            )
            .children(thread.snapshot_error().map(|_| {
                ui::button(
                    "retry-snapshot",
                    "Retry snapshot",
                    ui::Look::Ghost,
                    ui::Size::Sm,
                    cx,
                )
                .on_click(cx.listener(|view, _, window, cx| {
                    let thread = view.thread().clone();
                    thread.update(cx, |thread, _| thread.retry_snapshot());
                    view.run_snapshot(&thread, window, cx);
                    cx.notify();
                }))
            }))
            .into_any_element()
    }

    fn scope_button(&self, panel: &ReviewPanel, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.weak_entity();
        let turns = panel.facts.as_ref().map_or(0, |facts| facts.turns);
        let current = panel.scope;
        ui::button(
            "review-scope",
            scope_label(current),
            ui::Look::Secondary,
            ui::Size::Sm,
            cx,
        )
        .dropdown_caret(true)
        .disabled(panel.busy())
        .dropdown_menu(move |menu, _, _| {
            let item = |label: String, scope: Scope| {
                let view = view.clone();
                PopupMenuItem::new(label)
                    .checked(scope == current)
                    .on_click(move |_, window, cx| {
                        let _ = view.update(cx, |view, cx| view.select_scope(scope, window, cx));
                    })
            };
            let mut menu = menu
                .min_w(px(190.))
                .max_h(px(320.))
                .scrollable(true)
                .item(item(scope_label(Scope::All), Scope::All))
                .item(item(scope_label(Scope::Uncommitted), Scope::Uncommitted));
            if turns > 0 {
                menu = menu.separator();
                for turn in 1..=turns {
                    menu = menu.item(item(scope_label(Scope::Turn(turn)), Scope::Turn(turn)));
                }
            }
            menu
        })
        .into_any_element()
    }

    /// The git button: the plan's first action with the rest in its menu.
    fn git_button(
        &self,
        panel: &ReviewPanel,
        gate: Option<&'static str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(action) = panel.running {
            let target = target_name(panel);
            return ui::button_with_icon(
                "git-action",
                format!("{}…", action.label(&target)),
                action.icon(),
                ui::Look::Primary,
                ui::Size::Sm,
                cx,
            )
            .loading(true)
            .disabled(true)
            .into_any_element();
        }
        let idle = |label: &str, cx: &mut Context<Self>| {
            ui::button(
                "git-action",
                label.to_string(),
                ui::Look::Secondary,
                ui::Size::Sm,
                cx,
            )
            .disabled(true)
            .into_any_element()
        };
        let Some(facts) = panel.facts.as_ref() else {
            return idle(
                if panel.loading {
                    "Checking…"
                } else {
                    "Git unavailable"
                },
                cx,
            );
        };
        let actions = match git::plan(facts) {
            Plan::Idle(reason) => return idle(reason, cx),
            Plan::Actions(actions) => actions,
        };
        let target = target_name(panel);
        let main = actions[0];
        let mut primary = ui::button_with_icon(
            "git-primary",
            main.label(&target),
            main.icon(),
            ui::Look::Primary,
            ui::Size::Sm,
            cx,
        )
        .child(ui::key_caps("cmd-enter", cx))
        .disabled(gate.is_some())
        .on_click(cx.listener(move |view, _, window, cx| view.start_git_action(main, window, cx)));
        if let Some(reason) = gate {
            primary = primary.tooltip(reason);
        }
        if actions.len() == 1 {
            return primary.into_any_element();
        }
        let view = cx.weak_entity();
        DropdownButton::new("git-actions")
            .button(primary)
            .disabled(gate.is_some())
            .menu_accessibility_label("Choose a git action")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(240.));
                for &action in &actions {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(action.label(&target))
                            .icon(action.icon())
                            .on_click(move |_, window, cx| {
                                let _ = view.update(cx, |view, cx| {
                                    view.start_git_action(action, window, cx)
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    pub(super) fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.review_panel.as_ref().unwrap();
        let colors = cx.theme().colors;
        let stroke = crate::theme::material(cx).card_stroke;
        let (added, removed) = totals(&panel.rows);
        let gate = self.git_gate(&panel.thread, cx);
        let in_worktree = self.index_of(&panel.thread, cx).is_some_and(|ix| {
            self.slots[ix]
                .thread
                .read(cx)
                .copy()
                .is_some_and(|copy| !copy.is_legacy())
        });

        let back = ui::icon_button(
            "close-review",
            ui::IconName::ArrowLeft,
            "Back to thread",
            cx,
        )
        .debug_selector(|| "close-review".to_string())
        .on_click(cx.listener(|view, _, window, cx| {
            if !view.review_panel.as_ref().is_some_and(ReviewPanel::busy) {
                view.review_panel = None;
                view.focus_panel(window, cx);
                cx.notify();
            }
        }));
        let revert = match panel.scope {
            Scope::Turn(turn) if in_worktree => Some(
                ui::button(
                    "revert-turn",
                    format!("Revert turn {turn}"),
                    ui::Look::Ghost,
                    ui::Size::Sm,
                    cx,
                )
                .text_color(colors.danger)
                .disabled(gate.is_some() || panel.busy())
                .on_click(cx.listener(|view, _, window, cx| view.revert_turn(window, cx))),
            ),
            _ => None,
        };

        let top = h_flex()
            .flex_none()
            .h(px(46.))
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(stroke)
            .child(back)
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Changes"),
            )
            .child(self.scope_button(panel, cx))
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(files_label(panel.rows.len())),
            )
            .child(counts(added, removed, false, cx))
            .child(div().flex_1())
            .children(revert)
            .child(self.git_button(panel, gate, cx));

        let list = v_flex()
            .id("review-files")
            .flex_none()
            .w(px(250.))
            .h_full()
            .p_2()
            .gap_0p5()
            .border_r_1()
            .border_color(stroke)
            .overflow_y_scroll()
            .children(panel.rows.iter().enumerate().map(|(ix, row)| {
                file_row(("review-file", ix), row, ix == panel.selected, cx)
                    .on_click(cx.listener(move |view, _, _, cx| view.select_review_file(ix, cx)))
            }));
        let diff = if panel.files.is_empty() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(ui::caption(panel.empty_text(), cx))
                .into_any_element()
        } else {
            div()
                .debug_selector(|| "review-diff".to_string())
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(Diff::new(&panel.diff).soft_wrap(true))
                .into_any_element()
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .child(top)
            .children(self.review_message(panel, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(v_flex().flex_1().min_w_0().min_h_0().h_full().child(diff)),
            )
            .into_any_element()
    }

    /// What the last git action or read said: green when it worked, red when it didn't.
    fn review_message(&self, panel: &ReviewPanel, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = panel.message.as_ref()?;
        let theme = cx.theme();
        let tone = if message.ok {
            theme.success
        } else {
            theme.danger
        };
        let link = message.link.clone();
        Some(
            h_flex()
                .id("review-message")
                .flex_none()
                .max_h(px(120.))
                .overflow_y_scroll()
                .px_4()
                .py_2()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(crate::theme::material(cx).card_stroke)
                .bg(tone.opacity(0.1))
                .child(ui::icon(
                    if message.ok {
                        ui::IconName::Check
                    } else {
                        ui::IconName::CircleAlert
                    },
                    gpui_kit::component::Size::Small,
                    tone,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .child(message.text.clone()),
                )
                .children(link.map(|url| {
                    ui::button(
                        "open-pull-request",
                        "Open pull request",
                        ui::Look::Ghost,
                        ui::Size::Sm,
                        cx,
                    )
                    .on_click(move |_, _, cx| cx.open_url(&url))
                }))
                .child(
                    ui::button(
                        "dismiss-review-message",
                        "Dismiss",
                        ui::Look::Ghost,
                        ui::Size::Sm,
                        cx,
                    )
                    .on_click(cx.listener(|view, _, _, cx| {
                        if let Some(panel) = view.review_panel.as_mut() {
                            panel.message = None;
                        }
                        cx.notify();
                    })),
                )
                .into_any_element(),
        )
    }
}

fn target_name(panel: &ReviewPanel) -> String {
    panel
        .facts
        .as_ref()
        .and_then(|facts| facts.source.as_ref()?.branch.clone())
        .unwrap_or_else(|| "the project".into())
}

/// The commit sheet: the message, which the user may rewrite, and what happens after it.
/// Built again whenever the dialog layer is, so it reads its state from the panel.
fn commit_dialog(
    dialog: gpui_kit::component::dialog::Dialog,
    view: &WeakEntity<ChatView>,
    cx: &mut App,
) -> gpui_kit::component::dialog::Dialog {
    use gpui_kit::component::dialog::DialogEntrance;
    let Some(entity) = view.upgrade() else {
        return dialog;
    };
    let state = entity.read(cx);
    let Some((panel, sheet)) = state
        .review_panel
        .as_ref()
        .and_then(|panel| Some((panel, panel.sheet.as_ref()?)))
    else {
        return dialog;
    };
    let (action, writing, field) = (sheet.action, sheet.writing, sheet.field.clone());
    let target = target_name(panel);
    let branch = panel
        .facts
        .as_ref()
        .and_then(|facts| facts.status.branch.clone())
        .unwrap_or_default();
    let empty = field.read(cx).value().trim().is_empty();
    let then = action
        .steps()
        .iter()
        .skip(1)
        .map(|step| match step {
            git::Step::Commit => "commit",
            git::Step::Push => "push to origin",
            git::Step::PullRequest => "open a pull request",
            git::Step::Merge => "merge into the project",
        })
        .collect::<Vec<_>>();
    let then = (!then.is_empty()).then(|| format!("Then {}.", then.join(", ")));
    let muted = cx.theme().muted_foreground;
    let material = crate::theme::material(cx);
    let confirm = view.clone();
    let cancel = view.clone();
    let closed = view.clone();
    let enter = view.clone();
    dialog
        .centered()
        .width(px(520.))
        .bg(material.card)
        .entrance(DialogEntrance::None)
        .close_button(false)
        .on_close(move |_, _, cx| {
            let _ = closed.update(cx, |view, cx| view.close_commit_sheet(cx));
        })
        .title(
            div()
                .debug_selector(|| "commit-sheet".into())
                .child("Commit changes"),
        )
        .child(
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(ui::icon(
                            ui::IconName::GitBranch,
                            gpui_kit::component::Size::Small,
                            muted,
                        ))
                        .child(div().text_sm().text_color(muted).child(branch)),
                )
                .child(
                    div()
                        .debug_selector(|| "commit-message".into())
                        .capture_action(move |action: &gpui_kit::base::input::Enter, window, cx| {
                            if action.secondary {
                                cx.stop_propagation();
                                let _ =
                                    enter.update(cx, |view, cx| view.confirm_commit(window, cx));
                                window.close_dialog(cx);
                            }
                        })
                        .child(Textarea::new(&field).aria_label("Commit message")),
                )
                .when(writing, |column| {
                    column.child(
                        h_flex()
                            .debug_selector(|| "commit-writing".into())
                            .gap_2()
                            .items_center()
                            .child(
                                gpui_kit::component::spinner::Spinner::new()
                                    .small()
                                    .color(muted),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child("Writing a message…"),
                            ),
                    )
                })
                .children(then.map(|then| div().text_sm().text_color(muted).child(then))),
        )
        .footer(
            h_flex()
                .gap_2()
                .justify_end()
                .child(Button::new("commit-cancel").label("Cancel").on_click(
                    move |_, window, cx| {
                        let _ = cancel.update(cx, |view, cx| view.close_commit_sheet(cx));
                        window.close_dialog(cx);
                    },
                ))
                .child(
                    div().debug_selector(|| "commit-confirm".into()).child(
                        ui::button_with_icon(
                            "commit-confirm",
                            action.label(&target),
                            action.icon(),
                            ui::Look::Primary,
                            ui::Size::Md,
                            cx,
                        )
                        .disabled(empty)
                        .on_click(move |_, window, cx| {
                            let _ = confirm.update(cx, |view, cx| view.confirm_commit(window, cx));
                            window.close_dialog(cx);
                        }),
                    ),
                ),
        )
}

/// ⌘↵, pressed fresh: macOS repeats a held key, and a repeat must not run the action twice.
pub(super) fn is_primary_action_key(event: &KeyDownEvent) -> bool {
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    keystroke.key == "enter"
        && modifiers.platform
        && !(modifiers.control || modifiers.alt || modifiers.shift)
        && !event.is_held
}

#[cfg(test)]
mod tests {
    use super::*;
    use newjen_core::edits::{EditProposal, FileChange};

    fn press(keys: &'static str, is_held: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke::parse(keys).unwrap(),
            is_held,
            prefer_character_input: false,
        }
    }

    #[test]
    fn only_a_fresh_command_return_runs_the_primary_action() {
        assert!(is_primary_action_key(&press("cmd-enter", false)));
        assert!(!is_primary_action_key(&press("cmd-enter", true)));
        assert!(!is_primary_action_key(&press("enter", false)));
        assert!(!is_primary_action_key(&press("cmd-shift-enter", false)));
        assert!(!is_primary_action_key(&press("cmd-alt-enter", false)));
    }

    #[test]
    fn the_kit_parser_accepts_real_file_edit_previews() {
        let path = std::env::temp_dir().join(format!("newjen preview {}.txt", std::process::id()));
        std::fs::write(&path, "before\n").unwrap();
        for content in ["after\n", "no final newline", ""] {
            let proposal = EditProposal {
                path: path.clone(),
                change: FileChange::Write(content.into()),
            };
            let EditPreview::Patch(patch) = proposal.preview() else {
                panic!("patch");
            };
            let files = DiffFile::parse(&patch).unwrap();
            assert_eq!(files.len(), 1);
        }
        std::fs::remove_file(path).unwrap();
    }
}
