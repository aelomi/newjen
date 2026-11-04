use std::sync::OnceLock;

use gpui::{
    App, BoxShadow, Global, Hsla, Pixels, SharedString, Window, WindowAppearance,
    WindowBackgroundAppearance, hsla, point, px, rgb,
};
use gpui_kit::base::TypographyTokens;
use gpui_kit::component::{ActiveTheme as _, Theme};
use newjen_core::appearance::{Accent, Appearance, DiffColors, Font, Glass, Mode};
use objc2_app_kit::NSWorkspace;

pub const SIDEBAR: Pixels = px(255.);
/// The gap between the window's top, right and bottom edges and the floating card.
pub const CARD_INSET: Pixels = px(8.);
pub const CARD_RADIUS: Pixels = px(10.);

/// The window's materials, chosen by light or dark mode and by how much glass the user wants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    dark: bool,
    /// Over the blurred window, behind everything. Opaque for `Glass::Solid`.
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
    pub fn resolve(dark: bool, glass: Glass) -> Self {
        let shade = |hex: u32, alpha: f32| {
            let mut color = Hsla::from(rgb(hex));
            color.a = alpha;
            color
        };
        let tint_alpha = match glass {
            Glass::Clear => 0.6,
            Glass::Frosted => 0.82,
            Glass::Solid => 1.0,
        };
        let (glass_tint, popover) = if dark {
            (shade(0x0e0e12, tint_alpha), shade(0x1b1b21, 1.0))
        } else {
            (shade(0xf4f4f7, tint_alpha), shade(0xfbfbfd, 1.0))
        };
        if dark {
            Self {
                dark,
                glass_tint,
                glass_hover: hsla(0., 0., 1., 0.05),
                glass_selected: hsla(0., 0., 1., 0.09),
                glass_muted: shade(0xa3a3a3, 1.0),
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

/// The user's look. Everything visual derives from this one value in `apply`.
struct Look(Appearance);

impl Global for Look {}

pub fn appearance(cx: &App) -> &Appearance {
    static DEFAULT: OnceLock<Appearance> = OnceLock::new();
    match cx.try_global::<Look>() {
        Some(look) => &look.0,
        None => DEFAULT.get_or_init(Appearance::default),
    }
}

/// The glass the window is really made of: Reduce Transparency overrides the choice.
fn glass(cx: &App) -> Glass {
    if reduced(cx) {
        Glass::Solid
    } else {
        appearance(cx).glass
    }
}

pub fn material(cx: &App) -> Material {
    Material::resolve(cx.theme().is_dark(), glass(cx))
}

/// What the main window is made of: blurred glass, or opaque for solid.
pub fn window_background(cx: &App) -> WindowBackgroundAppearance {
    match glass(cx) {
        Glass::Solid => WindowBackgroundAppearance::Opaque,
        Glass::Clear | Glass::Frosted => WindowBackgroundAppearance::Blurred,
    }
}

/// The conversation column's widest size, or `None` when the user lets it fill the card.
pub fn column_width(cx: &App) -> Option<Pixels> {
    appearance(cx)
        .chat_width
        .max_points()
        .map(|points| px(f32::from(points)))
}

/// The one place that decides between light and dark: `Mode::System` follows the system.
fn window_appearance(mode: Mode, system: WindowAppearance) -> WindowAppearance {
    match mode {
        Mode::System => system,
        Mode::Light => WindowAppearance::Light,
        Mode::Dark => WindowAppearance::Dark,
    }
}

/// Reads the system's Reduce Transparency setting and the saved look once and applies them.
/// Call it after `gpui_kit::init`, before the window opens.
pub fn install(appearance: Appearance, cx: &mut App) {
    cx.set_global(ReduceTransparency(system_reduce_transparency()));
    cx.set_global(Look(appearance));
    reload(cx.window_appearance(), cx);
}

/// Makes `appearance` the look, on every window. The caller saves it.
pub fn set_appearance(appearance: Appearance, window: Option<&mut Window>, cx: &mut App) {
    cx.set_global(Look(appearance));
    let system = match &window {
        Some(window) => window.appearance(),
        None => cx.window_appearance(),
    };
    reload(system, cx);
    if let Some(window) = window {
        window.set_background_appearance(window_background(cx));
    }
}

/// The system switched between light and dark.
pub fn follow_system_appearance(window: &mut Window, cx: &mut App) {
    reload(window.appearance(), cx);
}

/// `Theme::change` loads Kit's own colors again, which is also what puts back the ones an
/// earlier accent or diff palette replaced. `apply` then lays the look on top.
fn reload(system: WindowAppearance, cx: &mut App) {
    let mode = window_appearance(appearance(cx).mode, system);
    Theme::change(mode, None, cx);
    apply(cx);
}

/// The accent's swatch, and its color for buttons, focus rings and selection.
pub fn accent_swatch(accent: Accent) -> Hsla {
    rgb(match accent {
        Accent::Graphite => 0x8e8e93,
        Accent::Blue => 0x3b82f6,
        Accent::Indigo => 0x6366f1,
        Accent::Violet => 0x8b5cf6,
        Accent::Pink => 0xec4899,
        Accent::Orange => 0xf97316,
        Accent::Green => 0x22c55e,
    })
    .into()
}

fn lighter(color: Hsla, by: f32) -> Hsla {
    Hsla {
        l: (color.l + by).clamp(0., 1.),
        ..color
    }
}

/// Graphite is Kit's own look, so it replaces nothing.
fn apply_accent(theme: &mut Theme, accent: Accent) {
    if accent == Accent::Graphite {
        return;
    }
    let base = accent_swatch(accent);
    let (hover, active) = (lighter(base, 0.05), lighter(base, -0.06));
    let white = hsla(0., 0., 1., 1.);
    theme.primary = base;
    theme.primary_hover = hover;
    theme.primary_active = active;
    theme.primary_foreground = white;
    theme.button_primary = base;
    theme.button_primary_hover = hover;
    theme.button_primary_active = active;
    theme.button_primary_foreground = white;
    theme.sidebar_primary = base;
    theme.sidebar_primary_foreground = white;
    theme.ring = base;
    theme.selection = base.opacity(0.3);
    theme.link = base;
    theme.link_hover = hover;
    theme.link_active = active;
    theme.switch = base;
    theme.slider_bar = base;
    theme.progress_bar = base;
    theme.caret = base;
}

/// The added and removed colors each choice shows, for the Settings swatches and the theme.
pub fn diff_swatches(colors: DiffColors) -> (Hsla, Hsla) {
    match colors {
        DiffColors::GreenRed => (rgb(0x22c55e).into(), rgb(0xef4444).into()),
        DiffColors::BlueOrange => (rgb(0x3b82f6).into(), rgb(0xf97316).into()),
    }
}

/// Green and red leave Kit's own. Blue and orange take their place everywhere Kit and the app
/// say added and removed, so counts, diff lines and status marks change together.
fn apply_diff_colors(theme: &mut Theme, colors: DiffColors) {
    if colors == DiffColors::GreenRed {
        return;
    }
    let (added, removed) = diff_swatches(colors);
    theme.success = added;
    theme.success_hover = lighter(added, 0.05);
    theme.success_active = lighter(added, -0.06);
    theme.danger = removed;
    theme.danger_hover = lighter(removed, 0.05);
    theme.danger_active = lighter(removed, -0.06);
}

/// The families the machine has, listed once: it costs about 100ms.
pub fn installed_fonts(cx: &App) -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| cx.text_system().all_font_names())
}

/// GPUI panics laying out text in a family the machine lacks, so a family the user chose is
/// applied only when it is installed.
fn usable_family(
    font: &Font,
    installed: impl FnOnce() -> &'static [String],
) -> Option<SharedString> {
    let name = font.family()?;
    installed()
        .iter()
        .any(|family| family == name)
        .then(|| name.to_string().into())
}

/// Puts the materials and the look onto Kit's theme. `Theme::change` resets the theme, so call
/// this after every one.
///
/// Kit's `background` color becomes the card fill, so everything inside the card shares one
/// solid. The window root paints the `background` token, so the glass tint takes that token's
/// place. Dialogs and the drawer also paint it and set the card fill themselves.
pub fn apply(cx: &mut App) {
    let material = material(cx);
    let look = appearance(cx).clone();
    let sans = usable_family(&look.interface_font, || installed_fonts(cx));
    let mono = usable_family(&look.code_font, || installed_fonts(cx));
    Theme::update(cx, |theme| {
        theme.background = material.card;
        theme.popover = material.popover;
        theme.list_active = material.glass_selected;
        theme.list_hover = material.glass_hover;
        // The default sans is the virtual system font, which Kit resolves to a real family
        // whenever this field changes; the default mono is Kit's own pick.
        theme.font_family = sans.unwrap_or_else(|| ".SystemUIFont".into());
        theme.mono_font_family = mono.unwrap_or_else(|| TypographyTokens::default().mono);
        theme.font_size = px(f32::from(look.interface_size.points()));
        theme.mono_font_size = px(f32::from(look.code_size.points()));
        apply_accent(theme, look.accent);
        apply_diff_colors(theme, look.diff_colors);
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
    use newjen_core::appearance::{CodeSize, TextSize};

    fn set_look(look: Appearance, system: WindowAppearance, cx: &mut App) {
        cx.set_global(ReduceTransparency(false));
        cx.set_global(Look(look));
        reload(system, cx);
    }

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
        let dark = Material::resolve(true, Glass::Frosted);
        let light = Material::resolve(false, Glass::Frosted);
        assert_eq!(dark.glass_tint, Hsla::from(rgb(0x0e0e12)).opacity(0.82));
        assert_eq!(light.glass_tint, Hsla::from(rgb(0xf4f4f7)).opacity(0.82));
        assert_eq!(dark.glass_muted, Hsla::from(rgb(0xa3a3a3)));
        assert_eq!(light.glass_muted, Hsla::from(rgb(0x525252)));
        assert_eq!(dark.card, Hsla::from(rgb(0x111114)));
        assert_eq!(light.card, Hsla::from(rgb(0xffffff)));
        assert_eq!(dark.glass_selected, hsla(0., 0., 1., 0.09));
        assert_eq!(light.glass_selected, hsla(0., 0., 0., 0.08));
        assert_eq!(dark.card_shadow()[0].offset, point(px(0.), px(4.)));
        assert_eq!(light.card_shadow()[0].blur_radius, px(16.));
    }

    #[test]
    fn glass_sets_how_much_of_the_desktop_shows_through() {
        for dark in [true, false] {
            let clear = Material::resolve(dark, Glass::Clear);
            let frosted = Material::resolve(dark, Glass::Frosted);
            let solid = Material::resolve(dark, Glass::Solid);
            assert_eq!(clear.glass_tint.a, 0.6);
            assert_eq!(frosted.glass_tint.a, 0.82);
            assert_eq!(solid.glass_tint.a, 1.0);
            for material in [clear, frosted, solid] {
                assert_eq!(
                    material.popover.a, 1.0,
                    "no glass lets text show through a popover"
                );
            }
            assert_ne!(solid.glass_tint, solid.card);
            assert_eq!(solid.card, frosted.card);
            assert_eq!(
                solid.glass_tint,
                Hsla {
                    a: 1.0,
                    ..frosted.glass_tint
                }
            );
        }
    }

    #[test]
    fn a_family_is_used_only_when_it_is_installed() {
        static INSTALLED: [String; 0] = [];
        let named = Font::Named("Menlo".into());
        assert_eq!(usable_family(&Font::System, || &INSTALLED), None);
        assert_eq!(usable_family(&named, || &INSTALLED), None);
        let installed: &'static [String] = Box::leak(Box::new(["Menlo".to_string()]));
        assert_eq!(usable_family(&named, || installed), Some("Menlo".into()));
        assert_eq!(
            usable_family(&Font::Named("Comic Neue".into()), || installed),
            None
        );
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
            assert_eq!(cx.theme().font_size, px(14.));
            assert_eq!(cx.theme().mono_font_size, px(12.));

            cx.set_global(ReduceTransparency(true));
            apply(cx);
            assert_eq!(cx.theme().colors.background, material(cx).card);
            assert_eq!(cx.theme().tokens.background.color.a, 1.0);
            assert_eq!(cx.theme().popover.a, 1.0);

            cx.set_global(ReduceTransparency(false));
            Theme::change(WindowAppearance::Light, None, cx);
            assert_ne!(cx.theme().tokens.background.color, material(cx).glass_tint);
            apply(cx);
            assert_eq!(cx.theme().font_size, px(14.));
            assert_eq!(cx.theme().mono_font_size, px(12.));
            assert_eq!(cx.theme().colors.background, material(cx).card);
            assert_eq!(cx.theme().tokens.background.color, material(cx).glass_tint);
            assert_eq!(cx.theme().list_hover, hsla(0., 0., 0., 0.05));
        });
    }

    #[gpui::test]
    fn a_chosen_mode_holds_when_the_system_switches(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            for (mode, system, dark) in [
                (Mode::System, WindowAppearance::Dark, true),
                (Mode::System, WindowAppearance::Light, false),
                (Mode::Light, WindowAppearance::Dark, false),
                (Mode::Dark, WindowAppearance::Light, true),
            ] {
                let look = Appearance {
                    mode,
                    ..Appearance::default()
                };
                set_look(look, system, cx);
                assert_eq!(cx.theme().is_dark(), dark, "{mode:?} on {system:?}");
            }
        });
    }

    #[gpui::test]
    fn every_accent_colors_buttons_rings_and_selection_and_graphite_puts_kit_back(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            Theme::change(WindowAppearance::Dark, None, cx);
            let kit = (cx.theme().primary, cx.theme().ring, cx.theme().selection);
            for &accent in Accent::ALL {
                let look = Appearance {
                    accent,
                    ..Appearance::default()
                };
                set_look(look.clone(), WindowAppearance::Dark, cx);
                let theme = cx.theme();
                if accent == Accent::Graphite {
                    assert_eq!((theme.primary, theme.ring, theme.selection), kit);
                    continue;
                }
                let base = accent_swatch(accent);
                assert_eq!(theme.primary, base, "{accent:?}");
                assert_eq!(theme.tokens.button_primary.color, base);
                assert_eq!(theme.ring, base);
                assert_eq!(theme.link, base);
                assert_eq!(theme.caret, base);
                assert_eq!(theme.selection, base.opacity(0.3));
                assert_ne!(theme.primary_hover, base);

                set_look(look, WindowAppearance::Light, cx);
                assert_eq!(cx.theme().primary, base, "{accent:?} in light");
            }
        });
    }

    #[gpui::test]
    fn blue_and_orange_replace_added_and_removed_and_green_red_puts_kit_back(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            Theme::change(WindowAppearance::Dark, None, cx);
            let kit = (cx.theme().success, cx.theme().danger);
            let look = Appearance {
                diff_colors: DiffColors::BlueOrange,
                ..Appearance::default()
            };
            set_look(look, WindowAppearance::Dark, cx);
            let (added, removed) = (cx.theme().success, cx.theme().danger);
            assert_eq!(added, Hsla::from(rgb(0x3b82f6)));
            assert_eq!(removed, Hsla::from(rgb(0xf97316)));
            assert_ne!(cx.theme().success_hover, added);
            assert_ne!(cx.theme().danger_active, removed);

            Theme::change(WindowAppearance::Light, None, cx);
            apply(cx);
            assert_eq!(cx.theme().success, added);

            set_look(Appearance::default(), WindowAppearance::Dark, cx);
            assert_eq!((cx.theme().success, cx.theme().danger), kit);
        });
    }

    #[gpui::test]
    fn glass_reaches_the_window_and_reduce_transparency_wins(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            for (glass, alpha, background) in [
                (Glass::Clear, 0.6, WindowBackgroundAppearance::Blurred),
                (Glass::Frosted, 0.82, WindowBackgroundAppearance::Blurred),
                (Glass::Solid, 1.0, WindowBackgroundAppearance::Opaque),
            ] {
                let look = Appearance {
                    glass,
                    ..Appearance::default()
                };
                set_look(look, WindowAppearance::Dark, cx);
                assert_eq!(cx.theme().tokens.background.color.a, alpha);
                assert_eq!(window_background(cx), background);
            }
            set_look(
                Appearance {
                    glass: Glass::Clear,
                    ..Appearance::default()
                },
                WindowAppearance::Dark,
                cx,
            );
            cx.set_global(ReduceTransparency(true));
            apply(cx);
            assert_eq!(cx.theme().tokens.background.color.a, 1.0);
            assert_eq!(window_background(cx), WindowBackgroundAppearance::Opaque);
        });
    }

    #[gpui::test]
    fn sizes_reach_the_theme_and_a_missing_family_falls_back(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            set_look(Appearance::default(), WindowAppearance::Dark, cx);
            let (sans, mono) = (
                cx.theme().font_family.clone(),
                cx.theme().mono_font_family.clone(),
            );
            let look = Appearance {
                interface_size: TextSize::new(16),
                code_size: CodeSize::new(10),
                interface_font: Font::Named("No Such Family 7".into()),
                code_font: Font::Named("No Such Mono 7".into()),
                ..Appearance::default()
            };
            set_look(look, WindowAppearance::Dark, cx);
            assert_eq!(cx.theme().font_size, px(16.));
            assert_eq!(cx.theme().mono_font_size, px(10.));
            assert_eq!(cx.theme().font_family, sans);
            assert_eq!(cx.theme().mono_font_family, mono);
        });
    }

    #[gpui::test]
    fn the_chat_column_follows_the_chat_width(cx: &mut TestAppContext) {
        use newjen_core::appearance::ChatWidth;
        cx.update(|cx| {
            assert_eq!(column_width(cx), Some(px(720.)));
            for (width, expected) in [
                (ChatWidth::Wide, Some(px(900.))),
                (ChatWidth::Full, None),
                (ChatWidth::Comfortable, Some(px(720.))),
            ] {
                cx.set_global(Look(Appearance {
                    chat_width: width,
                    ..Appearance::default()
                }));
                assert_eq!(column_width(cx), expected);
            }
        });
    }
}
