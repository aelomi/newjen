use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, SharedString, Styled as _,
    Window, px,
};
use gpui_kit::base::h_flex;
use newjen_core::providers::Provider;

use super::{Tick, Tween, provider_icon, ring_with_tick};

const RING: f32 = 16.0;
const RING_WIDTH: f32 = 2.0;
const MARK: f32 = 12.0;
const MARK_GAP: f32 = 5.0;
const LABEL_GAP: f32 = 6.0;
const LABEL_MAX: f32 = 120.0;
const ARC: Duration = Duration::from_millis(300);
const SLIDE: Duration = Duration::from_millis(200);
const TONE: Duration = Duration::from_millis(200);

/// What sits beside the ring when a limit is near: the provider's mark and a few words.
#[derive(Clone, Debug, PartialEq)]
pub struct GaugeLabel {
    pub provider: Provider,
    pub text: SharedString,
}

/// How the ring and its label look now.
#[derive(Clone, Copy, Debug)]
pub struct GaugeLook {
    pub fraction: f32,
    pub tick: Option<f32>,
    pub ink: Hsla,
    pub track: Hsla,
    pub tick_ink: Hsla,
}

/// The usage ring with the label that slides out of it. It remembers what it last showed, so a
/// new report eases the arc from where it was, a tone change crossfades, and the label slides
/// back in after the limit is no longer near. Under Reduce Motion the arc and the label's width
/// jump, and only the fades remain.
pub struct UsageGauge {
    id: ElementId,
    look: GaugeLook,
    label: Option<GaugeLabel>,
}

impl UsageGauge {
    pub fn new(id: impl Into<ElementId>, look: GaugeLook, label: Option<GaugeLabel>) -> Self {
        Self {
            id: id.into(),
            look,
            label,
        }
    }
}

impl IntoElement for UsageGauge {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct State {
    fraction: Tween<f32>,
    ink: Tween<Hsla>,
    shown: Tween<f32>,
    label: Option<GaugeLabel>,
}

impl State {
    fn new(gauge: &UsageGauge, now: Instant) -> Self {
        Self {
            fraction: Tween::at(gauge.look.fraction, now),
            ink: Tween::at(gauge.look.ink, now),
            shown: Tween::at(if gauge.label.is_some() { 1.0 } else { 0.0 }, now),
            label: gauge.label.clone(),
        }
    }

    fn settled(&self, now: Instant) -> bool {
        self.fraction.settled(now, ARC)
            && self.ink.settled(now, TONE)
            && self.shown.settled(now, SLIDE)
    }
}

impl Element for UsageGauge {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        window.with_element_state(global_id.expect("a keyed element"), |state, window| {
            let now = cx.background_executor().now();
            let reduce_motion = cx.reduce_motion();
            let mut state = state.unwrap_or_else(|| State::new(self, now));
            state
                .fraction
                .aim(self.look.fraction, now, ARC, !reduce_motion);
            state.ink.aim(self.look.ink, now, TONE, true);
            state.shown.aim(
                if self.label.is_some() { 1.0 } else { 0.0 },
                now,
                SLIDE,
                true,
            );
            if self.label.is_some() {
                state.label.clone_from(&self.label);
            }
            let shown = state.shown.value(now, SLIDE);
            if self.label.is_none() && shown <= 0.0 {
                state.label = None;
            }
            let ink = state.ink.value(now, TONE);
            let label = state.label.as_ref().map(|label| {
                let reach = if reduce_motion {
                    f32::from(shown > 0.0)
                } else {
                    shown
                };
                label_view(label, shown, reach, ink, cx)
            });
            let ring = ring_with_tick(
                state.fraction.value(now, ARC),
                px(RING),
                px(RING_WIDTH),
                px(1.),
                ink,
                self.look.track,
                self.look.tick.map(|at| Tick {
                    at,
                    color: self.look.tick_ink,
                    width: px(1.2),
                    reach: px(1.7),
                }),
            );
            let mut element = h_flex()
                .items_center()
                .children(label)
                .child(ring)
                .into_any_element();
            if !state.settled(now) {
                window.request_animation_frame();
            }
            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

/// The label at `shown` of the way out of the ring: `reach` of its width, `shown` of its opacity.
fn label_view(label: &GaugeLabel, shown: f32, reach: f32, ink: Hsla, cx: &App) -> AnyElement {
    h_flex()
        .debug_selector(|| "usage-label".to_string())
        .justify_end()
        .overflow_hidden()
        .max_w(px(LABEL_MAX * reach))
        .mr(px(LABEL_GAP * reach))
        .opacity(shown)
        .child(
            h_flex()
                .flex_none()
                .items_center()
                .whitespace_nowrap()
                .text_xs()
                .text_color(ink)
                .child(
                    provider_icon(label.provider, cx)
                        .size(px(MARK))
                        .mr(px(MARK_GAP)),
                )
                .child(label.text.clone()),
        )
        .into_any_element()
}
