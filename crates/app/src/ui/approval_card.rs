use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, Styled, Window, div, px,
};
use newjen_core::ApprovalIntent;

use super::{IconName, Look, Size, button, icon};
use gpui_kit::component::{ActiveTheme as _, Disableable as _};

pub struct DockMetrics {
    pub page_padding: f32,
    pub top_padding: f32,
    pub bottom_padding: f32,
    pub scope_gap: f32,
    pub padding: f32,
    pub border: f32,
    pub gap: f32,
    pub detail_x: f32,
    pub detail_y: f32,
    pub title_gap: f32,
    pub conversation_reserve: f32,
    pub title_rows: usize,
    pub detail_rows_min: usize,
    pub detail_rows_max: usize,
}

pub const DOCK_METRICS: DockMetrics = DockMetrics {
    page_padding: 16.,
    top_padding: 8.,
    bottom_padding: 16.,
    scope_gap: 8.,
    padding: 16.,
    border: 1.,
    gap: 12.,
    detail_x: 8.,
    detail_y: 6.,
    title_gap: 8.,
    conversation_reserve: 80.,
    title_rows: 2,
    detail_rows_min: 3,
    detail_rows_max: 9,
};

impl DockMetrics {
    pub fn detail_inset(&self) -> f32 {
        2. * (self.border + self.padding) + (self.detail_x * 2.).max(16. + self.title_gap)
    }

    pub fn chrome_height(&self, button_height: f32) -> f32 {
        2. * (self.border + self.padding + self.gap + self.detail_y) + button_height
    }
}

type OnAnswer = Rc<dyn Fn(ApprovalIntent, &mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ApprovalCard {
    id: ElementId,
    title: SharedString,
    detail: SharedString,
    on_answer: OnAnswer,
    preview: Option<AnyElement>,
    edit: bool,
    ready: bool,
    once_only: bool,
    selector_prefix: String,
    note: Option<Entity<gpui_kit::base::input::TextareaState>>,
}

impl ApprovalCard {
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        on_answer: impl Fn(ApprovalIntent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            detail: detail.into(),
            on_answer: Rc::new(on_answer),
            preview: None,
            edit: false,
            ready: true,
            once_only: false,
            selector_prefix: String::new(),
            note: None,
        }
    }
    /// Only Allow once and Deny: the inbox never grants for a session (that stays in the
    /// thread's own dock, where the scope row explains what is remembered).
    pub fn once_only(mut self) -> Self {
        self.once_only = true;
        self
    }
    /// Puts `prefix` before the buttons' debug selectors, so tests can tell cards apart.
    pub fn selectors(mut self, prefix: impl Into<String>) -> Self {
        self.selector_prefix = prefix.into();
        self
    }
    pub fn preview(mut self, preview: Option<AnyElement>, ready: bool) -> Self {
        self.edit = preview.is_some();
        self.preview = preview;
        self.ready = ready;
        self
    }
    pub fn note(mut self, note: Entity<gpui_kit::base::input::TextareaState>) -> Self {
        self.note = Some(note);
        self
    }
}

impl RenderOnce for ApprovalCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let m = &DOCK_METRICS;
        let c = cx.theme();
        let choice = |id: &'static str, label: &'static str, look, answer: ApprovalIntent| {
            let on_answer = self.on_answer.clone();
            let selector = format!("{}{id}", self.selector_prefix);
            button((self.id.clone(), id), label, look, Size::Md, cx)
                .disabled(!self.ready && !answer.is_deny())
                .debug_selector(move || selector.clone())
                .on_click(move |_, window, cx| on_answer(answer, window, cx))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(m.gap))
            .p(px(m.padding))
            .rounded(cx.theme().radius_lg)
            .border(px(m.border))
            .border_color(c.border)
            .bg(c.background)
            .text_sm()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(m.title_gap))
                    .child(icon(
                        IconName::Shield,
                        gpui_kit::component::Size::Medium,
                        c.muted_foreground,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.foreground)
                            .child(self.title),
                    ),
            )
            .children((!self.edit).then(|| {
                div().w_full().flex().child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(px(m.detail_x))
                        .py(px(m.detail_y))
                        .rounded(cx.theme().radius)
                        .bg(c.muted)
                        .font_family(c.mono_font_family.clone())
                        .text_color(c.muted_foreground)
                        .child(self.detail),
                )
            }))
            .children(self.preview)
            .children(self.note.map(|note| {
                gpui_kit::component::input::Textarea::new(&note)
                    .aria_label("Optional reason for denying")
            }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap(px(m.title_gap))
                    .child(choice("deny", "Deny", Look::Ghost, ApprovalIntent::Deny))
                    .children((!self.edit && !self.once_only).then(|| {
                        choice(
                            "always",
                            "Always allow",
                            Look::Secondary,
                            ApprovalIntent::AllowForSession,
                        )
                    }))
                    .child(choice(
                        "once",
                        "Allow once",
                        Look::Primary,
                        ApprovalIntent::AllowOnce,
                    )),
            )
    }
}

pub fn approval_dock(card: ApprovalCard, scope: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(DOCK_METRICS.scope_gap))
        .debug_selector(|| "approval-dock".to_string())
        .child(card)
        .child(scope)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{Context, FocusHandle, Modifiers, Render, TestAppContext};

    struct Example {
        focus: FocusHandle,
        answers: Vec<ApprovalIntent>,
    }

    impl Render for Example {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let card = ApprovalCard::new(
                "approval",
                "Allow Bash to run this command?",
                "$ echo first\nsecond\nlast",
                move |answer, _, cx| {
                    view.update(cx, |view, cx| {
                        view.answers.push(answer);
                        cx.notify();
                    });
                },
            );
            div()
                .track_focus(&self.focus)
                .w(px(608.))
                .font_family(cx.theme().font_family.clone())
                .child(approval_dock(card, div().h(px(18.)).into_any_element()))
        }
    }

    #[gpui::test]
    fn choices_fit_and_keep_their_answer_and_focus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
        });
        let (view, cx) = cx.add_window_view(|window, cx| {
            let focus = cx.focus_handle();
            focus.focus(window, cx);
            Example {
                focus,
                answers: vec![],
            }
        });
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        for (selector, answer) in [
            ("deny", ApprovalIntent::Deny),
            ("always", ApprovalIntent::AllowForSession),
            ("once", ApprovalIntent::AllowOnce),
        ] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(bounds.size.height >= gpui::px(32.));
            assert!(bounds.right() <= px(608.));
            cx.simulate_click(bounds.center(), Modifiers::none());
            assert_eq!(
                cx.update(|_, cx| view.read(cx).answers.last().copied()),
                Some(answer)
            );
            assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
        }
    }

    #[gpui::test]
    fn dock_stays_still_across_requests_and_reduce_motion(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
        });
        let (view, cx) = cx.add_window_view(|_, cx| Example {
            focus: cx.focus_handle(),
            answers: vec![],
        });
        cx.run_until_parked();
        let first = cx.debug_bounds("approval-dock").unwrap();
        cx.update(|_, cx| {
            cx.set_reduce_motion(true);
            view.update(cx, |_, cx| cx.notify());
        });
        cx.run_until_parked();
        assert_eq!(first, cx.debug_bounds("approval-dock").unwrap());
    }
}
