//! The "Usage" row at the foot of the thread list and the popover it opens: per provider, its
//! plan and each window's meter, as the CLIs reported them (`newjen_core::usage`).
//!
//! A bar exists only where the CLI reported how much was used. The popover moves in
//! (opacity and a 4 px rise, 150 ms) only when opened with the mouse.

use std::time::Duration;

use gpui::{
    Anchor, Animation, AnimationExt as _, AnyElement, App, Context, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, Render, RenderOnce, Styled,
    WeakEntity, Window, div, hsla, point, prelude::*, px,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::animation::ease_out_cubic;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Selectable, Sizable as _};
use newjen_core::picker::reset_time;
use newjen_core::providers::Provider;
use newjen_core::store::now_ms;
use newjen_core::usage::{self, ProviderUsage, UsageStatus, UsageWindow};

use crate::theme::material;
use crate::ui::{self, IconName, Tone};

const WIDTH: f32 = 300.0;
const RISE: f32 = 4.0;
const OPEN: Duration = Duration::from_millis(150);
const METER_WIDTH: f32 = 56.0;

/// The providers that reported something, in a fixed order.
fn reported() -> Vec<ProviderUsage> {
    [Provider::Claude, Provider::Codex]
        .into_iter()
        .filter_map(usage::latest)
        .collect()
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
    }
}

fn provider_icon(provider: Provider) -> IconName {
    match provider {
        Provider::Claude => IconName::Sparkles,
        Provider::Codex => IconName::Bot,
    }
}

/// The fullest window still counting, for the little meter on the row. A window past its
/// reset time no longer says anything.
pub fn worst_meter(usages: &[ProviderUsage], now: i64) -> Option<(f32, UsageStatus)> {
    usages
        .iter()
        .flat_map(|usage| &usage.windows)
        .filter(|window| !window.expired(now))
        .filter_map(|window| window.used_fraction.map(|used| (used, window.status)))
        .max_by(|a, b| a.0.total_cmp(&b.0))
}

/// "resets 4:10pm", or "reset" once the time has passed; nothing when none was reported.
pub fn reset_line(window: &UsageWindow, now: i64) -> Option<String> {
    if window.expired(now) {
        return Some("reset".to_string());
    }
    window
        .resets_at_unix_seconds
        .map(|reset| format!("resets {}", reset_time(reset, now)))
}

/// The credits line as a label and a value: "Credits 120" is `("Credits", Some("120"))`, "No
/// credits" has no value.
pub fn credits_parts(summary: &str) -> (&str, Option<&str>) {
    match summary.strip_prefix("Credits ") {
        Some(value) => ("Credits", Some(value)),
        None => (summary, None),
    }
}

/// The credits line for the popover: from 100 up a balance is rounded to whole credits with
/// thousands separators ("66,389"), below it keeps its cents ("12.50").
pub fn credits_line(credits: &newjen_core::usage::UsageCredits) -> (String, Option<String>) {
    match credits.balance {
        Some(balance) if !credits.unlimited && balance >= 100.0 => (
            "Credits".into(),
            Some(crate::format::thousands(balance.round() as usize)),
        ),
        _ => {
            let summary = credits.summary();
            let (label, value) = credits_parts(&summary);
            (label.to_string(), value.map(str::to_string))
        }
    }
}

fn status_color(status: UsageStatus, cx: &App) -> Hsla {
    let colors = &cx.theme().colors;
    match status {
        UsageStatus::Ok => colors.foreground,
        UsageStatus::Warning => colors.warning,
        UsageStatus::Limited => colors.danger,
    }
}

fn bar(id: String, used: f32, status: UsageStatus, height: f32, cx: &App) -> Progress {
    Progress::new(id)
        .value(used * 100.0)
        .color(status_color(status, cx))
        .with_size(gpui_kit::component::Size::Size(px(height)))
}

/// The state behind the footer row: whether the pointer or the keyboard last acted on its
/// button, which decides whether the popover moves in.
pub struct UsageMenu {
    pointer: bool,
}

impl UsageMenu {
    pub fn new() -> Self {
        Self { pointer: false }
    }
}

impl Render for UsageMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let usages = reported();
        let now = now_ms() / 1000;
        let meter = worst_meter(&usages, now);
        let pointer = self.pointer;
        div().id("usage-menu").flex_none().px_0p5().pb_2().child(
            Popover::new("usage")
                .anchor(Anchor::BottomLeft)
                .appearance(false)
                .trigger_style(gpui::StyleRefinement::default().w_full())
                .on_open_change({
                    let owner = cx.weak_entity();
                    move |open, _, cx| {
                        if !*open {
                            let _ = owner.update(cx, |menu, _| menu.pointer = false);
                        }
                    }
                })
                .trigger(Trigger {
                    meter,
                    owner: cx.weak_entity(),
                })
                .content(move |_, _, cx| panel(&usages, now, pointer, cx)),
        )
    }
}

/// The footer row: a gauge, "Usage" and the meter of the worst window.
#[derive(IntoElement)]
struct Trigger {
    meter: Option<(f32, UsageStatus)>,
    owner: WeakEntity<UsageMenu>,
}

impl Selectable for Trigger {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = material(cx).glass_muted;
        let (by_mouse, by_key) = (self.owner.clone(), self.owner);
        Button::new("usage-trigger")
            .ghost()
            .w_full()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                let _ = by_mouse.update(cx, |menu, _| menu.pointer = true);
            })
            .on_key_down(move |_, _, cx| {
                let _ = by_key.update(cx, |menu, _| menu.pointer = false);
            })
            .debug_selector(|| "usage-trigger".to_string())
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(ui::icon(
                        IconName::Gauge,
                        gpui_kit::component::Size::Small,
                        muted,
                    ))
                    .child(div().flex_1().text_color(muted).child("Usage"))
                    .children(self.meter.map(|(used, status)| {
                        div().flex_none().w(px(METER_WIDTH)).child(bar(
                            "usage-meter".into(),
                            used,
                            status,
                            4.0,
                            cx,
                        ))
                    })),
            )
    }
}

fn panel(usages: &[ProviderUsage], now: i64, pointer: bool, cx: &mut App) -> AnyElement {
    let material = material(cx);
    let theme = cx.theme();
    let ink = hsla(0., 0., 0., if theme.is_dark() { 0.45 } else { 0.18 });
    let ring = material.card_stroke;
    let surface = v_flex()
        .id("usage-panel")
        .debug_selector(|| "usage-panel".to_string())
        .relative()
        .w(px(WIDTH))
        .p_3()
        .gap_3()
        .rounded(px(10.))
        .bg(material.popover_glass)
        .text_color(theme.popover_foreground)
        .text_sm()
        .children(provider_sections(usages, now, cx));
    let surface = if usages.is_empty() {
        surface.child(
            div()
                .text_color(material.glass_muted)
                .child("No usage reported yet."),
        )
    } else {
        surface
    };
    // The shadow fades in with the surface: a translucent panel does not hide its own shadow,
    // so a shadow at full strength would show through it mid-fade.
    let shadow = move |strength: f32| {
        vec![
            gpui::BoxShadow {
                color: ring.opacity(strength),
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(1.),
                inset: false,
            },
            gpui::BoxShadow {
                color: ink.opacity(ink.a * strength),
                offset: point(px(0.), px(6.)),
                blur_radius: px(10.),
                spread_radius: px(-2.),
                inset: false,
            },
        ]
    };
    if !pointer {
        return surface.shadow(shadow(1.0)).into_any_element();
    }
    surface
        .with_animation(
            "usage-open",
            Animation::new(OPEN).with_easing(ease_out_cubic),
            move |surface, delta| {
                surface
                    .top(px(-RISE * (1.0 - delta)))
                    .opacity(delta)
                    .shadow(shadow(delta * delta * delta))
            },
        )
        .into_any_element()
}

fn provider_sections(usages: &[ProviderUsage], now: i64, cx: &App) -> Vec<AnyElement> {
    let material = material(cx);
    let mut sections = Vec::new();
    for (ix, usage) in usages.iter().enumerate() {
        let mut section = v_flex().gap_2p5();
        if ix > 0 {
            section = section
                .pt_3()
                .border_t_1()
                .border_color(material.card_stroke);
        }
        section = section.child(
            h_flex()
                .items_center()
                .gap_2()
                .child(ui::icon(
                    provider_icon(usage.provider),
                    gpui_kit::component::Size::Small,
                    material.glass_muted,
                ))
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_base()
                        .child(provider_name(usage.provider)),
                )
                .children(
                    usage
                        .plan_label()
                        .map(|plan| ui::status_badge(plan, Tone::Neutral)),
                ),
        );
        for window in &usage.windows {
            section = section.child(window_row(usage.provider, window, now, cx));
        }
        if let Some(credits) = &usage.credits {
            let (label, value) = credits_line(credits);
            section =
                section.child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .text_color(material.glass_muted)
                        .child(label)
                        .children(value.map(|value| {
                            div().text_color(cx.theme().popover_foreground).child(value)
                        })),
                );
        }
        sections.push(section.into_any_element());
    }
    sections
}

fn window_row(provider: Provider, window: &UsageWindow, now: i64, cx: &App) -> AnyElement {
    let muted = material(cx).glass_muted;
    let expired = window.expired(now);
    let status = if expired {
        UsageStatus::Ok
    } else {
        window.status
    };
    let reset = reset_line(window, now);
    let attention = status != UsageStatus::Ok;
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .text_color(muted)
                .child(div().min_w_0().child(window.label.clone()))
                .children(reset.map(|reset| {
                    div()
                        .flex_none()
                        .when(attention, |line| line.text_color(status_color(status, cx)))
                        .child(reset)
                })),
        )
        .children(window.used_fraction.filter(|_| !expired).map(|used| {
            bar(
                format!("usage-{}-{}", provider.id(), window.key),
                used,
                status,
                4.0,
                cx,
            )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn window(
        key: &str,
        used: Option<f32>,
        resets: Option<i64>,
        status: UsageStatus,
    ) -> UsageWindow {
        UsageWindow {
            key: key.into(),
            label: key.into(),
            used_fraction: used,
            resets_at_unix_seconds: resets,
            length_seconds: None,
            status,
        }
    }

    fn usage(windows: Vec<UsageWindow>) -> ProviderUsage {
        ProviderUsage {
            windows,
            ..ProviderUsage::empty(Provider::Claude)
        }
    }

    #[test]
    fn the_meter_shows_the_fullest_live_window() {
        let usages = [
            usage(vec![
                window("5h", Some(0.38), Some(NOW + 100), UsageStatus::Ok),
                window("week", Some(0.2), Some(NOW + 100), UsageStatus::Ok),
            ]),
            usage(vec![window(
                "codex",
                Some(0.91),
                None,
                UsageStatus::Warning,
            )]),
        ];
        assert_eq!(
            worst_meter(&usages, NOW),
            Some((0.91, UsageStatus::Warning))
        );
        // A window past its reset says nothing any more.
        let stale = [usage(vec![window(
            "5h",
            Some(0.99),
            Some(NOW - 1),
            UsageStatus::Warning,
        )])];
        assert_eq!(worst_meter(&stale, NOW), None);
        // No utilisation reported: no meter.
        let bare = [usage(vec![window("5h", None, None, UsageStatus::Limited)])];
        assert_eq!(worst_meter(&bare, NOW), None);
        assert_eq!(worst_meter(&[], NOW), None);
    }

    #[test]
    fn a_window_says_when_it_resets_or_that_it_did() {
        let live = window("5h", Some(0.5), Some(NOW + 3600), UsageStatus::Ok);
        assert!(reset_line(&live, NOW).unwrap().starts_with("resets "));
        let past = window("5h", Some(0.5), Some(NOW - 5), UsageStatus::Ok);
        assert_eq!(reset_line(&past, NOW).as_deref(), Some("reset"));
        let unknown = window("5h", None, None, UsageStatus::Ok);
        assert_eq!(reset_line(&unknown, NOW), None);
    }

    #[test]
    fn credits_split_into_a_label_and_a_value() {
        assert_eq!(credits_parts("Credits 120"), ("Credits", Some("120")));
        assert_eq!(
            credits_parts("Credits unlimited"),
            ("Credits", Some("unlimited"))
        );
        assert_eq!(credits_parts("No credits"), ("No credits", None));
    }

    #[test]
    fn a_large_credit_balance_reads_in_whole_credits_with_separators() {
        let credits = |balance| newjen_core::usage::UsageCredits {
            has_credits: true,
            unlimited: false,
            balance,
        };
        let line = |balance| credits_line(&credits(balance));
        assert_eq!(
            line(Some(66388.61)),
            ("Credits".into(), Some("66,389".into()))
        );
        assert_eq!(line(Some(100.0)), ("Credits".into(), Some("100".into())));
        assert_eq!(line(Some(1000.0)), ("Credits".into(), Some("1,000".into())));
        assert_eq!(line(Some(12.5)), ("Credits".into(), Some("12.50".into())));
        assert_eq!(line(Some(99.0)), ("Credits".into(), Some("99".into())));
        assert_eq!(line(None), ("Credits".into(), Some("available".into())));
    }
}
