use gpui::{App, Hsla, IntoElement, ParentElement as _, Styled as _, div, px};
use gpui_kit::component::ActiveTheme as _;

/// FNV-1a (32 bit) over the lowercased name's UTF-8 bytes.
fn hash(name: &str) -> u32 {
    name.to_lowercase()
        .bytes()
        .fold(0x811c_9dc5_u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        })
}

fn palette(cx: &App) -> [Hsla; 6] {
    let colors = &cx.theme().colors;
    [
        colors.blue,
        colors.green,
        colors.yellow,
        colors.red,
        colors.magenta,
        colors.cyan,
    ]
}

/// A color for `name`, the same every time, from Kit's palette.
pub fn monogram_color(name: &str, cx: &App) -> Hsla {
    let palette = palette(cx);
    palette[hash(name) as usize % palette.len()]
}

fn initial(name: &str) -> String {
    name.chars()
        .find(|c| !c.is_whitespace())
        .map_or_else(|| "?".to_string(), |first| first.to_uppercase().to_string())
}

/// The first letter of `name` on a small tinted tile, for the project row.
pub fn monogram(name: &str, cx: &App) -> impl IntoElement {
    let color = monogram_color(name, cx);
    div()
        .flex_none()
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .bg(color.opacity(0.22))
        .text_color(color)
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(initial(name))
}

/// The same tile, larger and outlined, for the project chooser.
pub fn monogram_large(name: &str, cx: &App) -> impl IntoElement {
    let color = monogram_color(name, cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded_md()
        .border_1()
        .border_color(color.opacity(0.35))
        .bg(color.opacity(0.16))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(color)
        .child(initial(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use gpui_kit::component::Theme;

    #[test]
    fn the_hash_is_fnv_1a_over_the_lowercased_name() {
        assert_eq!(hash(""), 2_166_136_261);
        assert_eq!(hash("a"), 0xe40c_292c);
        assert_eq!(hash("Newjen"), hash("newjen"));
        assert_ne!(hash("newjen"), hash("aelomi"));
    }

    #[gpui::test]
    fn a_name_always_gets_the_same_color_whatever_its_case(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            Theme::change(gpui::WindowAppearance::Dark, None, cx);
            assert_eq!(monogram_color("newjen", cx), monogram_color("newjen", cx));
            assert_eq!(monogram_color("NewJen", cx), monogram_color("newjen", cx));
            let palette = palette(cx);
            for name in [
                "newjen",
                "pstack",
                "rift",
                "aelomi-site",
                "dotfiles",
                "x",
                "",
            ] {
                assert!(palette.contains(&monogram_color(name, cx)), "{name}");
            }
            let distinct: std::collections::HashSet<_> =
                ["newjen", "pstack", "rift", "aelomi-site", "dotfiles"]
                    .iter()
                    .map(|name| hash(name) as usize % palette.len())
                    .collect();
            assert!(distinct.len() >= 3);
        });
    }

    #[test]
    fn the_tile_shows_the_first_letter_in_capitals() {
        assert_eq!(initial("newjen"), "N");
        assert_eq!(initial("  rift"), "R");
        assert_eq!(initial("épée"), "É");
        assert_eq!(initial("修复"), "修");
        assert_eq!(initial(""), "?");
    }
}
