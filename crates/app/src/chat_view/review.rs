use super::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::diff::{Diff, DiffFile, DiffState};
use newjen_core::edits::EditPreview;
use newjen_core::landing::{self, Integration, LandingMode, Review};

pub(super) struct ApprovalPreview {
    request: newjen_core::ApprovalKey,
    diff: Option<Entity<DiffState>>,
    message: String,
    expected: Option<EditPreview>,
    validating: bool,
}

impl ApprovalPreview {
    pub(super) fn loading(&self) -> bool {
        self.expected.is_none() || self.validating
    }
}

pub(super) struct ReviewPanel {
    thread: String,
    receipt: Review,
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
        let task = cx.background_spawn(async move { proposal.preview() });
        cx.spawn_in(window, async move |view, cx| {
            let current = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
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
        self.edit_preview = Some(ApprovalPreview {
            request,
            diff: None,
            message: "Reading the proposed edit…".into(),
            expected: None,
            validating: false,
        });
        let task = cx.background_spawn(async move { proposal.preview() });
        cx.spawn_in(window, async move |view, cx| {
            let preview = task.await;
            let _ = view.update_in(cx, |view, _, cx| {
                let Some(current) = view.edit_preview.as_mut().filter(|current| current.request == request) else { return; };
                current.expected = Some(preview.clone());
                match preview {
                    EditPreview::Patch(patch) => match DiffFile::parse(&patch) {
                        Ok(files) => { current.diff = Some(cx.new(|cx| DiffState::new(files, cx))); current.message.clear(); }
                        Err(error) => current.message = format!("Couldn't display the proposed edit: {error}"),
                    },
                    EditPreview::TooLarge => current.message = "This edit is too large to preview. Allow once is the only available approval.".into(),
                    EditPreview::Unavailable(reason) => current.message = reason,
                }
                cx.notify();
            });
        }).detach();
    }

    pub(super) fn render_edit_preview(&self, height: f32, cx: &App) -> Option<AnyElement> {
        let preview = self.edit_preview.as_ref()?;
        Some(match &preview.diff {
            Some(diff) => div()
                .h(px((height - 390.).clamp(64., 150.)))
                .w_full()
                .child(Diff::new(diff).soft_wrap(true))
                .into_any_element(),
            None => ui::caption(preview.message.clone(), cx).into_any_element(),
        })
    }

    pub(super) fn run_snapshot(
        &mut self,
        thread: &Entity<Thread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let store = self.store.clone();
        let task = cx.background_spawn(async move {
            thread::finish_quit(&store, [stopping]);
            landing::review(&tools, &cwd, &base)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |view, _, cx| {
                view.review_loading = false;
                match result {
                    Ok(receipt) => match if receipt.truncated { Ok(Vec::new()) } else { DiffFile::parse(&receipt.patch) } {
                        Ok(files) => {
                            let message = if receipt.truncated { "The diff is too large to show. Landing is blocked." } else if mode == LandingMode::AgentManaged { "Only work recorded with jj is shown. Git commits are not imported; use jj and bookmark your work. The copy stays after landing." } else if receipt.patch.is_empty() { "No changes." } else { "Review the changes, then land or discard the work." }.into();
                            view.review_panel = Some(ReviewPanel { thread: id, receipt, diff: cx.new(|cx| DiffState::new(files, cx)), busy: false, message });
                        }
                        Err(error) => view.slots[view.active].notice = Some(Notice::held("Couldn't display the diff", error.to_string())),
                    },
                    Err(error) => view.slots[view.active].notice = Some(Notice::held("Couldn't review work", format!("{error:#}"))),
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
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
        if self.slots.iter().enumerate().any(|(other, slot)| {
            slot.thread.read(cx).blocks_copy_creation()
                || (other != ix && slot.thread.read(cx).owns_project_work())
        }) {
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
                            if integration.is_none() { view.archive(&id, window, cx); }
                            else { view.slots[ix].notice = Some(Notice::held("Work landed", "The work is in the project. Archive the thread when you are ready to close it.".into())); }
                        }
                    }
                    Err(error) => if let Some(panel) = view.review_panel.as_mut() { panel.busy = false; panel.message = format!("{error:#}"); },
                }
                cx.notify();
            });
        }).detach();
        cx.notify();
    }

    pub(super) fn keep_project_files_and_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                let store = view.store.clone();
                cx.notify();
                Some(cx.background_spawn(async move {
                    thread::finish_quit(&store, [stopping]);
                }))
            });
            let Ok(Some(task)) = task else {
                return;
            };
            task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.review_loading = false;
                let Some(ix) = view.index_of(&id, cx) else {
                    return;
                };
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
            let mode = thread.landing_mode();
            let next = if mode == LandingMode::HarnessManaged {
                LandingMode::AgentManaged
            } else {
                LandingMode::HarnessManaged
            };
            let label = if mode == LandingMode::HarnessManaged {
                "Harness manages jj"
            } else {
                "Agent manages jj"
            };
            return h_flex()
                .px_4()
                .gap_2()
                .child(
                    ui::button("landing-mode", label, ui::Look::Ghost, ui::Size::Sm, cx).on_click(
                        cx.listener(move |view, _, _, cx| {
                            let result = view
                                .thread()
                                .update(cx, |thread, _| thread.set_landing_mode(next));
                            view.slots[view.active].notice =
                                notice_after(result, "Couldn't change landing mode");
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    ui::button(
                        "landing-default",
                        "Use as project default",
                        ui::Look::Ghost,
                        ui::Size::Sm,
                        cx,
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.store.set_default_landing_mode(view.project.id, mode);
                        view.project.landing_mode = mode;
                        cx.notify();
                    })),
                )
                .into_any_element();
        }
        h_flex()
            .px_4()
            .gap_2()
            .child(
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
                .on_click(cx.listener(|view, _, window, cx| view.open_review(window, cx))),
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
        let can_land = !panel.busy && !panel.receipt.truncated;
        let button = |id, label, integration| {
            ui::button(id, label, ui::Look::Secondary, ui::Size::Sm, cx)
                .disabled(!can_land)
                .on_click(cx.listener(move |view, _, window, cx| {
                    if can_land {
                        view.resolve_review(false, integration, window, cx);
                    }
                }))
        };
        let rebase = button("land-rebase", "Rebase and land", Integration::Rebase);
        let merge = button("land-merge", "Merge and land", Integration::Merge);
        v_flex()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_2()
            .child(ui::caption(panel.message.clone(), cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Diff::new(&panel.diff).soft_wrap(true)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(rebase)
                    .child(merge)
                    .child(
                        ui::button(
                            "discard-work",
                            "Discard work",
                            ui::Look::Ghost,
                            ui::Size::Sm,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.resolve_review(true, Integration::Rebase, window, cx)
                        })),
                    )
                    .child(
                        ui::button(
                            "close-review",
                            "Close review",
                            ui::Look::Ghost,
                            ui::Size::Sm,
                            cx,
                        )
                        .on_click(cx.listener(|view, _, _, cx| {
                            if !view.review_panel.as_ref().is_some_and(|panel| panel.busy) {
                                view.review_panel = None;
                                cx.notify();
                            }
                        })),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use newjen_core::edits::{EditProposal, FileChange};

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
