use super::{IconName, Look, Size, button, button_with_icon, icon};
use crate::rows::OnRestart;
use gpui::{prelude::*, *};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::{
    ActiveTheme as _,
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

/// What sits beside a message of the user's that has not gone through yet.
pub enum Pending {
    /// Held while the worktree is made: an hourglass.
    Held,
    /// Never reached the agent: says so, with Retry when there is a way to send it again.
    NotSent(Option<OnRestart>),
}

pub fn user_pending(id: ElementId, text: String, pending: Pending, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let lead = match pending {
        Pending::Held => icon(
            IconName::Hourglass,
            gpui_kit::component::Size::Small,
            theme.muted_foreground,
        )
        .into_any_element(),
        Pending::NotSent(retry) => h_flex()
            .flex_none()
            .gap_1p5()
            .text_sm()
            .text_color(theme.danger)
            .child(icon(
                IconName::CircleAlert,
                gpui_kit::component::Size::XSmall,
                theme.danger,
            ))
            .child("Not sent")
            .children(retry.map(|retry| {
                h_flex()
                    .gap_1p5()
                    .child(div().text_color(theme.muted_foreground).child("\u{b7}"))
                    .child(
                        button((id.clone(), "retry"), "Retry", Look::Ghost, Size::Sm, cx)
                            .debug_selector(|| "not-sent-retry".to_string())
                            .on_click(move |_, window, cx| retry(window, cx)),
                    )
            }))
            .into_any_element(),
    };
    h_flex()
        .w_full()
        .min_w_0()
        .justify_end()
        .items_center()
        .gap_2()
        .child(lead)
        .child(
            div()
                .min_w_0()
                .max_w(relative(0.8))
                .child(Bubble::new().child(text)),
        )
        .into_any_element()
}

/// A problem in the app or its agent: a tinted box with the headline, and on the right the
/// buttons that exist for it. A long detail or the process output sits behind "Show log".
#[derive(IntoElement)]
pub struct ErrorNotice {
    pub id: ElementId,
    pub title: SharedString,
    pub detail: SharedString,
    pub output: Vec<SharedString>,
    pub retry: Option<OnRestart>,
}

/// A detail short enough to sit under the title without a log.
const INLINE_DETAIL_CHARS: usize = 140;
/// The most log lines drawn open; the end matters most, so the start is cut.
const LOG_LINES: usize = 40;

impl RenderOnce for ErrorNotice {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let open = window.use_keyed_state((self.id.clone(), "log"), cx, |_, _| false);
        let shown = *open.read(cx);
        let detail = self.detail.trim().to_string();
        let inline = !detail.is_empty()
            && !detail.contains('\n')
            && detail.chars().count() <= INLINE_DETAIL_CHARS;
        let mut log: Vec<String> = Vec::new();
        if !detail.is_empty() && !inline {
            log.extend(detail.lines().map(str::to_string));
        }
        log.extend(self.output.iter().map(ToString::to_string));
        let hidden = log.len().saturating_sub(LOG_LINES);
        let theme = cx.theme();
        let danger = theme.danger;
        let header = h_flex()
            .gap_2()
            .items_center()
            .child(icon(
                IconName::CircleX,
                gpui_kit::component::Size::Small,
                danger,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(super::ellipsis(self.title.clone()).text_color(theme.foreground))
                    .when(inline, |text| {
                        text.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(detail.clone()),
                        )
                    }),
            )
            .when(!log.is_empty(), |row| {
                let open = open.clone();
                row.child(
                    button(
                        (self.id.clone(), "show-log"),
                        if shown { "Hide log" } else { "Show log" },
                        Look::Ghost,
                        Size::Sm,
                        cx,
                    )
                    .debug_selector(|| "show-log".to_string())
                    .on_click(move |_, _, cx| {
                        open.update(cx, |open, cx| {
                            *open = !*open;
                            cx.notify();
                        })
                    }),
                )
            })
            .children(self.retry.map(|retry| {
                button_with_icon(
                    (self.id.clone(), "retry"),
                    "Try again",
                    IconName::RefreshCw,
                    Look::Secondary,
                    Size::Sm,
                    cx,
                )
                .debug_selector(|| "retry".to_string())
                .on_click(move |_, window, cx| retry(window, cx))
            }));
        let theme = cx.theme();
        v_flex()
            .debug_selector(|| "error-notice".to_string())
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(danger.opacity(0.35))
            .bg(danger.opacity(0.12))
            .child(header)
            .when(shown && !log.is_empty(), |notice| {
                notice.child(
                    v_flex()
                        .pl_6()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .when(hidden > 0, |lines| {
                            lines.child(super::ellipsis(format!("… {hidden} earlier lines")))
                        })
                        .children(log.into_iter().skip(hidden).map(super::ellipsis)),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{OnRestart, Pending, user_pending};
    use gpui::{
        Context, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
        Window, div, px,
    };
    use std::cell::Cell;
    use std::rc::Rc;

    struct Example {
        retries: Rc<Cell<usize>>,
    }

    impl Render for Example {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let retries = self.retries.clone();
            let retry: OnRestart = Rc::new(move |_, _| retries.set(retries.get() + 1));
            div().w(px(600.)).child(user_pending(
                "sent".into(),
                "Run the full test suite.".to_string(),
                Pending::NotSent(Some(retry)),
                cx,
            ))
        }
    }

    #[gpui::test]
    fn a_message_that_was_not_sent_offers_retry(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let retries = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view(|_, _| Example {
            retries: retries.clone(),
        });
        cx.run_until_parked();
        let retry = cx.debug_bounds("not-sent-retry").unwrap();
        cx.simulate_click(retry.center(), Modifiers::none());
        assert_eq!(retries.get(), 1);
    }
}
