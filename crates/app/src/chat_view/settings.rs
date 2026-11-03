use super::*;
use crate::settings_view;

impl ChatView {
    /// Shows the Settings view in the main card, as `show_usage` does the Usage view.
    pub(super) fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane == Pane::Settings
            || self.review_loading
            || self
                .review_panel
                .as_ref()
                .is_some_and(review::ReviewPanel::busy)
        {
            return;
        }
        self.review_panel = None;
        self.terminal_open = false;
        self.drawer_open = false;
        self.pane = Pane::Settings;
        self.thread().clone().update(cx, |thread, cx| {
            thread.set_active(false);
            cx.notify();
        });
        self.focus_panel(window, cx);
        cx.notify();
    }

    /// ⌘, and the sidebar's gear: open Settings, or go back to the thread when it is open.
    pub(super) fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane == Pane::Settings {
            self.show(self.active, window, cx);
        } else {
            self.show_settings(window, cx);
        }
    }

    /// Applies a change from the view and keeps it for the next launch.
    fn set_appearance(
        &mut self,
        appearance: newjen_core::appearance::Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.store.set_appearance(&appearance);
        theme::set_appearance(appearance, Some(window), cx);
        cx.notify();
    }

    pub(super) fn render_settings_card(
        &mut self,
        placement: Placement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let header = title_bar::pane_header(
            "Settings",
            "Appearance",
            self.pane_list_toggle(placement, cx),
            cx,
        );
        let view = cx.weak_entity();
        let on_change: settings_view::OnChange = Rc::new(move |appearance, window, cx| {
            let _ = view.update(cx, |view, cx| view.set_appearance(appearance, window, cx));
        });
        let appearance = theme::appearance(cx).clone();
        let body = settings_view::render(
            &settings_view::Input {
                appearance: &appearance,
                on_change,
            },
            cx,
        );
        Self::pane_card("settings-card", header, body, placement, cx)
    }
}
