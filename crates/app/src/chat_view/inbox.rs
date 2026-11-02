//! The per-thread terminal and the panel under the main column that shows it. (Approvals are
//! answered from the thread's own dock; the list's "Needs you" group replaces the inbox.)

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

    /// Whether the terminal panel is out and the keyboard is in it.
    pub(super) fn terminal_focused(&self, window: &Window, cx: &App) -> bool {
        self.terminal_open
            && self.slots[self.active]
                .terminal
                .as_ref()
                .is_some_and(|terminal| terminal.focus_handle(cx).contains_focused(window, cx))
    }

    /// ⌘` or the terminal button: shows the thread's shell under the chat, opening one in its
    /// working folder the first time. Hides the panel when it is already up (the shell keeps
    /// running).
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_open {
            self.terminal_open = false;
            self.terminal_wanted = false;
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
            cx.notify();
            return;
        }
        let cwd = self.thread().read(cx).cwd().to_path_buf();
        let terminal = self.slots[self.active]
            .terminal
            .get_or_insert_with(|| cx.new(|cx| TerminalPanel::new(&cwd, cx)))
            .clone();
        self.terminal_open = true;
        self.terminal_wanted = true;
        self.drawer_open = false;
        window.focus(&terminal.focus_handle(cx), cx);
        cx.notify();
    }

    /// The panel's ×: stops the shell and takes the panel away.
    fn close_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(terminal) = self.slots[self.active].terminal.take() {
            terminal.update(cx, |terminal, _| terminal.shutdown());
        }
        self.terminal_open = false;
        self.terminal_wanted = false;
        self.enter(window, cx);
    }

    /// The terminal panel under the chat: a strip with the shell's tab and a close button, over
    /// the shell. `None` while it is hidden.
    pub(super) fn render_terminal_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.terminal_open {
            return None;
        }
        let terminal = self.slots[self.active].terminal.clone()?;
        let close = self.press(cx, |view, window, cx| view.close_terminal(window, cx));
        let colors = &cx.theme().colors;
        let strip = h_flex()
            .flex_none()
            .justify_between()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(colors.border)
            .child(
                h_flex()
                    .gap_1p5()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(colors.muted)
                    .text_sm()
                    .text_color(colors.foreground)
                    .child(ui::icon(
                        ui::IconName::Terminal,
                        gpui_kit::component::Size::Small,
                        colors.muted_foreground,
                    ))
                    .child(crate::terminal::shell_name()),
            )
            .child(
                ui::icon_button("close-terminal", ui::IconName::Close, "Close terminal", cx)
                    .debug_selector(|| "close-terminal".to_string())
                    .on_click(move |_, window, cx| close(window, cx)),
            );
        Some(
            v_flex()
                .id("terminal-drawer")
                .debug_selector(|| "terminal-drawer".to_string())
                .flex_none()
                .w_full()
                .h(px(TERMINAL_HEIGHT))
                .max_h(gpui::relative(0.5))
                .child(strip)
                .child(v_flex().flex_1().min_h_0().px_3().pb_2().child(terminal))
                .into_any_element(),
        )
    }
}

const TERMINAL_HEIGHT: f32 = 250.0;

/// Whether two project roots are the same repository, or one lies in the other.
pub(super) fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
