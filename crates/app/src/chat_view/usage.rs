use std::cell::RefCell;
use std::collections::HashMap;

use super::*;
use crate::usage_view::{self, Snapshot};
use newjen_core::providers::Provider;

thread_local! {
    static TURN_USAGE: RefCell<HashMap<(Provider, i64), (Instant, bool)>> =
        RefCell::new(HashMap::new());
}

const TURN_USAGE_FRESH: Duration = Duration::from_secs(10);

/// Whether Newjen's turns used `provider` since `since` (Unix seconds). The footer asks on
/// every redraw, so an answer is kept for a few seconds instead of reading the store each time.
fn turn_usage_since(store: &Store, provider: Provider, since: i64) -> bool {
    TURN_USAGE.with_borrow_mut(|cache| {
        if let Some((read_at, used)) = cache.get(&(provider, since))
            && read_at.elapsed() < TURN_USAGE_FRESH
        {
            return *used;
        }
        let used = store
            .threads_with_usage_since(provider, since * 1000)
            .is_ok_and(|threads| !threads.is_empty());
        cache.retain(|_, (read_at, _)| read_at.elapsed() < TURN_USAGE_FRESH);
        cache.insert((provider, since), (Instant::now(), used));
        used
    })
}

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

    /// The footer row: the settings gear and the usage ring, which shows the tightest limit among
    /// the providers in use.
    pub(super) fn render_usage_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = now_ms() / 1000;
        let usages = usage_view::board_usages();
        let on_screen = self.thread().read(cx).agent_config().map(|a| a.provider);
        let threads: Vec<_> = self
            .slots
            .iter()
            .filter_map(|slot| {
                let thread = slot.thread.read(cx);
                Some((thread.agent_config()?.provider, thread.status()))
            })
            .collect();
        let in_use = usage_view::providers_in_use(threads, on_screen, |known| {
            usage_view::providers_with_turns(&usages, now, |provider, since| {
                known.contains(&provider) || turn_usage_since(&self.store, provider, since)
            })
        });
        let gauge = usage_view::gauge(&usages, &in_use, now);
        let on_press = self.press(cx, |view, window, cx| view.show_usage(window, cx));
        usage_view::footer(gauge.as_ref(), self.pane == Pane::Usage, on_press, cx)
    }

    pub(super) fn render_usage_card(
        &mut self,
        mode: SidebarMode,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.usage_snapshot.is_none() {
            self.refresh_usage();
        }
        let show_list = (mode != SidebarMode::Docked).then(|| {
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
        });
        let header = title_bar::usage_header(show_list, cx);
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
        let material = crate::theme::material(cx);
        let floating = mode == SidebarMode::Docked;
        v_flex()
            .id("usage-card")
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
}
