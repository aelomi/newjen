//! The Thread / Terminal tabs over the main column and the per-thread terminal. (Approvals
//! are answered from the thread's own dock; the list's "Needs you" group replaces the inbox.)

use super::*;
impl ChatView {
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
}

/// Whether two project roots are the same repository, or one lies in the other.
pub(super) fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
