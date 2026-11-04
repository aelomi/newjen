use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, Rgba, SharedString,
    Styled as _, Window, px,
};
use gpui_kit::base::h_flex;
use gpui_kit::component::animation::ease_out_cubic;
use newjen_core::providers::Provider;

use super::{Tick, provider_icon, ring_with_tick};

const RING: f32 = 16.0;
const RING_WIDTH: f32 = 2.0;
const MARK: f32 = 12.0;
const MARK_GAP: f32 = 5.0;
const LABEL_GAP: f32 = 6.0;
const LABEL_MAX: f32 = 120.0;
const ARC: Duration = Duration::from_millis(300);
const SLIDE: Duration = Duration::from_millis(200);
const TONE: Duration = Duration::from_millis(200);

/// A value moving from `from` to `to`, started at `start`.
#[derive(Clone, Copy, Debug)]
struct Tween<T> {
    from: T,
    to: T,
    start: Instant,
}

trait Mix: Copy + PartialEq {
    fn mix(from: Self, to: Self, by: f32) -> Self;
}

impl Mix for f32 {
    fn mix(from: Self, to: Self, by: f32) -> Self {
        from + (to - from) * by
    }
}

impl Mix for Hsla {
    fn mix(from: Self, to: Self, by: f32) -> Self {
        let (from, to) = (Rgba::from(from), Rgba::from(to));
        Rgba {
            r: f32::mix(from.r, to.r, by),
            g: f32::mix(from.g, to.g, by),
            b: f32::mix(from.b, to.b, by),
            a: f32::mix(from.a, to.a, by),
        }
        .into()
    }
}

impl<T: Mix> Tween<T> {
    fn at(value: T, now: Instant) -> Self {
        Self {
            from: value,
            to: value,
            start: now,
        }
    }

    fn progress(&self, now: Instant, length: Duration) -> f32 {
        (now.saturating_duration_since(self.start).as_secs_f32() / length.as_secs_f32())
            .clamp(0.0, 1.0)
    }

    fn value(&self, now: Instant, length: Duration) -> T {
        T::mix(
            self.from,
            self.to,
            ease_out_cubic(self.progress(now, length)),
        )
    }

    fn settled(&self, now: Instant, length: Duration) -> bool {
        self.progress(now, length) >= 1.0
    }

    /// Heads for `target` from wherever the value is now, or jumps there. A target it already
    /// heads for changes nothing.
    fn aim(&mut self, target: T, now: Instant, length: Duration, animate: bool) {
        if target == self.to {
            return;
        }
        let from = if animate {
            self.value(now, length)
        } else {
            target
        };
        *self = Self {
            from,
            to: target,
            start: now,
        };
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::hsla;

    fn at(start: Instant, millis: u64) -> Instant {
        start + Duration::from_millis(millis)
    }

    #[test]
    fn a_new_target_eases_out_over_its_length() {
        let start = Instant::now();
        let mut arc = Tween::at(0.4_f32, start);
        arc.aim(0.8, start, ARC, true);
        assert_eq!(arc.value(start, ARC), 0.4);
        let halfway = arc.value(at(start, 150), ARC);
        assert!(halfway > 0.6 && halfway < 0.8, "{halfway}");
        assert!(!arc.settled(at(start, 299), ARC));
        assert!(arc.settled(at(start, 300), ARC));
        assert!((arc.value(at(start, 900), ARC) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn a_new_report_mid_flight_starts_from_where_the_value_is() {
        let start = Instant::now();
        let mut arc = Tween::at(0.0_f32, start);
        arc.aim(1.0, start, ARC, true);
        let seen = arc.value(at(start, 100), ARC);
        arc.aim(0.5, at(start, 100), ARC, true);
        assert!((arc.value(at(start, 100), ARC) - seen).abs() < 1e-6);
        assert!((arc.value(at(start, 400), ARC) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_same_target_does_not_restart_the_move() {
        let start = Instant::now();
        let mut arc = Tween::at(0.0_f32, start);
        arc.aim(1.0, start, ARC, true);
        let before = arc.value(at(start, 120), ARC);
        arc.aim(1.0, at(start, 120), ARC, true);
        assert_eq!(arc.value(at(start, 120), ARC), before);
        assert!(arc.settled(at(start, 300), ARC));
    }

    #[test]
    fn a_move_that_may_not_animate_jumps() {
        let start = Instant::now();
        let mut arc = Tween::at(0.1_f32, start);
        arc.aim(0.9, start, ARC, false);
        assert_eq!(arc.value(start, ARC), 0.9);
    }

    #[test]
    fn a_tone_crossfades_through_the_colors_between() {
        let start = Instant::now();
        let grey = hsla(0.0, 0.0, 0.5, 1.0);
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let mut ink = Tween::at(grey, start);
        ink.aim(red, start, TONE, true);
        assert_eq!(ink.value(start, TONE), grey);
        let middle = Rgba::from(ink.value(at(start, 100), TONE));
        let (from, to) = (Rgba::from(grey), Rgba::from(red));
        assert!(middle.r > from.r.min(to.r) && middle.r < from.r.max(to.r));
        assert!(middle.g > to.g && middle.g < from.g);
        let last = Rgba::from(ink.value(at(start, 200), TONE));
        assert!((last.r - to.r).abs() < 1e-4 && (last.g - to.g).abs() < 1e-4);
    }
}
