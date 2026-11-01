use std::rc::Rc;
use std::time::Duration;

use super::{IconName, ToolBody, icon};
use gpui::{
    AnyElement, App, ElementId, Entity, FontFeatures, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, prelude::*, px,
    relative,
};
use gpui_kit::base::{MotionReveal, MotionStatus, Transition, animation::cubic_bezier};
use gpui_kit::base::{h_flex, transition_with_status, v_flex};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    spinner::Spinner,
    text::{TextView, TextViewState},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Working,
    Done,
    Failed,
}

pub fn step_mark(state: StepState, cx: &App) -> AnyElement {
    let (name, color) = match state {
        StepState::Waiting | StepState::Working => (IconName::Circle, cx.theme().muted_foreground),
        StepState::Done => (IconName::Check, cx.theme().success),
        StepState::Failed => (IconName::Close, cx.theme().danger),
    };
    Icon::new(name).text_color(color).into_any_element()
}

/// The height and fade of a block settling or a row opening (the motion sheet's "Work block
/// settles").
const REVEAL: Duration = Duration::from_millis(200);
const BLOCK_HEADER: f32 = 36.;
const STEP_ROW: f32 = 28.;
/// A row's mark and the gap after it, which a note under the row lines up with.
const MARK: f32 = 16.;
const ROW_PAD: f32 = 12.;
const ROW_GAP: f32 = 8.;

/// How a step or a block reads at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Working,
    Waiting,
    Done,
    Failed,
    /// A call cut short, not a failure: a muted icon.
    Quiet(QuietMark),
    /// A turn the user stopped.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietMark {
    Expired,
    Paused,
}

pub fn mark(mark: Mark, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let small = gpui_kit::component::Size::XSmall;
    let (name, color) = match mark {
        Mark::Working => {
            return Spinner::new()
                .with_size(small)
                .color(theme.cyan)
                .into_any_element();
        }
        Mark::Waiting => (IconName::CirclePause, theme.warning),
        Mark::Done => (IconName::Check, theme.success),
        Mark::Failed => (IconName::Close, theme.danger),
        Mark::Quiet(QuietMark::Expired) => (IconName::Clock, theme.muted_foreground),
        Mark::Quiet(QuietMark::Paused) => (IconName::CirclePause, theme.muted_foreground),
        Mark::Stopped => (IconName::Square, theme.muted_foreground),
    };
    icon(name, small, color).into_any_element()
}

fn mark_slot(inner: AnyElement) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(MARK))
        .flex()
        .items_center()
        .justify_center()
        .child(inner)
}

/// One call's raw arguments and result, for a step opened by click.
#[derive(Clone)]
pub struct Detail {
    pub id: u64,
    /// Names the call when a step covers several ("Read src/lib.rs").
    pub title: Option<SharedString>,
    pub arguments: Option<SharedString>,
    pub result: Option<SharedString>,
    pub failed: bool,
}

/// A step's raw calls, read when its row is opened.
pub type Details = Rc<dyn Fn(&App) -> Vec<Detail>>;

/// A block's rows, read while it is open.
pub type Parts = Rc<dyn Fn(&App) -> Vec<Part>>;

/// One row of a work block.
pub struct StepView {
    pub key: ElementId,
    pub mark: Mark,
    pub headline: SharedString,
    pub detail: Option<SharedString>,
    /// The detail is a command: drawn in the mono font.
    pub mono: bool,
    /// The row waits for the user: a faint warning tint.
    pub waiting: bool,
    /// Where the time would go, for a call that never ran ("Stopped when Newjen quit").
    pub note: Option<SharedString>,
    /// What the user wrote when denying it, quoted under the row.
    pub quote: Option<SharedString>,
    /// Lines added and removed, for a step that edits files.
    pub stat: Option<(usize, usize)>,
    /// Read when the row is opened: the raw calls it covers.
    pub details: Details,
    pub opens: bool,
}

pub enum Part {
    Step(StepView),
    /// What the agent said between two steps.
    Narration(Narration),
}

/// A reply written between two steps: markdown like any reply, quieter, and cut to a few lines
/// with a "Show more" when it is longer.
pub struct Narration {
    pub key: ElementId,
    pub text: SharedString,
    /// Its rendered markdown. Without one (never in the app) the text shows as plain lines.
    pub state: Option<Entity<TextViewState>>,
}

/// Lines of a narration shown before "Show more".
const NARRATION_LINES: usize = 6;

#[derive(IntoElement)]
struct NarrationRow {
    narration: Narration,
}

impl RenderOnce for NarrationRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Narration { key, text, state } = self.narration;
        let ui = window.use_keyed_state((key.clone(), "ui"), cx, |_, _| Disclosure::default());
        let expanded = ui.read(cx).forced.unwrap_or(false);
        // The text finds out after it is painted whether it was cut: redraw when that changes.
        let watched = state.clone();
        window.use_keyed_state((key.clone(), "watch"), cx, move |_, cx| {
            if let Some(state) = &watched {
                cx.observe(state, |_, _, cx| cx.notify()).detach();
            }
        });
        let muted = cx.theme().muted_foreground;
        let (body, more) = match state {
            Some(state) => {
                let clamped = state.read(cx).is_clamped();
                let view = TextView::new(&state)
                    .selectable(false)
                    .stream_fade(false)
                    .code_block_actions(|block, _, _| {
                        super::CopyButton::new("copy", block.code().to_string())
                    });
                let view = if expanded {
                    view
                } else {
                    view.max_lines(NARRATION_LINES)
                };
                (view.into_any_element(), clamped || expanded)
            }
            None => (div().line_clamp(4).child(text).into_any_element(), false),
        };
        v_flex()
            .debug_selector(|| "narration".to_string())
            .pr(px(ROW_PAD))
            .py_1()
            .pl(px(ROW_PAD + MARK + ROW_GAP))
            .text_color(muted)
            .text_size(gpui::rems(0.8125))
            .child(body)
            .when(more, |row| {
                row.child(
                    super::button(
                        (key, "more"),
                        if expanded { "Show less" } else { "Show more" },
                        super::Look::Ghost,
                        super::Size::Sm,
                        cx,
                    )
                    .debug_selector(|| "narration-more".to_string())
                    .on_click(move |event, _, cx| toggle(&ui, expanded, event.is_keyboard(), cx)),
                )
            })
    }
}

/// The whole block, as the transcript row draws it. Its steps are built only while it is open.
#[derive(IntoElement)]
pub struct WorkBlockView {
    id: ElementId,
    mark: Mark,
    title: SharedString,
    elapsed: Option<SharedString>,
    count: SharedString,
    live: bool,
    parts: Parts,
}

impl WorkBlockView {
    pub fn new(
        id: impl Into<ElementId>,
        mark: Mark,
        title: impl Into<SharedString>,
        count: impl Into<SharedString>,
        live: bool,
        parts: Parts,
    ) -> Self {
        Self {
            id: id.into(),
            mark,
            title: title.into(),
            elapsed: None,
            count: count.into(),
            live,
            parts,
        }
    }

    pub fn elapsed(mut self, elapsed: Option<SharedString>) -> Self {
        self.elapsed = elapsed;
        self
    }
}

/// What the user chose for one disclosure, and whether the choice came from the keyboard (which
/// never animates).
#[derive(Clone, Copy, Default)]
struct Disclosure {
    forced: Option<bool>,
    instant: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Reveal {
    Hidden,
    Shown,
    Moving(f32),
}

/// Whether the content under a disclosure is drawn, and how far it has opened. Keyboard
/// toggles and Reduce Motion jump straight to the end.
fn reveal(id: &ElementId, open: bool, instant: bool, window: &mut Window, cx: &mut App) -> Reveal {
    let policy = Transition::new(if instant { Duration::ZERO } else { REVEAL })
        .ease(cubic_bezier(0.77, 0.0, 0.175, 1.0));
    let moved = transition_with_status(
        (id.clone(), "reveal"),
        if open { 1.0f32 } else { 0.0 },
        policy,
        window,
        cx,
    );
    match (moved.status, open) {
        (MotionStatus::Idle | MotionStatus::Finished, true) => Reveal::Shown,
        (MotionStatus::Idle | MotionStatus::Finished, false) => Reveal::Hidden,
        _ => Reveal::Moving(moved.value),
    }
}

fn revealed(id: &ElementId, state: Reveal, content: AnyElement) -> Option<AnyElement> {
    match state {
        Reveal::Hidden => None,
        Reveal::Shown => Some(content),
        Reveal::Moving(progress) => Some(
            MotionReveal::new(
                (id.clone(), "clip"),
                progress,
                div().opacity(progress).child(content).into_any_element(),
            )
            .into_any_element(),
        ),
    }
}

fn toggle(state: &gpui::Entity<Disclosure>, shown: bool, keyboard: bool, cx: &mut App) {
    state.update(cx, |state, cx| {
        state.forced = Some(!shown);
        state.instant = keyboard;
        cx.notify();
    });
}

fn tnum() -> FontFeatures {
    FontFeatures(std::sync::Arc::new(vec![("tnum".into(), 1)]))
}

fn one_line(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text.into())
}

impl RenderOnce for WorkBlockView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ui = window.use_keyed_state((self.id.clone(), "ui"), cx, |_, _| Disclosure::default());
        let choice = *ui.read(cx);
        let open = choice.forced.unwrap_or(self.live);
        let state = reveal(&self.id, open, choice.instant, window, cx);
        let theme = cx.theme();
        let faint = crate::theme::material(cx).glass_hover;
        let header = Button::new((self.id.clone(), "header"))
            .debug_selector(|| "work-block-header".to_string())
            .ghost()
            .w_full()
            .h(px(BLOCK_HEADER))
            .px(px(ROW_PAD))
            .rounded(px(0.))
            .toggled(open)
            .accessibility_label(format!(
                "{}, {}, {}",
                self.title,
                self.count,
                if open { "expanded" } else { "collapsed" }
            ))
            .on_click({
                let ui = ui.clone();
                move |event, _, cx| toggle(&ui, open, event.is_keyboard(), cx)
            })
            .gap(px(ROW_GAP))
            .child(mark_slot(mark(self.mark, cx)))
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(self.title.clone()),
            )
            .children(self.elapsed.map(|elapsed| {
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .font_features(tnum())
                    .child(elapsed)
            }))
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_color(theme.muted_foreground)
                    .font_features(tnum())
                    .child(self.count.clone()),
            )
            .child(icon(
                if open {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                },
                gpui_kit::component::Size::Small,
                theme.muted_foreground,
            ));
        let content = (state != Reveal::Hidden).then(|| {
            let parts = (self.parts)(cx);
            v_flex()
                .debug_selector(|| "work-block-steps".to_string())
                .pb_1()
                .children(parts.into_iter().map(|part| match part {
                    Part::Step(step) => StepRow { step }.into_any_element(),
                    Part::Narration(narration) => NarrationRow { narration }.into_any_element(),
                }))
                .into_any_element()
        });
        let theme = cx.theme();
        v_flex()
            .debug_selector(|| "work-block".to_string())
            .w_full()
            .overflow_hidden()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .when(!open, |block| block.bg(faint))
            .text_size(gpui::rems(0.875))
            .child(header)
            .children(content.and_then(|content| revealed(&self.id, state, content)))
    }
}

#[derive(IntoElement)]
struct StepRow {
    step: StepView,
}

impl RenderOnce for StepRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let step = self.step;
        let id = step.key.clone();
        let ui = window.use_keyed_state((id.clone(), "ui"), cx, |_, _| Disclosure::default());
        let choice = *ui.read(cx);
        let open = step.opens && choice.forced.unwrap_or(false);
        let state = reveal(&id, open, choice.instant, window, cx);
        let theme = cx.theme();
        let tint = theme.warning.opacity(0.08);
        let mut line = h_flex()
            .size_full()
            .min_w_0()
            .items_center()
            .gap(px(ROW_GAP))
            .child(mark_slot(mark(step.mark, cx)))
            .child(
                one_line(step.headline.clone())
                    .flex_shrink_0()
                    .max_w(relative(0.62))
                    .text_color(theme.foreground),
            );
        line = match step.detail.clone() {
            Some(detail) => {
                let detail = one_line(detail).flex_1().text_color(theme.muted_foreground);
                line.child(if step.mono {
                    detail
                        .font_family(theme.mono_font_family.clone())
                        .text_size(gpui::rems(0.75))
                } else {
                    detail
                })
            }
            None => line.child(div().flex_1()),
        };
        if let Some(note) = step.note.clone() {
            line = line.child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(theme.muted_foreground)
                    .child(note),
            );
        }
        if let Some((added, removed)) = step.stat {
            line = line.child(stat(added, removed, cx));
        }
        let row = if step.opens {
            Button::new(id.clone())
                .debug_selector(|| "step-row".to_string())
                .ghost()
                .w_full()
                .h(px(STEP_ROW))
                .px(px(ROW_PAD))
                .rounded(px(0.))
                .toggled(open)
                .accessibility_label(format!(
                    "{}, {}",
                    step.headline,
                    if open { "expanded" } else { "collapsed" }
                ))
                .when(step.waiting, |button| button.bg(tint))
                .on_click({
                    let ui = ui.clone();
                    move |event, _, cx| toggle(&ui, open, event.is_keyboard(), cx)
                })
                .child(line)
                .into_any_element()
        } else {
            div()
                .debug_selector(|| "step-row".to_string())
                .w_full()
                .h(px(STEP_ROW))
                .px(px(ROW_PAD))
                .flex()
                .items_center()
                .when(step.waiting, |row| row.bg(tint))
                .child(line)
                .into_any_element()
        };
        let details = (state != Reveal::Hidden).then(|| {
            let calls = (step.details)(cx);
            let many = calls.len() > 1;
            let theme = cx.theme();
            v_flex()
                .gap_2()
                .pr(px(ROW_PAD))
                .pl(px(ROW_PAD + MARK + ROW_GAP))
                .pt_1()
                .pb_2()
                .children(calls.into_iter().map(|call| {
                    v_flex()
                        .gap_1()
                        .children(call.title.filter(|_| many).map(|title| {
                            one_line(title)
                                .font_family(theme.mono_font_family.clone())
                                .text_size(gpui::rems(0.75))
                                .text_color(theme.muted_foreground)
                        }))
                        .child(
                            ToolBody::new(("call", call.id))
                                .arguments(call.arguments)
                                .result(call.result, call.failed),
                        )
                }))
                .into_any_element()
        });
        let muted = cx.theme().muted_foreground;
        v_flex()
            .w_full()
            .child(row)
            .children(step.quote.map(|quote| {
                div()
                    .pl(px(ROW_PAD + MARK + ROW_GAP))
                    .pr(px(ROW_PAD))
                    .pb_1p5()
                    .text_color(muted)
                    .line_clamp(2)
                    .child(quote)
            }))
            .children(details.and_then(|details| revealed(&id, state, details)))
    }
}

fn stat(added: usize, removed: usize, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .flex_none()
        .gap_1p5()
        .font_features(tnum())
        .when(added > 0, |line| {
            line.child(
                div()
                    .text_color(theme.success)
                    .child(format!("+{}", crate::format::thousands(added))),
            )
        })
        .when(removed > 0, |line| {
            line.child(
                div()
                    .text_color(theme.danger)
                    .child(format!("\u{2212}{}", crate::format::thousands(removed))),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Modifiers, Render, TestAppContext};

    struct Example {
        live: bool,
        long: bool,
    }

    impl Render for Example {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let long = self.long;
            let step = move |id: u64, headline: &str, waiting: bool| {
                Part::Step(StepView {
                    key: ("step", id).into(),
                    mark: if waiting { Mark::Waiting } else { Mark::Done },
                    headline: if long {
                        "Ran ".repeat(80)
                    } else {
                        headline.to_string()
                    }
                    .into(),
                    detail: Some(if long {
                        "very/long/path/".repeat(60).into()
                    } else {
                        "src/lib.rs".into()
                    }),
                    mono: long,
                    waiting,
                    note: None,
                    quote: None,
                    stat: Some((12_408, 4)),
                    details: Rc::new(|_| {
                        vec![Detail {
                            id: 1,
                            title: None,
                            arguments: Some("{}".into()),
                            result: Some("ok".into()),
                            failed: false,
                        }]
                    }),
                    opens: true,
                })
            };
            div().w(px(640.)).child(WorkBlockView::new(
                ("block", 1u64),
                Mark::Done,
                "Worked for 4m 3s",
                "2 steps",
                self.live,
                Rc::new(move |_| {
                    vec![
                        step(1, "Read 3 files", false),
                        Part::Narration(Narration {
                            key: ("narration", 1u64).into(),
                            text: "Now the tests.".into(),
                            state: None,
                        }),
                        step(2, "Run the tests", true),
                    ]
                }),
            ))
        }
    }

    fn block(live: bool, long: bool, cx: &mut TestAppContext) -> &mut gpui::VisualTestContext {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (_, cx) = cx.add_window_view(|_, _| Example { live, long });
        cx.run_until_parked();
        cx
    }

    #[gpui::test]
    fn a_finished_block_is_one_line_until_clicked(cx: &mut TestAppContext) {
        let cx = block(false, false, cx);
        assert!(cx.debug_bounds("work-block-header").is_some());
        assert!(cx.debug_bounds("work-block-steps").is_none());
        let header = cx.debug_bounds("work-block-header").unwrap();
        cx.simulate_click(header.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("work-block-steps").is_some());
        let header = cx.debug_bounds("work-block-header").unwrap();
        cx.simulate_click(header.center(), Modifiers::none());
        cx.run_until_parked();
        assert!(cx.debug_bounds("work-block-steps").is_none());
    }

    #[gpui::test]
    fn a_live_block_shows_its_steps_and_a_step_opens_its_raw_calls(cx: &mut TestAppContext) {
        let cx = block(true, false, cx);
        let steps = cx.debug_bounds("work-block-steps").unwrap();
        assert!(cx.debug_bounds("tool-details").is_none());
        cx.simulate_click(
            gpui::point(steps.center().x, steps.origin.y + px(STEP_ROW / 2.)),
            Modifiers::none(),
        );
        cx.run_until_parked();
        assert!(cx.debug_bounds("tool-details").is_some());
    }

    struct Said {
        state: Entity<TextViewState>,
    }

    impl Render for Said {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let state = self.state.clone();
            div().w(px(640.)).child(WorkBlockView::new(
                ("block", 2u64),
                Mark::Working,
                "Working",
                "1 step",
                true,
                Rc::new(move |_| {
                    vec![Part::Narration(Narration {
                        key: ("narration", 2u64).into(),
                        text: "plain".into(),
                        state: Some(state.clone()),
                    })]
                }),
            ))
        }
    }

    fn said<'a>(
        text: &str,
        cx: &'a mut TestAppContext,
    ) -> (Entity<TextViewState>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let state =
            cx.update(|cx| cx.new(|cx| TextViewState::markdown(text, cx).selectable(false)));
        let shown = state.clone();
        let (_, cx) = cx.add_window_view(move |_, _| Said { state: shown });
        cx.run_until_parked();
        (state, cx)
    }

    #[gpui::test]
    fn what_the_agent_says_between_steps_is_rendered_as_markdown(cx: &mut TestAppContext) {
        let (state, cx) = said("## What's going on\n\nIt ran **twice**.", cx);
        let text = cx.update(|_, cx| state.read(cx).rendered_text().as_str().to_string());
        assert!(
            text.contains("What's going on") && text.contains("twice"),
            "{text}"
        );
        assert!(!text.contains("##") && !text.contains("**"), "{text}");
        assert!(cx.debug_bounds("narration").is_some());
        // Short enough to need no "Show more".
        assert!(cx.debug_bounds("narration-more").is_none());
    }

    #[gpui::test]
    fn a_long_narration_is_cut_with_a_show_more(cx: &mut TestAppContext) {
        let long = (1..=30)
            .map(|n| format!("Line {n} of what the agent wrote."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let (_, cx) = said(&long, cx);
        cx.run_until_parked();
        let cut = cx.debug_bounds("narration").unwrap().size.height;
        assert!(cut < px(400.), "{cut:?}");
        let more = cx.debug_bounds("narration-more").expect("a Show more");
        cx.simulate_click(more.center(), Modifiers::none());
        cx.run_until_parked();
        let open = cx.debug_bounds("narration").unwrap().size.height;
        assert!(open > cut, "{open:?} {cut:?}");
        let less = cx.debug_bounds("narration-more").expect("a Show less");
        cx.simulate_click(less.center(), Modifiers::none());
        cx.run_until_parked();
        assert_eq!(cx.debug_bounds("narration").unwrap().size.height, cut);
    }

    #[gpui::test]
    fn long_commands_and_huge_counts_stay_on_one_line_inside_the_block(cx: &mut TestAppContext) {
        let cx = block(true, true, cx);
        let block = cx.debug_bounds("work-block").unwrap();
        assert_eq!(block.size.width, px(640.));
        let row = cx.debug_bounds("step-row").unwrap();
        assert_eq!(row.size.height, px(STEP_ROW));
        assert!(row.size.width <= block.size.width);
        let header = cx.debug_bounds("work-block-header").unwrap();
        assert_eq!(header.size.height, px(BLOCK_HEADER));
    }
}
