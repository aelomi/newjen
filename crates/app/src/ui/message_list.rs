use gpui::{AnyElement, App, ElementId, IntoElement, RenderOnce, SharedString, Window};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};

type RenderItem = Box<dyn Fn(usize, &mut Window, &mut App) -> AnyElement>;

#[derive(IntoElement)]
pub struct MessageList {
    id: ElementId,
    count: usize,
    render: RenderItem,
}

impl MessageList {
    pub fn new(
        id: impl Into<ElementId>,
        count: usize,
        render: impl Fn(usize, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            count,
            render: Box::new(render),
        }
    }
}

impl RenderOnce for MessageList {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let count = self.count;
        let state = window.use_keyed_state((self.id.clone(), "messages"), cx, |_, cx| {
            MessageScrollerState::new(count, cx)
        });
        let fresh = window.use_keyed_state((self.id.clone(), "fresh"), cx, |_, _| 0usize);
        let (before, away) = {
            let state = state.read(cx);
            (state.item_count(), !state.is_following_tail())
        };
        let arrived = arrived(before, count, away, *fresh.read(cx));
        if arrived != *fresh.read(cx) {
            fresh.update(cx, |fresh, _| *fresh = arrived);
        }
        if before != count {
            state.update(cx, |state, cx| {
                state.splice(count.min(before)..before, count.saturating_sub(before), cx);
            });
        }
        let label: SharedString = match arrived {
            0 => "Latest".into(),
            1 => "1 new message".into(),
            n => format!("{n} new messages").into(),
        };
        MessageScroller::new(self.id, state, self.render)
            .with_jump_button_label(label.clone())
            .with_jump_button_renderer(move |button| button.label(label))
            .with_jump_button_transition(std::time::Duration::ZERO)
    }
}

/// How many rows arrived while the reader was scrolled away from the newest one.
fn arrived(before: usize, now: usize, away: bool, fresh: usize) -> usize {
    match (away, now >= before) {
        (true, true) => fresh + (now - before),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    #[test]
    fn new_messages_count_only_while_away() {
        assert_eq!(arrived(3, 5, true, 0), 2);
        assert_eq!(arrived(5, 6, true, 2), 3);
        assert_eq!(arrived(5, 6, false, 0), 0);
        assert_eq!(arrived(6, 2, true, 3), 0);
    }
    #[gpui::test]
    fn kit_scroller_follows_appended_rows_and_can_return_to_tail(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let state = cx.new(|cx| MessageScrollerState::new(3, cx));
        state.update(cx, |state, cx| {
            assert!(state.is_following_tail());
            state.append(2, cx);
            assert_eq!(state.item_count(), 5);
            assert!(state.is_following_tail());
            state.scroll_to_item(0, cx);
            assert!(!state.is_following_tail());
            state.append(1, cx);
            assert!(!state.is_following_tail());
            state.scroll_to_end(cx);
            assert!(state.is_following_tail());
        });
    }
}
