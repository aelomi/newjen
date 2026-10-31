use std::time::Duration;

use crate::ui::{Chevron, CodeBlock, StepState, ellipsis, step_mark};
use gpui::{
    App, ElementId, Entity, FontFeatures, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, SharedString, Styled, Window, div, prelude::*,
};
use gpui_kit::component::{
    ActiveTheme as _,
    button::{Button, ButtonVariants as _},
    collapsible::Collapsible,
};

fn took(spent: Duration) -> String {
    let ms = spent.as_millis() as u64;
    let tenths = (ms + 50) / 100;
    match ms {
        0..1_000 => format!("{ms} ms"),
        _ if tenths < 600 => format!("{}.{} s", tenths / 10, tenths % 10),
        _ => {
            let secs = (ms + 500) / 1_000;
            if secs >= 86400 {
                format!("{}d {}h", secs / 86400, secs / 3600 % 24)
            } else if secs < 3600 {
                format!("{}m {}s", secs / 60, secs % 60)
            } else {
                format!("{}h {}m", secs / 3600, secs % 3600 / 60)
            }
        }
    }
}

fn header(id: ElementId, name: SharedString, toggle: Option<Entity<bool>>, cx: &App) -> Button {
    let open = toggle.as_ref().map(|state| *state.read(cx));
    Button::new(id)
        .debug_selector(|| "tool-header".to_string())
        .when_some(open, |button, open| button.toggled(open))
        .ghost()
        .w_full()
        .accessibility_label(match open {
            Some(true) => format!("{name}, expanded"),
            Some(false) => format!("{name}, collapsed"),
            None => name.to_string(),
        })
        .when_some(toggle, |button, state| {
            button.on_click(move |_, _, cx| {
                state.update(cx, |open, cx| {
                    *open = !*open;
                    cx.notify();
                })
            })
        })
}

#[derive(IntoElement)]
pub struct ToolCard {
    id: ElementId,
    name: SharedString,
    status: StepState,
    summary: Option<SharedString>,
    took: Option<Duration>,
    arguments: Option<SharedString>,
    result: Option<SharedString>,
}

impl ToolCard {
    pub fn new(id: impl Into<ElementId>, name: impl Into<SharedString>, status: StepState) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status,
            summary: None,
            took: None,
            arguments: None,
            result: None,
        }
    }

    pub fn summary(mut self, text: impl Into<SharedString>) -> Self {
        self.summary = Some(text.into());
        self
    }

    pub fn took(mut self, took: Duration) -> Self {
        self.took = Some(took);
        self
    }

    pub fn arguments(mut self, json: impl Into<SharedString>) -> Self {
        self.arguments = Some(json.into());
        self
    }

    pub fn result(mut self, text: impl Into<SharedString>) -> Self {
        self.result = Some(text.into());
        self
    }
}

impl RenderOnce for ToolCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state((self.id.clone(), "open"), cx, |_, _| false);
        let open = *state.read(cx);
        let opens = self.arguments.is_some() || self.result.is_some();
        let failed = self.status == StepState::Failed;
        let mark = step_mark(self.status, cx);
        let colors = cx.theme();
        let label = |text: &'static str| {
            div()
                .text_size(gpui::rems(0.75))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.muted_foreground)
                .child(text)
        };
        Collapsible::new()
            .open(open && opens)
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .text_size(gpui::rems(0.875))
            .child(
                header(
                    (self.id.clone(), "header").into(),
                    self.name.clone(),
                    opens.then_some(state),
                    cx,
                )
                .child(mark)
                .child(
                    div()
                        .min_w_0()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(colors.foreground)
                        .child(ellipsis(self.name)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(colors.muted_foreground)
                        .children(self.summary.map(ellipsis)),
                )
                .children(self.took.map(|spent| {
                    div()
                        .flex_none()
                        .text_color(colors.muted_foreground)
                        .font_features(FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)])))
                        .child(took(spent))
                }))
                .when(opens, |row| {
                    row.child(Chevron::new(open).size(gpui::px(16.)))
                }),
            )
            .content(
                div()
                    .debug_selector(|| "tool-details".to_string())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_3()
                    .pt_1()
                    .pb_3()
                    .children(self.arguments.map(|json| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label("Arguments"))
                            .child(
                                CodeBlock::new((self.id.clone(), "arguments"), json)
                                    .language("json"),
                            )
                    }))
                    .children(self.result.map(|text| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(label(if failed { "Error" } else { "Result" }))
                            .child(if failed {
                                div()
                                    .text_color(colors.danger)
                                    .child(text)
                                    .into_any_element()
                            } else {
                                CodeBlock::new((self.id.clone(), "result"), text).into_any_element()
                            })
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{
        Context, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, Render,
        TestAppContext,
    };
    use gpui_kit::component::Root;
    struct Example {
        focus: FocusHandle,
    }
    impl Render for Example {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .track_focus(&self.focus)
                .size_full()
                .child(ToolCard::new("tool", "Bash", StepState::Done).arguments("{}"))
        }
    }
    #[gpui::test]
    fn kit_disclosure_keeps_click_focus_and_opens_from_keyboard(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut example = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| Example {
                focus: cx.focus_handle(),
            });
            view.read(cx).focus.clone().focus(window, cx);
            example = Some(view.clone());
            Root::new(view, window, cx)
        });
        let view = example.unwrap();
        cx.run_until_parked();
        assert!(cx.debug_bounds("tool-details").is_none());
        let header = cx.debug_bounds("tool-header").unwrap();
        cx.simulate_click(header.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("tool-details").is_some());
        assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
        cx.simulate_keystrokes("tab");
        assert!(!cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
        for (key, expanded) in [("enter", false), ("space", true)] {
            let keystroke = Keystroke::parse(key).unwrap();
            cx.simulate_event(KeyDownEvent {
                keystroke: keystroke.clone(),
                is_held: false,
                prefer_character_input: false,
            });
            cx.simulate_event(KeyUpEvent { keystroke });
            cx.run_until_parked();
            assert_eq!(cx.debug_bounds("tool-details").is_some(), expanded);
        }
    }

    #[test]
    fn elapsed_labels_keep_the_tool_cards_units_and_rounding() {
        for (ms, label) in [
            (999, "999 ms"),
            (1_050, "1.1 s"),
            (59_950, "1m 0s"),
            (245_000, "4m 5s"),
            (5_025_000, "1h 23m"),
            (183_600_000, "2d 3h"),
        ] {
            assert_eq!(took(Duration::from_millis(ms)), label);
        }
    }
}
