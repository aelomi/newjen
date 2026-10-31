use super::IconName;
use gpui::{App, ElementId, SharedString};
use gpui_kit::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants as _},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    Secondary,
    Primary,
    Ghost,
    Outline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Sm,
    Md,
}

fn styled(id: impl Into<ElementId>, look: Look, size: Size) -> Button {
    let button = Button::new(id);
    let button = match look {
        Look::Primary => button.primary(),
        Look::Ghost => button.ghost(),
        Look::Outline => button.outline(),
        Look::Secondary => button,
    };
    match size {
        Size::Sm => button.small(),
        Size::Md => button,
    }
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    look: Look,
    size: Size,
    _: &App,
) -> Button {
    styled(id, look, size).label(label)
}

pub fn button_with_icon(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    glyph: IconName,
    look: Look,
    size: Size,
    _: &App,
) -> Button {
    styled(id, look, size).label(label).icon(glyph)
}

pub fn icon_button(
    id: impl Into<ElementId>,
    glyph: IconName,
    name: &'static str,
    cx: &App,
) -> Button {
    icon_button_with_look(id, glyph, name, Look::Ghost, false, cx)
}

pub fn icon_button_with_look(
    id: impl Into<ElementId>,
    glyph: IconName,
    name: &'static str,
    look: Look,
    disabled: bool,
    _: &App,
) -> Button {
    styled(id, look, Size::Sm)
        .icon(glyph)
        .tooltip(name)
        .accessibility_label(name)
        .disabled(disabled)
}
