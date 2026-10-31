use super::ellipsis;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, MouseButton, SharedString, Window, div,
    prelude::*,
};
use gpui_kit::component::{ActiveTheme as _, list::ListItem};

/// What a list row shows.
pub struct ListRow {
    pub title: SharedString,
    /// The quiet line under the title.
    pub detail: Option<String>,
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
                                    line.text_color(cx.theme().muted_foreground)
                                }),
                        )
                        .children(row.detail.map(|detail| {
                            ellipsis(detail)
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                        })),
                )
                .children(row.trailing.map(|text| {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(text)
                })),
        )
}
