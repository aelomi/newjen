use super::changes::{FileRow, counts, file_row, files_label, totals};
use super::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::DropdownButton;
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use gpui_kit::component::menu::PopupMenuItem;
use newjen_core::edits::{EditPreview, ReadEdit};
use newjen_core::landing::{self, Integration, LandingMode, Review};

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

pub(super) struct ReviewPanel {
    thread: String,
    receipt: Review,
    /// The patch split by file, in patch order, and the row each file has in the list.
    files: Vec<DiffFile>,
    rows: Vec<FileRow>,
    selected: usize,
    /// Shows the selected file only.
    diff: Entity<DiffState>,
    pub(super) busy: bool,
    message: String,
}

struct Resolution {
    id: String,
    receipt: Review,
    copy: Option<rift::Copy>,
    cwd: PathBuf,
    mode: LandingMode,
    integration: Option<Integration>,
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
        cx.spawn_in(window, async move |_, cx| {
            let result = task.await;
            let _ = thread.update(cx, |thread, cx| {
                thread.finish_snapshot(result);
                cx.notify();
            });
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
        if self.review_loading || self.review_panel.as_ref().is_some_and(|panel| panel.busy) {
            return;
        }
        let thread = self.thread().read(cx);
        if is_busy(thread.status())
            || thread.preparing().is_some()
            || thread.work_pending()
            || !thread.background_tasks().is_empty()
        {
            self.slots[self.active].notice = Some(Notice::held(
                "Wait for the thread",
                "The turn and its jj snapshot must finish before review.".into(),
            ));
            cx.notify();
            return;
        }
        let Some(base) = thread.review_base().map(str::to_string) else {
            self.slots[self.active].notice = Some(Notice::held("Landing unavailable", "Landing currently requires a jj repository colocated with git and a thread started with review tracking.".into()));
            cx.notify();
            return;
        };
        let id = thread.id().to_string();
        let cwd = thread.cwd().to_path_buf();
        let mode = thread.landing_mode();
        let tools = self.tools.clone();
        self.review_loading = true;
        let stopping = self.thread().update(cx, |thread, _| thread.begin_quit());
        let completion = self
            .thread()
            .update(cx, |thread, _| thread.track_background_work());
        let store = self.store.clone();
        let task = cx.background_spawn(async move {
            let result = thread::finish_quit(&store, [stopping])
                .map_err(anyhow::Error::from)
                .and_then(|()| landing::review(&tools, &cwd, &base));
            let _ = completion.send(());
            result
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.review_loading = false;
                if view.quit_saving {
                    return;
                }
                match result {
                    Ok(receipt) => view.show_review(id, receipt, mode, window, cx),
                    Err(error) => {
                        view.slots[view.active].notice =
                            Some(Notice::held("Couldn't review work", format!("{error:#}")))
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Puts the review panel for thread `id` on screen with `receipt`'s diff. The panel
    /// replaces the composer, so the root takes the keyboard: ⌘1–9 and the other shortcuts
    /// keep working while the diff is open.
    pub(super) fn show_review(
        &mut self,
        id: String,
        receipt: Review,
        mode: LandingMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.take_pending_file();
        match if receipt.truncated {
            Ok(Vec::new())
        } else {
            DiffFile::parse(&receipt.patch)
        } {
            Ok(files) => {
                let message = if receipt.truncated { "The diff is too large to show. Landing is blocked." } else if mode == LandingMode::AgentManaged { "Only work recorded with jj is shown. Git commits are not imported; use jj and bookmark your work. The copy stays after landing." } else if receipt.patch.is_empty() { "No changes." } else { "" }.into();
                let selected = target
                    .and_then(|path| files.iter().position(|file| file.path().as_ref() == path))
                    .unwrap_or(0);
                let shown: Vec<DiffFile> = files.get(selected).cloned().into_iter().collect();
                self.review_panel = Some(ReviewPanel {
                    thread: id,
                    receipt,
                    rows: files.iter().map(FileRow::from_diff).collect(),
                    files,
                    selected,
                    diff: cx.new(|cx| DiffState::new(shown, cx)),
                    busy: false,
                    message,
                });
                window.focus(&self.root, cx);
            }
            Err(error) => {
                self.slots[self.active].notice =
                    Some(Notice::held("Couldn't display the diff", error.to_string()))
            }
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

    /// ⌘↵ in the review: land the way the Land button does.
    pub(super) fn land_with_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ready = self
            .review_panel
            .as_ref()
            .is_some_and(|panel| !panel.busy && !panel.receipt.truncated);
        if ready {
            self.resolve_review(false, Integration::Rebase, window, cx);
        }
    }

    fn resolve_review(
        &mut self,
        discard: bool,
        integration: Integration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.review_panel.as_ref() else {
            return;
        };
        if panel.busy {
            return;
        }
        let id = panel.thread.clone();
        let Some(ix) = self.index_of(&id, cx) else {
            return;
        };
        if self
            .slots
            .iter()
            .any(|slot| slot.thread.read(cx).blocks_copy_creation())
            || self.project_work_owned_elsewhere(&self.slots[ix].thread, cx)
        {
            self.review_panel.as_mut().unwrap().message =
                "Land, discard, or keep the files and close the project-folder thread before landing copy work.".into();
            cx.notify();
            return;
        }
        let thread = self.slots[ix].thread.read(cx);
        let receipt = panel.receipt.clone();
        let copy = thread.copy().cloned();
        let cwd = thread.cwd().to_path_buf();
        let mode = thread.landing_mode();
        let oversized_copy = copy.is_some() && receipt.truncated;
        let resolution = Resolution {
            id,
            receipt,
            copy,
            cwd,
            mode,
            integration: (!discard).then_some(integration),
        };
        if discard {
            let question = window.prompt(gpui::PromptLevel::Warning, "Discard this thread's work?", Some(if oversized_copy { "The diff is too large to display. This deletes the entire Rift copy, including changes you have not reviewed. The conversation is archived." } else { "This removes the Rift copy, or restores the project-folder work to this thread's base. The conversation is archived." }), &["Cancel", "Discard"], cx);
            cx.spawn_in(window, async move |view, cx| {
                if question.await.ok() == Some(1) {
                    let _ = view.update_in(cx, |view, window, cx| {
                        if view.quit_saving {
                            return;
                        }
                        view.perform_resolution(resolution, window, cx)
                    });
                }
            })
            .detach();
        } else {
            self.perform_resolution(resolution, window, cx);
        }
    }

    fn perform_resolution(
        &mut self,
        resolution: Resolution,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        let Resolution {
            id,
            receipt,
            copy,
            cwd,
            mode,
            integration,
        } = resolution;
        let Some(ix) = self.index_of(&id, cx) else {
            return;
        };
        let thread = self.slots[ix].thread.read(cx);
        if is_busy(thread.status())
            || thread.preparing().is_some()
            || thread.work_pending()
            || !thread.background_tasks().is_empty()
        {
            return;
        }
        let Some(panel) = self
            .review_panel
            .as_mut()
            .filter(|panel| panel.thread == id && !panel.busy)
        else {
            return;
        };
        panel.busy = true;
        panel.message = "Checking and applying work…".into();
        let completion = self.slots[ix]
            .thread
            .update(cx, |thread, _| thread.track_background_work());
        let tools = self.tools.clone();
        let task = cx.background_spawn(async move {
            match (integration, copy) {
                (Some(integration), Some(copy)) => {
                    landing::land_copy(&tools, &copy, mode, &id, integration, &receipt)
                }
                (Some(_), None) => landing::land_project(&tools, &cwd, mode, &id, &receipt),
                (None, Some(_)) => Ok(()),
                (None, None) => landing::discard_project(&tools, &cwd, mode, &id, &receipt),
            }
        });
        let id = self.review_panel.as_ref().unwrap().thread.clone();
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                match result {
                    Ok(()) => {
                        if let Some(ix) = view.index_of(&id, cx) {
                            view.slots[ix].thread.update(cx, |thread, cx| { thread.finish_landing(); cx.notify(); });
                            view.review_panel = None;
                            if view.quit_saving { cx.notify(); return; }
                            if integration.is_none() { view.archive(&id, window, cx); }
                            else {
                                view.slots[ix].notice = Some(Notice::held("Work landed", "The work is in the project. Archive the thread when you are ready to close it.".into()));
                                view.focus_panel(window, cx);
                            }
                        }
                    }
                    Err(error) => if let Some(panel) = view.review_panel.as_mut() { panel.busy = false; panel.message = format!("{error:#}"); },
                }
                cx.notify();
            });
            let _ = completion.send(());
        }).detach();
        cx.notify();
    }

    pub(super) fn keep_project_files_and_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_saving {
            return;
        }
        if self.review_loading || self.review_panel.as_ref().is_some_and(|panel| panel.busy) {
            return;
        }
        let thread = self.thread().clone();
        if !thread.read(cx).can_release_project_tracking() {
            return;
        }
        let id = thread.read(cx).id().to_string();
        let question = window.prompt(gpui::PromptLevel::Warning, "Keep files and close this thread?", Some("Newjen stops tracking this thread. Files and jj history stay as they are. Manage this work in your terminal. Other threads can then use the project folder."), &["Cancel", "Keep files and close"], cx);
        cx.spawn_in(window, async move |view, cx| {
            if question.await.ok() != Some(1) {
                return;
            }
            let task = view.update_in(cx, |view, _, cx| {
                if view.quit_saving {
                    return None;
                }
                let ix = view.index_of(&id, cx)?;
                let thread = view.slots[ix].thread.clone();
                if !thread.read(cx).can_release_project_tracking()
                    || view.review_loading
                    || view.review_panel.as_ref().is_some_and(|panel| panel.busy)
                {
                    return None;
                }
                view.review_loading = true;
                let stopping = thread.update(cx, |thread, _| thread.begin_quit());
                let completion = thread.update(cx, |thread, _| thread.track_background_work());
                let store = view.store.clone();
                cx.notify();
                Some(cx.background_spawn(async move {
                    let result = thread::finish_quit(&store, [stopping]);
                    let _ = completion.send(());
                    result
                }))
            });
            let Ok(Some(task)) = task else {
                return;
            };
            let saved = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.review_loading = false;
                if view.quit_saving {
                    return;
                }
                let Some(ix) = view.index_of(&id, cx) else {
                    return;
                };
                if let Err(error) = saved {
                    view.slots[ix].notice =
                        Some(Notice::held("Couldn't close the thread", error.to_string()));
                    cx.notify();
                    return;
                }
                let result = view.slots[ix].thread.update(cx, |thread, cx| {
                    let result = thread.release_project_tracking();
                    cx.notify();
                    result
                });
                match result {
                    Ok(()) => {
                        view.review_panel = None;
                        view.archive(&id, window, cx);
                    }
                    Err(error) => {
                        view.slots[ix].notice =
                            Some(Notice::held("Couldn't close the thread", error.to_string()))
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
                            "Loading diff…"
                        } else {
                            "Review work"
                        },
                        ui::Look::Ghost,
                        ui::Size::Sm,
                        cx,
                    )
                    .on_click(cx.listener(|view, _, window, cx| view.open_review(window, cx)))
                }),
            )
            .children(thread.can_release_project_tracking().then(|| {
                ui::button(
                    "keep-files-close",
                    "Keep files and close",
                    ui::Look::Ghost,
                    ui::Size::Sm,
                    cx,
                )
                .on_click(
                    cx.listener(|view, _, window, cx| {
                        view.keep_project_files_and_close(window, cx)
                    }),
                )
            }))
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

    pub(super) fn render_review(&self, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.review_panel.as_ref().unwrap();
        let colors = cx.theme().colors;
        let stroke = crate::theme::material(cx).card_stroke;
        let can_land = !panel.busy && !panel.receipt.truncated;
        let has_copy = self
            .index_of(&panel.thread, cx)
            .is_some_and(|ix| self.slots[ix].thread.read(cx).copy().is_some());
        let (added, removed) = totals(&panel.rows);

        let back = ui::icon_button(
            "close-review",
            ui::IconName::ArrowLeft,
            "Back to thread",
            cx,
        )
        .debug_selector(|| "close-review".to_string())
        .on_click(cx.listener(|view, _, window, cx| {
            if !view.review_panel.as_ref().is_some_and(|panel| panel.busy) {
                view.review_panel = None;
                view.focus_panel(window, cx);
                cx.notify();
            }
        }));
        let discard = ui::button("discard-work", "Discard", ui::Look::Ghost, ui::Size::Sm, cx)
            .text_color(colors.danger)
            .on_click(cx.listener(|view, _, window, cx| {
                view.resolve_review(true, Integration::Rebase, window, cx)
            }));
        let land = ui::button_with_icon(
            "land-rebase",
            "Land",
            ui::IconName::GitBranch,
            ui::Look::Primary,
            ui::Size::Sm,
            cx,
        )
        .child(ui::key_caps("cmd-enter", cx))
        .disabled(!can_land)
        .on_click(cx.listener(|view, _, window, cx| {
            view.land_with_key(window, cx);
        }));
        // Only a Rift copy has two ways to land; project-folder work is already in place.
        let land = if has_copy {
            let view = cx.weak_entity();
            DropdownButton::new("land")
                .button(land)
                .disabled(!can_land)
                .menu_accessibility_label("Choose how to land")
                .dropdown_menu(move |menu, _, _| {
                    let item = |label: &'static str, integration| {
                        let view = view.clone();
                        PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let _ =
                                view.update(cx, |view, cx| {
                                    if view.review_panel.as_ref().is_some_and(|panel| {
                                        !panel.busy && !panel.receipt.truncated
                                    }) {
                                        view.resolve_review(false, integration, window, cx)
                                    }
                                });
                        })
                    };
                    menu.item(item("Rebase and land", Integration::Rebase))
                        .item(item("Merge and land", Integration::Merge))
                })
                .into_any_element()
        } else {
            land.into_any_element()
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
                    .child("Review"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(files_label(panel.rows.len())),
            )
            .child(counts(added, removed, false, cx))
            .child(div().flex_1())
            .child(discard)
            .child(land);

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
                .child(ui::caption(
                    if panel.message.is_empty() {
                        "No changes.".to_string()
                    } else {
                        panel.message.clone()
                    },
                    cx,
                ))
                .into_any_element()
        } else {
            div()
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
            .children(
                (!panel.message.is_empty() && !panel.files.is_empty()).then(|| {
                    div()
                        .id("review-message")
                        .flex_none()
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(stroke)
                        .child(ui::caption(panel.message.clone(), cx))
                }),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(v_flex().flex_1().min_w_0().min_h_0().child(diff)),
            )
            .into_any_element()
    }
}

/// ⌘↵, pressed fresh: macOS repeats a held key, and a repeat must not land twice.
pub(super) fn is_land_key(event: &KeyDownEvent) -> bool {
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
    use gpui::Keystroke;
    use newjen_core::edits::{EditProposal, FileChange};

    fn press(keys: &'static str, is_held: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(keys).unwrap(),
            is_held,
            prefer_character_input: false,
        }
    }

    #[test]
    fn only_a_fresh_command_return_lands() {
        assert!(is_land_key(&press("cmd-enter", false)));
        assert!(!is_land_key(&press("cmd-enter", true)));
        assert!(!is_land_key(&press("enter", false)));
        assert!(!is_land_key(&press("cmd-shift-enter", false)));
        assert!(!is_land_key(&press("cmd-alt-enter", false)));
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
