//! Draws one row of the conversation (one transcript `Item`) with Ely's chat and agent
//! components. Each function returns a finished element; the chat view lays them out.

use std::rc::Rc;

use ely_gpui_component::agent::ToolCallCard;
use ely_gpui_component::chat::{ErrorMessage, MessageBubble, Role, StepState, StreamingMarkdown};
use ely_gpui_component::layout::h_stack;
use ely_gpui_component::primitives::{Icon, IconName};
use ely_gpui_component::theme::{ActiveTheme, IconSize};
use ely_gpui_component::typography::Caption;
use gpui::{AnyElement, App, ElementId, IntoElement, ParentElement, SharedString, Styled, Window};
use newjen_core::{Item, ToolState};

use crate::format::{cap, cap_ends, turn_caption};

/// Tool output and arguments longer than this are cut (Ely's code block folds after 14 lines
/// anyway; this keeps huge outputs from slowing layout).
const TOOL_TEXT_LINES: usize = 200;
const TOOL_TEXT_CHARS: usize = 20_000;
/// Ely shows a failed tool's output as plain text with no fold, so it gets a much smaller cap:
/// the first and last lines (error output usually ends with its summary).
const FAILED_HEAD: usize = 8;
const FAILED_TAIL: usize = 16;
const FAILED_LINE_CHARS: usize = 240;

/// Called when the user presses "Try again" on an error.
pub type OnRestart = Rc<dyn Fn(&mut Window, &mut App)>;

/// Draws one transcript item. Element ids come from the item's stable id, so Ely's per-element
/// state (streaming reveal, open/closed cards) survives re-renders.
///
/// `is_last` says whether this is the conversation's last item. Only that one can still be
/// streaming: core keeps every reply of the turn `live` until the turn ends, but a reply
/// followed by something else is finished, so it must not show a typing caret.
///
/// `retry` is the "Try again" action for an error row, given only where `can_retry` allows it.
pub fn item(item: &Item, is_last: bool, retry: Option<&OnRestart>, cx: &mut App) -> AnyElement {
    match item {
        Item::User { text, .. } => MessageBubble::new(Role::User)
            .child(text.clone())
            .into_any_element(),
        Item::Assistant { id, text, live, .. } => MessageBubble::new(Role::Assistant)
            .child(StreamingMarkdown::new(
                ("reply", *id),
                text.clone(),
                *live && is_last,
            ))
            .into_any_element(),
        Item::Tool {
            id,
            display_name,
            summary,
            input,
            state,
            output,
            took,
            ..
        } => {
            let mut card =
                ToolCallCard::new(("tool", *id), display_name.clone(), step_state(*state));
            if !summary.is_empty() {
                card = card.summary(summary.clone());
            }
            // A denied call never ran, so it has no time to show.
            if let Some(took) = took.filter(|_| *state != ToolState::Denied) {
                card = card.took(took);
            }
            if let Ok(arguments) = serde_json::to_string_pretty(input) {
                card = card.arguments(cap(&arguments, TOOL_TEXT_LINES, TOOL_TEXT_CHARS));
            }
            // Borrowed, not cloned: this runs on every frame and outputs can be megabytes.
            let result = match state {
                ToolState::Denied => Some("Denied"),
                _ => output.as_deref(),
            };
            if let Some(result) = result.filter(|text| !text.is_empty()) {
                let shown = match state {
                    ToolState::Failed => {
                        cap_ends(result, FAILED_HEAD, FAILED_TAIL, FAILED_LINE_CHARS)
                    }
                    _ => cap(result, TOOL_TEXT_LINES, TOOL_TEXT_CHARS),
                };
                card = card.result(shown);
            }
            card.into_any_element()
        }
        Item::TurnEnd {
            duration_ms,
            cost_usd,
            stopped,
            ..
        } => turn_end(
            turn_caption(*duration_ms, *cost_usd, *stopped),
            *stopped,
            cx,
        ),
        Item::Error {
            id, title, detail, ..
        } => error(("error", *id).into(), title, detail, retry),
    }
}

/// An error that happened in the app rather than in Claude (for example `claude` could not be
/// started). Drawn like a transcript error. With no `retry`, Ely draws no "Try again" button.
pub fn error(id: ElementId, title: &str, detail: &str, retry: Option<&OnRestart>) -> AnyElement {
    let mut message = ErrorMessage::new(id, SharedString::from(title.to_string()))
        .detail(SharedString::from(detail.to_string()));
    if let Some(restart) = retry.cloned() {
        message = message.on_retry(move |window, cx| restart(window, cx));
    }
    MessageBubble::new(Role::Error)
        .child(message)
        .into_any_element()
}

/// Whether an error row gets a "Try again" button. Restarting kills the running `claude`, so
/// only the newest error (the last row) offers it, and only while no turn is running: an old
/// error's button clicked mid-turn would throw that turn away. The app's own notice is the
/// exception: it says Newjen could not write to `claude` at all, so the running turn can
/// neither be answered nor stopped, and restarting is the only way on.
pub fn can_retry(is_last_row: bool, is_notice: bool, busy: bool) -> bool {
    is_last_row && (is_notice || !busy)
}

/// The quiet line under a finished turn: "✓ Done in 4.2s · $0.0041".
fn turn_end(caption: String, stopped: bool, cx: &mut App) -> AnyElement {
    let color = cx.theme().colors.fg_subtle;
    let icon = if stopped {
        IconName::Square
    } else {
        IconName::Check
    };
    h_stack()
        .gap_1p5()
        .child(Icon::new(icon).size(IconSize::Xs).color(color))
        .child(Caption::new(caption).text_color(color))
        .into_any_element()
}

/// Ely's four step states. A denied call is shown as failed (with "Denied" as its result).
pub fn step_state(state: ToolState) -> StepState {
    match state {
        ToolState::Running => StepState::Working,
        ToolState::Waiting => StepState::Waiting,
        ToolState::Done => StepState::Done,
        ToolState::Denied | ToolState::Failed => StepState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_states_map_to_ely_steps() {
        assert_eq!(step_state(ToolState::Running), StepState::Working);
        assert_eq!(step_state(ToolState::Waiting), StepState::Waiting);
        assert_eq!(step_state(ToolState::Done), StepState::Done);
        assert_eq!(step_state(ToolState::Denied), StepState::Failed);
        assert_eq!(step_state(ToolState::Failed), StepState::Failed);
    }

    #[test]
    fn only_the_newest_error_can_retry_and_not_mid_turn() {
        assert!(can_retry(true, false, false));
        // An older error, further up the conversation.
        assert!(!can_retry(false, false, false));
        // The newest error, but a new turn is already running.
        assert!(!can_retry(true, false, true));
        // Newjen's own notice: the pipe to claude is broken, so restarting is the way out.
        assert!(can_retry(true, true, true));
        assert!(!can_retry(false, true, false));
    }
}
