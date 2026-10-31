//! The Thread / Approvals / Terminal tabs over the main column, the approvals inbox and the
//! per-thread terminal.
//!
//! The inbox lists every live approval of every loaded project: this project's threads and
//! its peers' (`ProjectView` hands each view the others, see `set_copy_peers`). It never
//! copies a request: each row reads the thread that owns it, and an answer carries that
//! thread's id and the request's key, so the thread on screen never decides where an answer
//! goes. It offers Allow once and Deny only; session grants stay in the thread's own dock.

use super::*;
use gpui::WeakEntity;
use gpui_kit::component::scroll::ScrollableElement as _;

/// The inbox's room for a request's detail: wider than the dock, and it scrolls.
const INBOX_ROOM: Room = Room {
    cols: 100,
    rows: 12,
};

type OnAnswer = Rc<dyn Fn(ApprovalIntent, &mut Window, &mut App)>;

/// One waiting thread, read from the view that owns it.
struct InboxRow {
    /// The owning view; `None` for this one.
    owner: Option<WeakEntity<ChatView>>,
    thread_id: String,
    key: newjen_core::ApprovalKey,
    thread_title: String,
    project: SharedString,
    folder: String,
    title: String,
    detail: String,
    preview: Option<AnyElement>,
    ready: bool,
    notice: Option<String>,
}

impl ChatView {
    /// Threads with a live approval across every loaded project.
    pub(super) fn waiting_everywhere(&self, cx: &App) -> usize {
        self.waiting_threads(cx)
            + self
                .copy_peers
                .iter()
                .filter_map(|peer| peer.upgrade())
                .map(|peer| peer.read(cx).waiting_threads(cx))
                .sum::<usize>()
    }

    pub(super) fn open_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread()
            .clone()
            .update(cx, |thread, _| thread.set_active(false));
        self.panel = Panel::Inbox;
        self.drawer_open = false;
        window.focus(&self.root, cx);
        cx.notify();
    }

    /// Whether a thread of this project or a peer sharing the repository has a terminal in
    /// the project folder (not in a Rift copy), where a shell could change files while Rift
    /// snapshots the project.
    pub(super) fn project_terminal_open(&self, source: &Path, cx: &App) -> bool {
        let open_in = |view: &ChatView| {
            view.slots.iter().any(|slot| {
                let thread = slot.thread.read(cx);
                slot.terminal.is_some()
                    && thread.copy().is_none()
                    && overlaps(source, &rift::project_root(thread.cwd()).0)
            })
        };
        open_in(self)
            || self
                .copy_peers
                .iter()
                .filter_map(|peer| peer.upgrade())
                .any(|peer| open_in(peer.read(cx)))
    }

    /// The thread (here or in a peer) whose Rift copy of `source` is being made, if any.
    fn copy_in_progress(&self, source: &Path, cx: &App) -> bool {
        let making_in = |view: &ChatView| {
            view.slots.iter().any(|slot| {
                let thread = slot.thread.read(cx);
                thread.preparing().is_some()
                    && overlaps(source, &rift::project_root(thread.cwd()).0)
            })
        };
        making_in(self)
            || self
                .copy_peers
                .iter()
                .filter_map(|peer| peer.upgrade())
                .any(|peer| making_in(peer.read(cx)))
    }

    /// ⌘` or the Terminal tab: shows the thread's shell, opening one in its working folder
    /// the first time. Back to the thread when it is already up.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel == Panel::Terminal {
            self.panel = Panel::Thread;
            self.enter(window, cx);
            return;
        }
        let refused = {
            let thread = self.thread().read(cx);
            if thread.preparing().is_some()
                || (thread.works_in() == WorksIn::RiftCopy && thread.copy().is_none())
            {
                Some(Notice::held(
                    "The working folder is not ready",
                    "Send the first message and wait for the Rift copy before opening its \
                     terminal."
                        .to_string(),
                ))
            } else if thread.copy().is_none()
                && self.copy_in_progress(&rift::project_root(thread.cwd()).0, cx)
            {
                Some(Notice::held(
                    "A Rift copy is being made",
                    "Wait for the copy before opening a terminal in the project folder."
                        .to_string(),
                ))
            } else {
                None
            }
        };
        if let Some(notice) = refused {
            self.slots[self.active].notice = Some(notice);
            self.panel = Panel::Thread;
            cx.notify();
            return;
        }
        let cwd = self.thread().read(cx).cwd().to_path_buf();
        let terminal = self.slots[self.active]
            .terminal
            .get_or_insert_with(|| cx.new(|cx| TerminalPanel::new(&cwd, cx)))
            .clone();
        self.thread()
            .clone()
            .update(cx, |thread, _| thread.set_active(false));
        self.panel = Panel::Terminal;
        self.drawer_open = false;
        window.focus(&terminal.focus_handle(cx), cx);
        cx.notify();
    }

    fn close_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.slots[self.active].terminal.take() {
            terminal.update(cx, |terminal, _| terminal.shutdown());
        }
        self.panel = Panel::Thread;
        self.enter(window, cx);
    }

    pub(super) fn render_panel_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let thread = self.press(cx, |view, window, cx| {
            view.panel = Panel::Thread;
            view.enter(window, cx);
        });
        let inbox = self.press(cx, |view, window, cx| view.open_inbox(window, cx));
        let terminal = self.press(cx, |view, window, cx| view.toggle_terminal(window, cx));
        let close = self.press(cx, |view, window, cx| view.close_terminal(window, cx));
        let tab = |id, label: SharedString, panel, action: OnPress| {
            ui::button(
                id,
                label,
                if self.panel == panel {
                    ui::Look::Secondary
                } else {
                    ui::Look::Ghost
                },
                ui::Size::Sm,
                cx,
            )
            .on_click(move |_, window, cx| action(window, cx))
        };
        let count = self.waiting_everywhere(cx);
        h_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .py_1()
            .child(tab(
                "conversation-tab",
                "Thread".into(),
                Panel::Thread,
                thread,
            ))
            .child(tab(
                "approvals-tab",
                format!("Approvals ({count})").into(),
                Panel::Inbox,
                inbox,
            ))
            .child(tab(
                "terminal-tab",
                "Terminal".into(),
                Panel::Terminal,
                terminal,
            ))
            .when(self.panel == Panel::Terminal, |row| {
                row.child(
                    ui::button(
                        "close-terminal",
                        "Close terminal",
                        ui::Look::Ghost,
                        ui::Size::Sm,
                        cx,
                    )
                    .on_click(move |_, window, cx| close(window, cx)),
                )
            })
            .into_any_element()
    }

    /// The rows of `view`'s waiting threads, oldest request first per thread.
    fn inbox_rows(
        &self,
        view: &ChatView,
        owner: Option<WeakEntity<ChatView>>,
        height: f32,
        cx: &App,
    ) -> Vec<InboxRow> {
        view.slots
            .iter()
            .filter_map(|slot| {
                let thread = slot.thread.read(cx);
                let request = thread.transcript().pending_approval()?;
                let queued = thread
                    .transcript()
                    .items()
                    .iter()
                    .filter(|item| {
                        matches!(
                            item,
                            Item::Tool {
                                state: ToolState::Waiting,
                                ..
                            }
                        )
                    })
                    .count();
                let mut title = approval::title(request.view());
                if queued > 1 {
                    title.push_str(&format!(" (1 of {queued})"));
                }
                let detail =
                    approval::detail(request.view(), slot.inbox_outside.as_deref(), INBOX_ROOM);
                Some(InboxRow {
                    owner: owner.clone(),
                    thread_id: thread.id().to_string(),
                    key: request.key(),
                    thread_title: thread.title().to_string(),
                    project: view.project_label.clone(),
                    folder: tilde(thread.cwd(), view.home.as_deref()),
                    title,
                    detail: detail.text,
                    preview: slot
                        .preview
                        .as_ref()
                        .map(|preview| preview.render(height, cx)),
                    ready: !slot
                        .preview
                        .as_ref()
                        .is_some_and(|preview| preview.loading()),
                    notice: slot.notice.as_ref().map(|notice| notice.detail.clone()),
                })
            })
            .collect()
    }

    pub(super) fn render_inbox(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let height = f32::from(window.viewport_size().height);
        let mut rows = self.inbox_rows(self, None, height, cx);
        for peer in &self.copy_peers {
            let Some(peer) = peer.upgrade() else { continue };
            rows.extend(self.inbox_rows(peer.read(cx), Some(peer.downgrade()), height, cx));
        }
        let waiting = !rows.is_empty();
        let mut content = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .p_4()
            .child(ui::title("Approvals", cx));
        for row in rows {
            content = content.child(self.render_inbox_row(row, cx));
        }
        if let Some(note) = self.attention_note {
            content = content.child(ui::caption(note, cx));
        }
        if !waiting {
            content = content.child(ui::caption("All caught up. No approvals are waiting.", cx));
        }
        div()
            .id("approvals-inbox")
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_y_scrollbar()
            .child(content)
            .into_any_element()
    }

    fn render_inbox_row(&self, row: InboxRow, cx: &mut Context<Self>) -> AnyElement {
        let id = SharedString::from(format!("{}-{}", row.thread_id, row.key));
        let thread_id = row.thread_id.clone();
        let key = row.key;
        // An answer goes to the view that owns the thread, by thread id and request key.
        let answer: OnAnswer = match &row.owner {
            None => {
                let view = cx.weak_entity();
                Rc::new(move |choice, window, cx| {
                    let _ = view.update(cx, |view, cx| {
                        view.inbox_answer(&thread_id, key, choice, window, cx)
                    });
                })
            }
            Some(owner) => {
                let owner = owner.clone();
                Rc::new(move |choice, window, cx| {
                    let _ = owner.update(cx, |owner, cx| {
                        owner.inbox_answer(&thread_id, key, choice, window, cx)
                    });
                })
            }
        };
        let open: OnPress = match (&row.owner, &self.thread_opener) {
            (None, _) => {
                let thread_id = row.thread_id.clone();
                self.press(cx, move |view, window, cx| {
                    view.show_thread(&thread_id, window, cx)
                })
            }
            (Some(_), Some(opener)) => {
                let opener = opener.clone();
                let thread_id = row.thread_id.clone();
                Rc::new(move |window, cx| opener(&thread_id, window, cx))
            }
            (Some(_), None) => Rc::new(|_, _| {}),
        };
        let card = ui::ApprovalCard::new(
            (ElementId::from("inbox-approval"), id.clone()),
            row.title,
            row.detail,
            move |choice, window, cx| answer(choice, window, cx),
        )
        .once_only()
        .selectors(format!("inbox-{}-", row.thread_id))
        .preview(row.preview, row.ready);
        v_flex()
            .w_full()
            .min_w_0()
            .id((ElementId::from("inbox-row"), id.clone()))
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .min_w_0()
                    .child(ui::ellipsis(row.thread_title))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(ui::middle_ellipsis(format!(
                                "{} · {}",
                                row.project, row.folder
                            ))),
                    )
                    .child(
                        ui::button(
                            (ElementId::from("inbox-review"), id),
                            "Open thread",
                            ui::Look::Ghost,
                            ui::Size::Sm,
                            cx,
                        )
                        .debug_selector(move || format!("inbox-{}-open", row.thread_id))
                        .on_click(move |_, window, cx| open(window, cx)),
                    ),
            )
            .children(row.notice.map(|notice| ui::caption(notice, cx)))
            .child(card)
            .into_any_element()
    }
}

/// Whether two project roots are the same repository, or one lies in the other.
pub(super) fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
