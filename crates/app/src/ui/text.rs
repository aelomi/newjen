use gpui::{App, Div, Keystroke, ParentElement as _, SharedString, Styled, div};
use gpui_kit::component::{ActiveTheme as _, kbd::Kbd, label::Label};

pub fn title(text: impl Into<SharedString>, _: &App) -> Label {
    Label::new(text).text_xl()
}

pub fn caption(text: impl Into<SharedString>, cx: &App) -> Label {
    Label::new(text)
        .text_sm()
        .text_color(cx.theme().muted_foreground)
}

pub fn key_caps(keys: &'static str, _: &App) -> Kbd {
    Kbd::new(Keystroke::parse(keys).expect("a valid shortcut"))
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
