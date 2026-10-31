use super::IconName;
use gpui::{prelude::*, *};
use gpui_kit::component::{ActiveTheme as _, Icon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Working,
    Done,
    Failed,
}

pub fn step_mark(state: StepState, cx: &App) -> AnyElement {
    let (name, color) = match state {
        StepState::Waiting | StepState::Working => (IconName::Circle, cx.theme().muted_foreground),
        StepState::Done => (IconName::Check, cx.theme().success),
        StepState::Failed => (IconName::Close, cx.theme().danger),
    };
    Icon::new(name).text_color(color).into_any_element()
}
