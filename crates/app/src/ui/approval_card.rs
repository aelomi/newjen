use std::{rc::Rc, time::Duration};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, ElementId, Entity, FontWeight,
    InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px,
};
use newjen_core::ApprovalIntent;

use super::{IconName, Look, Size, button, ellipsis, icon};
use gpui_kit::component::animation::{cubic_bezier, ease_out_cubic};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};

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

const ENTER: Duration = Duration::from_millis(180);
const EXIT: Duration = Duration::from_millis(120);
const RISE: f32 = 8.;
/// The detail block shows this many lines, then scrolls.
const DETAIL_LINES: f32 = 4.;
const DETAIL_LINE_HEIGHT: f32 = 20.;
const TILE: f32 = 32.;

/// How long a mouse-answered card keeps fading after its request is gone.
pub const EXIT_FADE: Duration = EXIT;

type OnAnswer = Rc<dyn Fn(ApprovalIntent, &mut Window, &mut App)>;
type OnExit = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct ApprovalCard {
    id: ElementId,
    title: SharedString,
    headline: Option<SharedString>,
    queue: Option<SharedString>,
    risks: Vec<&'static str>,
    /// The headline is Claude's own description of the command.
    by_model: bool,
    glyph: IconName,
    detail: SharedString,
    on_answer: OnAnswer,
    on_mouse_answer: Option<OnExit>,
    preview: Option<AnyElement>,
    edit: bool,
    ready: bool,
    remember: bool,
    ghost: bool,
    note: Option<Entity<gpui_kit::base::input::TextareaState>>,
}

impl ApprovalCard {
    /// `title` is the fixed question: the card's accessibility label, and its headline until
    /// [`ApprovalCard::headline`] gives a plainer one.
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        on_answer: impl Fn(ApprovalIntent, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            headline: None,
            queue: None,
            risks: Vec::new(),
            by_model: false,
            glyph: IconName::Shield,
            detail: detail.into(),
            on_answer: Rc::new(on_answer),
            on_mouse_answer: None,
            preview: None,
            edit: false,
            ready: true,
            remember: true,
            ghost: false,
            note: None,
        }
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
    /// The plain sentence in the card's header ("Delete 3 files").
    pub fn headline(mut self, headline: impl Into<SharedString>) -> Self {
        self.headline = Some(headline.into());
        self
    }
    /// Quiet text after the headline, such as "1 of 3".
    pub fn queue(mut self, queue: Option<SharedString>) -> Self {
        self.queue = queue;
        self
    }
    /// The small warning tags, most severe first: one sits at the header's right end, several
    /// on a row under it.
    pub fn risks(mut self, risks: Vec<&'static str>) -> Self {
        self.risks = risks;
        self
    }
    /// Says under the headline that Claude wrote it, and that the command below is what runs.
    pub fn by_model(mut self, by_model: bool) -> Self {
        self.by_model = by_model;
        self
    }
    /// The icon in the tile at the header's left end.
    pub fn glyph(mut self, glyph: IconName) -> Self {
        self.glyph = glyph;
        self
    }
    /// Whether the request offers "Allow in this thread" at all.
    pub fn can_remember(mut self, remember: bool) -> Self {
        self.remember = remember;
        self
    }
    /// Called after a mouse click has answered (not a key press), so the owner can keep this
    /// card on screen while it fades out.
    pub fn on_mouse_answer(mut self, exit: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_mouse_answer = Some(Rc::new(exit));
        self
    }
    /// What the owner keeps after a mouse answer: this card without its preview and note,
    /// drawn inert and fading out over [`EXIT_FADE`].
    pub fn exit_ghost(&self) -> Self {
        Self {
            id: self.id.clone(),
            title: self.title.clone(),
            headline: self.headline.clone(),
            queue: self.queue.clone(),
            risks: self.risks.clone(),
            by_model: self.by_model,
            glyph: self.glyph,
            detail: self.detail.clone(),
            on_answer: self.on_answer.clone(),
            on_mouse_answer: None,
            preview: None,
            edit: false,
            ready: true,
            remember: self.remember,
            ghost: true,
            note: None,
        }
    }
    pub fn is_edit(&self) -> bool {
        self.edit
    }
}

/// A key hint inside a button, in that button's own colors.
fn key_cap(text: &'static str, fg: gpui::Hsla) -> impl IntoElement {
    div()
        .flex_none()
        .px_1()
        .rounded_sm()
        .text_xs()
        .line_height(gpui::relative(1.2))
        .bg(fg.opacity(0.14))
        .text_color(fg.opacity(0.75))
        .child(text)
}

impl RenderOnce for ApprovalCard {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let m = &DOCK_METRICS;
        let c = cx.theme();
        let ghost = self.ghost;
        let on_exit = self.on_mouse_answer.clone();
        let choice = |id: &'static str,
                      label: &'static str,
                      look,
                      answer: ApprovalIntent,
                      keys: Option<(&'static str, gpui::Hsla)>| {
            let on_answer = self.on_answer.clone();
            let on_exit = on_exit.clone();
            let selector = id.to_string();
            let mut button = button((self.id.clone(), id), label, look, Size::Md, cx)
                .disabled(!self.ready && !answer.is_deny());
            if !ghost {
                button = button.debug_selector(move || selector.clone()).on_click(
                    move |event: &ClickEvent, window, cx| {
                        on_answer(answer, window, cx);
                        if !event.is_keyboard()
                            && let Some(exit) = &on_exit
                        {
                            exit(window, cx);
                        }
                    },
                );
            }
            match keys {
                Some((text, fg)) => button.child(key_cap(text, fg)),
                None => button,
            }
        };
        let headline = self.headline.clone().unwrap_or_else(|| self.title.clone());
        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .flex_none()
                    .size(px(TILE))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(c.radius_lg)
                    .bg(c.warning.opacity(0.14))
                    .child(icon(
                        self.glyph,
                        gpui_kit::component::Size::Medium,
                        c.warning,
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(c.foreground)
                            .child(ellipsis(headline)),
                    )
                    .when(self.by_model, |column| {
                        column.child(
                            div()
                                .id((self.id.clone(), "summary-by"))
                                .debug_selector(|| "approval-summary-by".to_string())
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child("Claude's summary")
                                .tooltip(|window, cx| {
                                    Tooltip::new(
                                        "Written by Claude. The command below is what will run.",
                                    )
                                    .build(window, cx)
                                }),
                        )
                    }),
            )
            .children(self.queue.map(|queue| {
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(queue)
            }))
            .children((self.risks.len() == 1).then(|| {
                div()
                    .flex_none()
                    .child(Tag::danger().small().child(self.risks[0]))
            }));
        // Several tags share a row under the header, lined up with the headline.
        let tags = (self.risks.len() > 1).then(|| {
            div()
                .debug_selector(|| "approval-risks".to_string())
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .pl(px(TILE + 12.))
                .children(
                    self.risks
                        .iter()
                        .map(|label| Tag::danger().small().child(*label)),
                )
        });
        let detail = (!self.edit).then(|| {
            div()
                .id((self.id.clone(), "detail"))
                .w_full()
                .max_h(px(DETAIL_LINES * DETAIL_LINE_HEIGHT + 2. * m.detail_y))
                .overflow_y_scroll()
                .px(px(m.detail_x))
                .py(px(m.detail_y))
                .rounded(c.radius)
                .bg(c.muted)
                .font_family(c.mono_font_family.clone())
                .line_height(px(DETAIL_LINE_HEIGHT))
                .text_color(c.muted_foreground)
                .child(self.detail)
        });
        let remember = self.remember && !self.edit;
        let actions = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(m.title_gap))
            .child(choice(
                "deny",
                "Deny",
                Look::Ghost,
                ApprovalIntent::Deny,
                Some(("esc", c.muted_foreground)),
            ))
            .child(div().flex_1())
            .children(remember.then(|| {
                choice(
                    "always",
                    "Allow in this thread",
                    Look::Outline,
                    ApprovalIntent::AllowForSession,
                    None,
                )
            }))
            .child(choice(
                "once",
                "Allow once",
                Look::Primary,
                ApprovalIntent::AllowOnce,
                Some(("⌘↵", c.primary_foreground)),
            ));
        let card = div()
            .id(self.id.clone())
            .aria_label(self.title.clone())
            .relative()
            .flex()
            .flex_col()
            .gap(px(m.gap))
            .p(px(m.padding))
            .rounded(c.radius_lg)
            .border(px(m.border))
            .border_color(c.warning.opacity(0.45))
            .bg(c.background)
            .text_sm()
            .child(header)
            .children(tags)
            .children(detail)
            .children(self.preview)
            .children(self.note.map(|note| {
                gpui_kit::component::input::Textarea::new(&note)
                    .aria_label("Optional reason for denying")
            }))
            .child(actions);
        if ghost {
            card.with_animation(
                (self.id, "exit"),
                Animation::new(EXIT).with_easing(ease_out_cubic),
                |card, t| card.opacity(1. - t),
            )
            .into_any_element()
        } else {
            card.with_animation(
                (self.id, "enter"),
                Animation::new(ENTER).with_easing(cubic_bezier(0.23, 1., 0.32, 1.)),
                |card, t| card.top(px(RISE * (t - 1.))).opacity(t),
            )
            .into_any_element()
        }
    }
}

pub fn approval_dock(card: ApprovalCard, scope: Option<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(DOCK_METRICS.scope_gap))
        .debug_selector(|| "approval-dock".to_string())
        .child(card)
        .children(scope)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{Context, FocusHandle, Modifiers, Render, TestAppContext};

    struct Example {
        focus: FocusHandle,
        answers: Vec<ApprovalIntent>,
        exits: usize,
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
            )
            .headline("Run a very long command that does not fit on a single line of the card")
            .risks(vec!["Deletes files", "Uses the network"])
            .by_model(true)
            .glyph(IconName::Trash)
            .on_mouse_answer({
                let view = cx.entity();
                move |_, cx| view.update(cx, |view, _| view.exits += 1)
            });
            div()
                .track_focus(&self.focus)
                .w(px(608.))
                .font_family(cx.theme().font_family.clone())
                .child(approval_dock(
                    card,
                    Some(div().h(px(18.)).into_any_element()),
                ))
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
                exits: 0,
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
        // Each mouse click told the owner, which keeps the card to fade it out.
        assert_eq!(cx.update(|_, cx| view.read(cx).exits), 3);
    }

    #[gpui::test]
    fn dock_stays_still_across_requests_and_reduce_motion(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
        });
        let (view, cx) = cx.add_window_view(|_, cx| Example {
            focus: cx.focus_handle(),
            answers: vec![],
            exits: 0,
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
