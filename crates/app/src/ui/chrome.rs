use super::{IconName, ellipsis};
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, MouseButton, SharedString, Window,
    div, prelude::*,
};
use gpui_kit::component::{ActiveTheme as _, TitleBar, list::ListItem};

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

/// What a list row shows.
pub struct ListRow {
    pub title: SharedString,
    /// The quiet line under the title.
    pub detail: Option<String>,
    pub detail_icon: Option<IconName>,
    pub leading: Option<AnyElement>,
    /// Quiet text at the right end, such as a shortcut.
    pub trailing: Option<SharedString>,
    /// The title in the semibold weight.
    pub strong: bool,
    /// The title in the muted color.
    pub quiet: bool,
    pub selected: bool,
}

pub fn list_row(
    id: impl Into<ElementId>,
    row: ListRow,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> ListItem {
    let label = row.detail.as_ref().map_or_else(
        || row.title.to_string(),
        |detail| format!("{}: {detail}", row.title),
    );
    ListItem::new(id)
        .w_full()
        .flex_row()
        .selected(row.selected)
        .accessibility_label(label)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(on_click)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .children(row.leading)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            ellipsis(row.title)
                                .when(row.strong, |line| line.font_weight(FontWeight::SEMIBOLD))
                                .when(row.quiet, |line| {
                                    line.text_color(theme::material(cx).glass_muted)
                                }),
                        )
                        .children(row.detail.map(|detail| {
                            let color = theme::material(cx).glass_muted;
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(color)
                                .children(row.detail_icon.map(|icon| {
                                    super::icon(icon, gpui_kit::component::Size::XSmall, color)
                                        .flex_none()
                                }))
                                .child(ellipsis(detail))
                        })),
                )
                .children(row.trailing.map(|text| {
                    div()
                        .text_sm()
                        .text_color(theme::material(cx).glass_muted)
                        .child(text)
                })),
        )
}
