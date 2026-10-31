use super::{IconName, Look, Size, button_with_icon};
use crate::rows::OnRestart;
use gpui::{prelude::*, *};
use gpui_kit::component::{
    alert::Alert,
    bubble::{Bubble, BubbleVariant},
    message::{Message, MessageAlignment, MessageContent},
};

pub fn assistant(body: impl IntoElement, _: &App) -> Div {
    div()
        .debug_selector(|| "assistant-reply".to_string())
        .child(
            Message::new().content(
                MessageContent::new()
                    .bubble(Bubble::new().with_variant(BubbleVariant::Ghost).child(body)),
            ),
        )
}

pub fn user(text: String, _: &App) -> Message {
    Message::new()
        .alignment(MessageAlignment::End)
        .content(MessageContent::new().bubble(Bubble::new().child(text)))
}

#[derive(IntoElement)]
pub struct ErrorNotice {
    pub id: ElementId,
    pub title: SharedString,
    pub detail: SharedString,
    pub retry: Option<OnRestart>,
}

impl RenderOnce for ErrorNotice {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Alert::error(self.id.clone(), self.detail).title(self.title))
            .children(self.retry.map(|retry| {
                div().flex().child(
                    button_with_icon(
                        (self.id, "retry"),
                        "Try again",
                        IconName::RefreshCw,
                        Look::Secondary,
                        Size::Sm,
                        cx,
                    )
                    .on_click(move |_, window, cx| retry(window, cx)),
                )
            }))
    }
}
