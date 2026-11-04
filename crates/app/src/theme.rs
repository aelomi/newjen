use gpui::{
    App, BoxShadow, Global, Hsla, Pixels, Window, WindowBackgroundAppearance, hsla, point, px, rgb,
};
use gpui_kit::component::{ActiveTheme as _, Theme};
use objc2_app_kit::NSWorkspace;

pub const SIDEBAR: Pixels = px(255.);
/// The gap between the window's top, right and bottom edges and the floating card.
pub const CARD_INSET: Pixels = px(8.);
pub const CARD_RADIUS: Pixels = px(10.);

/// The window's materials, chosen by light or dark mode and by Reduce Transparency.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    dark: bool,
    /// Over the blurred window, behind everything. Opaque under Reduce Transparency.
    pub glass_tint: Hsla,
    pub glass_hover: Hsla,
    pub glass_selected: Hsla,
    /// Muted text that sits on the glass.
    pub glass_muted: Hsla,
    /// The solid floating panel: the chat, and the recent projects.
    pub card: Hsla,
    pub card_stroke: Hsla,
    /// Menus, popovers and tooltips: fully opaque. GPUI blurs only what is behind the window,
    /// never the card's own text behind a popover, so any see-through shows that text through
    /// it, and a tooltip over text was illegible.
    pub popover: Hsla,
}

impl Material {
    /// Under Reduce Transparency the tint loses its alpha.
    pub fn resolve(dark: bool, reduce_transparency: bool) -> Self {
        let shade = |hex: u32, alpha: f32| {
            let mut color = Hsla::from(rgb(hex));
            color.a = alpha;
            color
        };
        let (tint, popover) = if dark {
            (shade(0x0e0e12, 0.82), shade(0x1b1b21, 1.0))
        } else {
            (shade(0xf4f4f7, 0.82), shade(0xfbfbfd, 1.0))
        };
        let glass_tint = if reduce_transparency {
            Hsla { a: 1.0, ..tint }
        } else {
            tint
        };
        if dark {
            Self {
                dark,
                glass_tint,
                glass_hover: hsla(0., 0., 1., 0.05),
                glass_selected: hsla(0., 0., 1., 0.09),
                glass_muted: shade(0xd4d4d4, 1.0),
                card: shade(0x111114, 1.0),
                card_stroke: hsla(0., 0., 1., 0.07),
                popover,
            }
        } else {
            Self {
                dark,
                glass_tint,
                glass_hover: hsla(0., 0., 0., 0.05),
                glass_selected: hsla(0., 0., 0., 0.08),
                glass_muted: shade(0x525252, 1.0),
                card: shade(0xffffff, 1.0),
                card_stroke: hsla(0., 0., 0., 0.06),
                popover,
            }
        }
    }

    pub fn card_shadow(&self) -> Vec<BoxShadow> {
        let (y, blur, alpha) = if self.dark {
            (4., 24., 0.35)
        } else {
            (2., 16., 0.12)
        };
        vec![BoxShadow {
            color: hsla(0., 0., 0., alpha),
            offset: point(px(0.), px(y)),
            blur_radius: px(blur),
            spread_radius: px(0.),
            inset: false,
        }]
    }
}

/// Whether macOS asks apps for less transparency (Accessibility, Display).
#[derive(Default)]
struct ReduceTransparency(bool);

impl Global for ReduceTransparency {}

fn reduced(cx: &App) -> bool {
    cx.try_global::<ReduceTransparency>()
        .is_some_and(|state| state.0)
}

fn system_reduce_transparency() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency()
}

pub fn material(cx: &App) -> Material {
    Material::resolve(cx.theme().is_dark(), reduced(cx))
}

/// What the main window is made of: blurred glass, or opaque under Reduce Transparency.
pub fn window_background(cx: &App) -> WindowBackgroundAppearance {
    if reduced(cx) {
        WindowBackgroundAppearance::Opaque
    } else {
        WindowBackgroundAppearance::Blurred
    }
}

/// Reads the system setting once and applies the materials. Call it after the first
/// `Theme::change`, before the window opens.
pub fn install(cx: &mut App) {
    cx.set_global(ReduceTransparency(system_reduce_transparency()));
    apply(cx);
}

/// Puts the materials onto Kit's theme. `Theme::change` resets the theme, so call this after
/// every one.
///
/// Kit's `background` color becomes the card fill, so everything inside the card shares one
/// solid. The window root paints the `background` token, so the glass tint takes that token's
/// place. Dialogs and the drawer also paint it and set the card fill themselves.
pub fn apply(cx: &mut App) {
    let material = material(cx);
    Theme::update(cx, |theme| {
        theme.background = material.card;
        theme.popover = material.popover;
        theme.list_active = material.glass_selected;
        theme.list_hover = material.glass_hover;
    });
    // Set on the token alone: `Theme::update` would copy it back onto the `background` color.
    Theme::global_mut(cx).tokens.background = material.glass_tint.into();
    cx.refresh_windows();
}

/// Follows the system's Reduce Transparency setting, which has no change notice. Call it on a
/// timer, as Reduce Motion is.
pub fn follow_reduce_transparency(window: &mut Window, cx: &mut App) {
    set_reduced(system_reduce_transparency(), window, cx);
}

fn set_reduced(now: bool, window: &mut Window, cx: &mut App) {
    if now == reduced(cx) {
        return;
    }
    cx.set_global(ReduceTransparency(now));
    window.set_background_appearance(window_background(cx));
    apply(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, WindowAppearance};

    #[gpui::test]
    fn default_kit_theme_follows_system_appearance(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            for (appearance, dark) in [
                (WindowAppearance::Dark, true),
                (WindowAppearance::Light, false),
            ] {
                Theme::change(appearance, None, cx);
                assert_eq!(cx.theme().is_dark(), dark);
                assert!(!cx.theme().font_family.is_empty());
                assert!(!cx.theme().mono_font_family.is_empty());
            }
        });
    }

    #[test]
    fn materials_follow_light_and_dark() {
        let dark = Material::resolve(true, false);
        let light = Material::resolve(false, false);
        assert_eq!(dark.glass_tint, Hsla::from(rgb(0x0e0e12)).opacity(0.82));
        assert_eq!(light.glass_tint, Hsla::from(rgb(0xf4f4f7)).opacity(0.82));
        assert_eq!(dark.glass_muted, Hsla::from(rgb(0xd4d4d4)));
        assert_eq!(light.glass_muted, Hsla::from(rgb(0x525252)));
        assert_eq!(dark.card, Hsla::from(rgb(0x111114)));
        assert_eq!(light.card, Hsla::from(rgb(0xffffff)));
        assert_eq!(dark.glass_selected, hsla(0., 0., 1., 0.09));
        assert_eq!(light.glass_selected, hsla(0., 0., 0., 0.08));
        assert_eq!(dark.card_shadow()[0].offset, point(px(0.), px(4.)));
        assert_eq!(light.card_shadow()[0].blur_radius, px(16.));
    }

    #[test]
    fn reduce_transparency_makes_the_glass_opaque() {
        for dark in [true, false] {
            let glass = Material::resolve(dark, false);
            let opaque = Material::resolve(dark, true);
            assert!(glass.glass_tint.a < 1.0);
            assert_eq!(glass.popover.a, 1.0);
            assert_eq!(
                opaque.glass_tint,
                Hsla {
                    a: 1.0,
                    ..glass.glass_tint
                }
            );
            assert_eq!(opaque.popover.a, 1.0);
            assert_ne!(opaque.glass_tint, opaque.card);
            assert_eq!(opaque.card, glass.card);
        }
    }

    #[gpui::test]
    fn apply_puts_the_materials_on_kit_theme_and_survives_a_theme_change(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            Theme::change(WindowAppearance::Dark, None, cx);
            cx.set_global(ReduceTransparency(false));
            apply(cx);
            assert_eq!(cx.theme().colors.background, material(cx).card);
            assert_eq!(cx.theme().tokens.background.color, material(cx).glass_tint);
            assert_eq!(cx.theme().popover, material(cx).popover);
            assert_eq!(
                cx.theme().tokens.popover.background,
                material(cx).popover.into(),
                "Kit's tooltip and menus paint this token"
            );
            assert_eq!(cx.theme().tokens.popover.color.a, 1.0);

            cx.set_global(ReduceTransparency(true));
            apply(cx);
            assert_eq!(cx.theme().colors.background, material(cx).card);
            assert_eq!(cx.theme().tokens.background.color.a, 1.0);
            assert_eq!(cx.theme().popover.a, 1.0);

            cx.set_global(ReduceTransparency(false));
            Theme::change(WindowAppearance::Light, None, cx);
            assert_ne!(cx.theme().tokens.background.color, material(cx).glass_tint);
            apply(cx);
            assert_eq!(cx.theme().colors.background, material(cx).card);
            assert_eq!(cx.theme().tokens.background.color, material(cx).glass_tint);
            assert_eq!(cx.theme().list_hover, hsla(0., 0., 0., 0.05));
        });
    }
}
