use crate::ui::IconName;
use gpui::InteractiveElement as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, BorderStyle, Bounds, Div, ElementId,
    IntoElement, ParentElement, RenderOnce, Styled, Window, canvas, div, outline, point, size,
};
use gpui_kit::component::{ActiveTheme as _, Icon, tag::Tag};
use std::time::Duration;

const RING: Duration = Duration::from_millis(1600);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Warning,
}

pub fn status_badge(label: impl Into<gpui::SharedString>, tone: Tone) -> Tag {
    match tone {
        Tone::Neutral => Tag::secondary(),
        Tone::Info => Tag::info(),
        Tone::Warning => Tag::warning(),
    }
    .child(label.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Failed,
}

pub fn agent_pill(state: AgentState) -> Tag {
    match state {
        AgentState::Working => Tag::info()
            .child(Icon::new(IconName::Circle))
            .child("Working"),
        AgentState::Failed => Tag::danger()
            .child(Icon::new(IconName::Close))
            .child("Failed"),
    }
    .gap_1()
}

/// What it holds, with a thin ring that widens to twice its size and fades, over and over.
/// No ring at all under Reduce Motion: a still ring would read as a different state.
#[derive(IntoElement)]
pub struct Pulse {
    id: ElementId,
    base: Div,
    held: Vec<AnyElement>,
}

impl Pulse {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            base: div(),
            held: Vec::new(),
        }
    }
}

impl Styled for Pulse {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for Pulse {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.held.extend(elements);
    }
}

impl RenderOnce for Pulse {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let held = self.base.relative().children(self.held);
        if cx.reduce_motion() {
            return held.into_any_element();
        }
        let color = cx.theme().primary;
        let ring = div()
            .debug_selector(|| "pulse-ring".to_string())
            .absolute()
            .inset_0()
            .with_animation(self.id, Animation::new(RING).repeat(), move |ring, t| {
                ring.child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let side = bounds.size.width.max(bounds.size.height);
                            let radius = side / 2. * (1. + t);
                            let reach = bounds.center();
                            let corner = point(reach.x - radius, reach.y - radius);
                            window.paint_quad(
                                outline(
                                    Bounds::new(corner, size(radius * 2., radius * 2.)),
                                    color.opacity(0.5 * (1. - t)),
                                    BorderStyle::Solid,
                                )
                                .corner_radii(radius),
                            );
                        },
                    )
                    .size_full(),
                )
            });
        held.child(ring).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{Context, Render, TestAppContext, px};

    use super::*;

    struct WorkingDot;

    impl Render for WorkingDot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(
                Pulse::new("dot")
                    .size(px(7.))
                    .child(div().size(px(7.)).rounded_full()),
            )
        }
    }

    fn rings(reduce_motion: bool, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| {
            gpui_kit::init(cx);

            cx.set_reduce_motion(reduce_motion);
        });
        let (_, cx) = cx.add_window_view(|_, _| WorkingDot);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        cx.debug_bounds("pulse-ring").is_some()
    }

    #[gpui::test]
    fn the_working_dot_has_no_ring_under_reduce_motion(cx: &mut TestAppContext) {
        assert!(!rings(true, cx));
    }

    #[gpui::test]
    fn the_working_dot_rings_when_motion_is_allowed(cx: &mut TestAppContext) {
        assert!(rings(false, cx));
    }
}
