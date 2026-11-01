use gpui::ParentElement;
use gpui_kit::component::tag::Tag;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
}

pub fn status_badge(label: impl Into<gpui::SharedString>, tone: Tone) -> Tag {
    match tone {
        Tone::Neutral => Tag::secondary(),
    }
    .child(label.into())
}
