use gpui::{App, Div, prelude::*};
use gpui_kit::component::{ActiveTheme as _, TitleBar};

use crate::theme::{self, CARD_RADIUS};

/// Kit's title bar with its gradient and border taken off, for a bar that sits on the glass.
pub fn glass_title_bar(cx: &App) -> TitleBar {
    TitleBar::new()
        .bg(cx.theme().transparent)
        .border_color(cx.theme().transparent)
}

/// Kit's title bar as the header of the card: the card shows through, with a line below.
pub fn card_title_bar(cx: &App) -> TitleBar {
    TitleBar::new()
        .bg(cx.theme().transparent)
        .border_color(theme::material(cx).card_stroke)
}

/// Makes `base` a solid floating card: the fill, a hairline border, rounded corners and a soft
/// shadow.
pub fn card(base: Div, cx: &App) -> Div {
    let material = theme::material(cx);
    base.bg(material.card)
        .border_1()
        .border_color(material.card_stroke)
        .rounded(CARD_RADIUS)
        .shadow(material.card_shadow())
        .overflow_hidden()
}
