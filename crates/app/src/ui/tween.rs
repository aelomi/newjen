use std::time::{Duration, Instant};

use gpui::{Hsla, Rgba};
use gpui_kit::component::animation::ease_out_cubic;

/// A value moving from `from` to `to`, started at `start`. It eases out, and a move that begins
/// later than now waits at `from` until then.
#[derive(Clone, Copy, Debug)]
pub struct Tween<T> {
    from: T,
    to: T,
    start: Instant,
}

pub trait Mix: Copy + PartialEq {
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
    pub fn at(value: T, now: Instant) -> Self {
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

    pub fn value(&self, now: Instant, length: Duration) -> T {
        T::mix(
            self.from,
            self.to,
            ease_out_cubic(self.progress(now, length)),
        )
    }

    pub fn settled(&self, now: Instant, length: Duration) -> bool {
        self.from == self.to || self.progress(now, length) >= 1.0
    }

    /// Heads for `target` from wherever the value is now, or jumps there. A target it already
    /// heads for changes nothing.
    pub fn aim(&mut self, target: T, now: Instant, length: Duration, animate: bool) {
        self.aim_at(target, now, now, length, animate);
    }

    /// Like [`Self::aim`], but the move begins at `begin`: the value holds until then. `running`
    /// is the length the move now under way takes, which tells where the value is now.
    pub fn aim_at(
        &mut self,
        target: T,
        begin: Instant,
        now: Instant,
        running: Duration,
        animate: bool,
    ) {
        if target == self.to {
            return;
        }
        let from = if animate {
            self.value(now, running)
        } else {
            target
        };
        *self = Self {
            from,
            to: target,
            start: begin,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::hsla;

    const ARC: Duration = Duration::from_millis(300);
    const TONE: Duration = Duration::from_millis(200);

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
    fn a_move_that_may_not_animate_jumps_and_is_settled_at_once() {
        let start = Instant::now();
        let mut arc = Tween::at(0.1_f32, start);
        arc.aim(0.9, start, ARC, false);
        assert_eq!(arc.value(start, ARC), 0.9);
        assert!(arc.settled(start, ARC));
        arc.aim_at(0.2, at(start, 500), start, ARC, false);
        assert_eq!(arc.value(start, ARC), 0.2);
        assert!(arc.settled(start, ARC));
    }

    #[test]
    fn a_value_that_has_nowhere_to_go_is_settled() {
        let start = Instant::now();
        let arc = Tween::at(0.4_f32, start);
        assert!(arc.settled(start, ARC));
        let mut moving = Tween::at(0.4_f32, start);
        moving.aim(0.8, start, ARC, true);
        assert!(!moving.settled(start, ARC));
    }

    #[test]
    fn a_move_with_a_delay_holds_until_it_begins() {
        let start = Instant::now();
        let mut fade = Tween::at(1.0_f32, start);
        fade.aim_at(0.0, at(start, 140), start, ARC, true);
        assert_eq!(fade.value(at(start, 100), ARC), 1.0);
        assert!(!fade.settled(at(start, 100), ARC));
        assert!(fade.value(at(start, 200), ARC) < 1.0);
        assert!(fade.settled(at(start, 440), ARC));
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
