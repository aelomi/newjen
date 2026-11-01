use crate::ui::IconName;
use gpui::{ParentElement, Styled};
use gpui_kit::component::{Icon, tag::Tag};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Info,
    Warning,
}

pub fn status_badge(label: impl Into<gpui::SharedString>, tone: Tone) -> Tag {
    match tone {
        Tone::Neutral => Tag::secondary(),
        Tone::Info => Tag::info(),
        Tone::Warning => Tag::warning(),
    }
    .child(label.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Failed,
}

pub fn agent_pill(state: AgentState) -> Tag {
    match state {
        AgentState::Working => Tag::info()
            .child(Icon::new(IconName::Circle))
            .child("Working"),
        AgentState::Failed => Tag::danger()
            .child(Icon::new(IconName::Close))
            .child("Failed"),
    }
    .gap_1()
}
