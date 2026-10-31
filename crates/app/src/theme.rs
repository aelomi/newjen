use gpui::{Pixels, px};

pub const SIDEBAR: Pixels = px(255.);

#[cfg(test)]
mod tests {
    use gpui::{TestAppContext, WindowAppearance};
    use gpui_kit::component::{ActiveTheme as _, Theme};
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
}
