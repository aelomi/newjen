use super::*;
use crate::usage_view::{self, Snapshot};

impl ChatView {
    /// Shows the Usage view in the main card. The thread that was on screen stops being "on
    /// screen", so finishing its turn marks it unread again.
    pub(super) fn show_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane == Pane::Usage
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
        self.pane = Pane::Usage;
        self.thread().clone().update(cx, |thread, cx| {
            thread.set_active(false);
            cx.notify();
        });
        self.refresh_usage();
        self.focus_panel(window, cx);
        cx.notify();
    }

    /// Reads the Usage view's numbers from the store. True when they changed.
    pub(super) fn refresh_usage(&mut self) -> bool {
        let loaded = Snapshot::load(&self.store, now_ms());
        let changed = self.usage_snapshot.as_ref() != Some(&loaded);
        self.usage_snapshot = Some(loaded);
        changed
    }

    pub(super) fn render_usage_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = now_ms() / 1000;
        let meter = usage_view::footer_meter(now).map(|meter| usage_view::meter_view(meter, cx));
        let on_press = self.press(cx, |view, window, cx| view.show_usage(window, cx));
        let on_settings = self.press(cx, |view, window, cx| view.toggle_settings(window, cx));
        sidebar::usage_row(
            self.pane == Pane::Usage,
            meter,
            on_press,
            (self.pane == Pane::Settings, on_settings),
            cx,
        )
    }

    /// The button that brings the thread list back, when it is out of sight.
    pub(super) fn pane_list_toggle(
        &self,
        placement: Placement,
        cx: &mut Context<Self>,
    ) -> Option<(OnPress, sidebar::ListBadge)> {
        (placement != Placement::Docked).then(|| {
            let statuses: Vec<Status> = self
                .slots
                .iter()
                .map(|slot| slot.thread.read(cx).status())
                .collect();
            let badge = sidebar::ListBadge {
                attention: sidebar::hidden_needs_you(&statuses, self.active),
                update: self.update_ready(cx),
            };
            let toggle = self.press(cx, |view, window, cx| view.toggle_sidebar(window, cx));
            (toggle, badge)
        })
    }

    /// The main card of the Usage or Settings view: its header over its body.
    pub(super) fn pane_card(
        id: &'static str,
        header: AnyElement,
        body: AnyElement,
        placement: Placement,
        cx: &App,
    ) -> AnyElement {
        let material = crate::theme::material(cx);
        let floating = placement == Placement::Docked;
        v_flex()
            .id(id)
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(material.card)
            .when(floating, |main| {
                main.border_1()
                    .border_color(material.card_stroke)
                    .rounded(CARD_RADIUS)
                    .shadow(material.card_shadow())
                    .overflow_hidden()
            })
            .child(header)
            .child(body)
            .into_any_element()
    }

    pub(super) fn render_usage_card(
        &mut self,
        placement: Placement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.usage_snapshot.is_none() {
            self.refresh_usage();
        }
        let header = title_bar::pane_header(
            "Usage",
            "From each CLI's own reports",
            self.pane_list_toggle(placement, cx),
            cx,
        );
        let snapshot = self.usage_snapshot.clone().unwrap_or_default();
        let openable: Vec<String> = snapshot
            .threads
            .iter()
            .filter(|line| line.project_id == self.project.id && !line.archived)
            .filter(|line| self.has_thread(&line.thread_id, cx))
            .map(|line| line.thread_id.clone())
            .collect();
        let view = cx.weak_entity();
        let on_open: usage_view::OnOpenThread = Rc::new(move |id, window, cx| {
            let id = id.to_string();
            let _ = view.update(cx, |view, cx| view.show_thread(&id, window, cx));
        });
        let body = usage_view::render(
            &usage_view::Input {
                snapshot: &snapshot,
                statuses: &self.picker.statuses,
                now: now_ms() / 1000,
                now_ms: now_ms(),
                openable: &openable,
                on_open,
            },
            cx,
        );
        Self::pane_card("usage-card", header, body, placement, cx)
    }
}
