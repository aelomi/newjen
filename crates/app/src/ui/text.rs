use gpui::{
    AnyElement, App, Div, FontWeight, Half as _, InteractiveElement as _, IntoElement as _,
    Keystroke, ParentElement as _, SharedString, Styled, div, relative, rems,
};
use gpui_kit::component::{ActiveTheme as _, kbd::Kbd, label::Label};

pub fn title(text: impl Into<SharedString>, _: &App) -> Label {
    Label::new(text)
        .text_size(rems(1.6))
        .font_weight(FontWeight::SEMIBOLD)
}

pub fn caption(text: impl Into<SharedString>, cx: &App) -> Label {
    Label::new(text)
        .text_sm()
        .text_color(cx.theme().muted_foreground)
}

/// A shortcut as Kit's key chip. Kit draws Escape as the ⎋ glyph, which reads as a circular
/// arrow, so Escape is set in the same chip as the word "esc".
pub fn key_caps(keys: &'static str, cx: &App) -> AnyElement {
    let keystroke = Keystroke::parse(keys).expect("a valid shortcut");
    if keystroke.key != "escape" || keystroke.modifiers.modified() {
        return Kbd::new(keystroke).into_any_element();
    }
    let theme = cx.theme();
    div()
        .debug_selector(|| "kbd:esc".to_string())
        .text_color(theme.muted_foreground)
        .bg(theme.tokens.muted)
        .py_0p5()
        .px_1()
        .min_w_5()
        .text_center()
        .rounded(theme.radius.half())
        .line_height(relative(1.))
        .text_xs()
        .flex_shrink_0()
        .child("esc")
        .into_any_element()
}

pub fn ellipsis(text: impl Into<SharedString>) -> Div {
    div().w_full().min_w_0().text_ellipsis().child(text.into())
}

pub fn middle_ellipsis(text: impl Into<SharedString>) -> Div {
    div()
        .w_full()
        .min_w_0()
        .text_ellipsis_middle()
        .child(text.into())
}
