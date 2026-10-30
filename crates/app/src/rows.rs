//! Draws one row of the conversation (one transcript `Item`) with Ely's chat and agent
//! components. Each function returns a finished element; the chat view lays them out.

use std::rc::Rc;

use ely_gpui_component::agent::ToolCallCard;
use ely_gpui_component::buttons::{Button, ButtonVariant};
use ely_gpui_component::chat::{ErrorMessage, MessageBubble, Role, StepState, StreamingMarkdown};
use ely_gpui_component::layout::{h_stack, v_stack};
use ely_gpui_component::primitives::{Icon, IconName};
use ely_gpui_component::theme::{ActiveTheme, IconSize, Radius, TextSize};
use ely_gpui_component::typography::{Caption, Ellipsis};
use gpui::{
    AnyElement, App, ElementId, FontWeight, IntoElement, ParentElement, SharedString, Styled,
    Window, div, prelude::*, px,
};
use newjen_core::transcript::{
    BACKGROUND_STOPPED_NOTE, EXPIRED_NOTE, FRESH_START_NOTE, STOPPED_ON_QUIT,
    STOPPED_ON_QUIT_DETAIL, STOPPED_TOOL_NOTE, STOPPED_UNSENT_DETAIL, UNREADABLE_NOTE,
};
use newjen_core::{Item, ToolState};

use crate::approval::ScopeRow;
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
/// The shortest the hairlines beside the fresh-start note get in a narrow chat.
const HAIRLINE_MIN: f32 = 24.0;

/// Called when the user presses "Try again" on an error, or Continue on a stopped thread.
pub type OnRestart = Rc<dyn Fn(&mut Window, &mut App)>;

/// Draws one transcript item. Element ids come from the item's stable id, so Ely's per-element
/// state (streaming reveal, open/closed cards) survives re-renders.
///
/// `typing` says whether a reply here may type in: only the conversation's last item can still
/// be streaming (core keeps every reply of the turn `live` until the turn ends, but a reply
/// followed by something else is finished, so it must not show a typing caret), and not on the
/// first frame after its thread comes on screen (see `live_reply`).
///
/// `retry` is the "Try again" action for an error row, given only where `can_retry` allows it.
/// `resume` is the Continue action for the "Stopped when Newjen quit" notice and its label (see
/// `continue_label`), given only where `shows_continue` allows it.
pub fn item(
    item: &Item,
    typing: bool,
    retry: Option<&OnRestart>,
    resume: Option<(&OnRestart, &'static str)>,
    resends: bool,
    cx: &App,
) -> AnyElement {
    match item {
        Item::User { text, .. } => MessageBubble::new(Role::User)
            .child(text.clone())
            .into_any_element(),
        Item::Assistant { id, text, live, .. } => MessageBubble::new(Role::Assistant)
            .child(StreamingMarkdown::new(
                ("reply", *id),
                text.clone(),
                *live && typing,
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
            // A call Newjen's quit cut short is not a failure: a quiet card says what happened.
            if let ToolLook::Quiet { icon, note } = tool_look(*state, output.as_deref()) {
                return quiet_tool(display_name, summary, icon, note, cx);
            }
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
        Item::Stopped { id } => stopped_notice(*id, resume, resends, cx),
        Item::FreshStart { .. } => fresh_start(cx),
        Item::Unreadable { .. } => caption_row(IconName::Info, UNREADABLE_NOTE.to_string(), cx),
    }
}

/// The reply still streaming at the end of `items`, if any: its id. When a thread comes on
/// screen, this reply is drawn whole on its first frame rather than typed in from its first
/// word. Ely's `StreamingMarkdown` remembers how much it has shown only while it is drawn, so
/// a thread that was off screen would otherwise replay its whole reply, and switching threads
/// must be instant. Text that arrives after that frame types in as usual.
pub fn live_reply(items: &[Item]) -> Option<u64> {
    match items.last() {
        Some(Item::Assistant { id, live: true, .. }) => Some(*id),
        _ => None,
    }
}

/// Where Claude's memory of the conversation starts over: its reason, centered between two
/// hairlines, drawn like Ely's labelled `Divider`. Unlike that one, the hairlines never shrink
/// away and a label too long for one line wraps centered between them: the reason is a long
/// sentence, and the chat column is narrower than it in small windows.
fn fresh_start(cx: &App) -> AnyElement {
    let theme = cx.theme();
    let line = || {
        div()
            .flex_1()
            .min_w(px(HAIRLINE_MIN))
            .border_t_1()
            .border_color(theme.colors.border)
    };
    h_stack()
        .items_center()
        .gap_3()
        .child(line())
        .child(
            div()
                .min_w_0()
                .text_center()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(theme.colors.fg_subtle)
                .child(FRESH_START_NOTE),
        )
        .child(line())
        .into_any_element()
}

/// Whether the "Stopped when Newjen quit" notice gets its Continue button: only while it is
/// the conversation's last row. Anything after it (a message, a turn, the fresh-start divider)
/// means the thread already went on, and the composer is the way forward.
pub fn shows_continue(items: &[Item]) -> bool {
    matches!(items.last(), Some(Item::Stopped { .. }))
}

/// The label of that Continue button: "Send again" when it sends the user's last message
/// again because Claude never started its turn (`Thread::continue_resends`).
/// Whether the Stopped notice at `ix` is about a message Claude never got: the user's last
/// message before it is still unconfirmed. Stays true after Send again, so the notice keeps
/// saying what happened.
pub fn stopped_resends(items: &[Item], ix: usize) -> bool {
    items[..ix.min(items.len())]
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::User { unconfirmed, .. } => Some(*unconfirmed),
            _ => None,
        })
        .unwrap_or(false)
}

pub fn continue_label(resends: bool) -> &'static str {
    if resends { "Send again" } else { "Continue" }
}

/// The notice's second line: what Continue or Send again will do.
pub fn stopped_detail(resends: bool) -> &'static str {
    if resends {
        STOPPED_UNSENT_DETAIL
    } else {
        STOPPED_ON_QUIT_DETAIL
    }
}

/// The notice a thread gets when Newjen quit during its turn (screen 9): "Stopped when Newjen
/// quit / Nothing runs until you continue.", with Continue when `resume` is given.
fn stopped_notice(
    id: u64,
    resume: Option<(&OnRestart, &'static str)>,
    resends: bool,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let text = v_stack()
        .flex_1()
        .min_w_0()
        .gap_0p5()
        .child(
            div()
                .text_size(theme.text_size(TextSize::Base))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors.fg)
                .child(STOPPED_ON_QUIT),
        )
        .child(
            div()
                .text_size(theme.text_size(TextSize::Sm))
                .text_color(colors.fg_subtle)
                .child(stopped_detail(resends)),
        );
    h_stack()
        .gap_2p5()
        .px_3()
        .py_2p5()
        .rounded(theme.radius(Radius::Lg))
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .child(
            Icon::new(IconName::CirclePause)
                .size(IconSize::Sm)
                .color(colors.fg_muted),
        )
        .child(text)
        .children(resume.map(|(resume, label)| {
            let resume = resume.clone();
            Button::new(("continue", id), label)
                .variant(ButtonVariant::Secondary)
                .on_click(move |_, window, cx| resume(window, cx))
        }))
        .into_any_element()
}

/// How a tool call's card looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolLook {
    /// Ely's tool card in this step state.
    Card(StepState),
    /// A quiet card for a call Newjen's quit cut short: a muted icon and a note where the time
    /// goes, never the red failure styling (which stays for real failures).
    Quiet { icon: QuietIcon, note: String },
}

/// The muted icon of a quiet tool card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietIcon {
    /// A clock: the approval it waited for can no longer be given.
    Expired,
    /// A pause: it was running when Newjen quit, or Newjen stopped its claude.
    Stopped,
}

/// How a call in `state` with `output` is drawn (see "Rendering decisions" in the slice 2
/// spec): an approval Newjen quit before you answered is "Expired · Newjen quit before you
/// answered"; a call that was running then is "Stopped when Newjen quit", and a command left
/// running in the background when Newjen stopped its claude is "Stopped with Claude".
/// Everything else is Ely's card.
pub fn tool_look(state: ToolState, output: Option<&str>) -> ToolLook {
    match state {
        ToolState::Expired => ToolLook::Quiet {
            icon: QuietIcon::Expired,
            note: format!("Expired · {EXPIRED_NOTE}"),
        },
        ToolState::Failed
            if let Some(note @ (STOPPED_TOOL_NOTE | BACKGROUND_STOPPED_NOTE)) = output =>
        {
            ToolLook::Quiet {
                icon: QuietIcon::Stopped,
                note: note.to_string(),
            }
        }
        state => ToolLook::Card(step_state(state)),
    }
}

/// A quiet tool card: the same frame and header as Ely's tool card (screen 9 shows them side by
/// side), with a muted icon in place of the status mark and the note where the time goes.
fn quiet_tool(name: &str, summary: &str, icon: QuietIcon, note: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let icon = match icon {
        QuietIcon::Expired => IconName::Clock,
        QuietIcon::Stopped => IconName::CirclePause,
    };
    let header = h_stack()
        .gap_2()
        .px_3()
        .py_2()
        .child(
            div()
                .flex_none()
                .child(Icon::new(icon).size(IconSize::Sm).color(colors.fg_subtle)),
        )
        .child(
            div()
                .min_w_0()
                .font_family(theme.mono_family.clone())
                .text_color(colors.fg)
                .child(Ellipsis::new(name.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(colors.fg_muted)
                .child(Ellipsis::new(summary.to_string())),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(colors.fg_subtle)
                .child(note),
        );
    div()
        .rounded(theme.radius(Radius::Md))
        .border_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .text_size(theme.text_size(TextSize::Sm))
        .child(header)
        .into_any_element()
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
fn turn_end(caption: String, stopped: bool, cx: &App) -> AnyElement {
    let icon = if stopped {
        IconName::Square
    } else {
        IconName::Check
    };
    caption_row(icon, caption, cx)
}

/// The quiet line under the approval dock saying what "Always allow" does, with the remembered
/// rule in the mono font. Kept to one line (the rule is already cut to fit, see
/// `approval::scope_row`), because the dock counts it as exactly one row.
pub fn scope(row: &ScopeRow, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = theme.colors.fg_subtle;
    // The gaps stand in for the spaces around the rule, so they are trimmed off the words.
    let words = h_stack()
        .gap_1()
        .min_w_0()
        .child(Caption::new(row.before.trim_end().to_string()).text_color(color))
        .children(row.code.clone().map(|code| {
            div()
                .min_w_0()
                .font_family(theme.mono_family.clone())
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(theme.colors.fg_muted)
                .text_ellipsis_middle()
                .child(code)
        }))
        .when(!row.after.is_empty(), |line| {
            line.child(Caption::new(row.after.trim_start().to_string()).text_color(color))
        });
    h_stack()
        .gap_1p5()
        .px_1()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(Icon::new(IconName::Info).size(IconSize::Xs).color(color))
        .child(words)
        .into_any_element()
}

/// A small icon and a caption, both in the subtle text color.
fn caption_row(icon: IconName, caption: String, cx: &App) -> AnyElement {
    let color = cx.theme().colors.fg_subtle;
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
        // `tool_look` draws expired calls as quiet cards; this is only a fallback.
        ToolState::Denied | ToolState::Failed | ToolState::Expired => StepState::Failed,
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
    fn calls_cut_short_by_quit_are_quiet_and_real_failures_stay_red() {
        assert_eq!(
            tool_look(ToolState::Expired, Some(EXPIRED_NOTE)),
            ToolLook::Quiet {
                icon: QuietIcon::Expired,
                note: "Expired · Newjen quit before you answered".to_string(),
            }
        );
        assert_eq!(
            tool_look(ToolState::Failed, Some(STOPPED_TOOL_NOTE)),
            ToolLook::Quiet {
                icon: QuietIcon::Stopped,
                note: "Stopped when Newjen quit".to_string(),
            }
        );
        assert_eq!(
            tool_look(ToolState::Failed, Some(BACKGROUND_STOPPED_NOTE)),
            ToolLook::Quiet {
                icon: QuietIcon::Stopped,
                note: "Stopped with Claude".to_string(),
            }
        );
        // A tool that really failed keeps Ely's failed card.
        assert_eq!(
            tool_look(ToolState::Failed, Some("exit code 1")),
            ToolLook::Card(StepState::Failed)
        );
        assert_eq!(
            tool_look(ToolState::Done, None),
            ToolLook::Card(StepState::Done)
        );
    }

    #[test]
    fn continue_shows_only_while_the_stopped_notice_is_last() {
        let user = Item::User {
            id: 1,
            text: "hi".to_string(),
            unconfirmed: false,
        };
        let stopped = Item::Stopped { id: 2 };
        assert!(shows_continue(&[user.clone(), stopped.clone()]));
        // Continue was pressed (its message follows), or the conversation started over.
        let continued = Item::User {
            id: 3,
            text: newjen_core::thread::CONTINUE_MESSAGE.to_string(),
            unconfirmed: true,
        };
        assert!(!shows_continue(&[stopped.clone(), continued]));
        assert!(!shows_continue(&[stopped, Item::FreshStart { id: 3 }]));
        assert!(!shows_continue(&[user]));
        assert!(!shows_continue(&[]));
    }

    #[test]
    fn continue_says_send_again_when_it_resends_the_message() {
        assert_eq!(continue_label(false), "Continue");
        assert_eq!(continue_label(true), "Send again");
        assert_eq!(stopped_detail(false), STOPPED_ON_QUIT_DETAIL);
        assert_eq!(stopped_detail(true), STOPPED_UNSENT_DETAIL);
        let user = |unconfirmed| Item::User {
            id: 1,
            text: "a".into(),
            unconfirmed,
        };
        let items = [user(true), Item::Stopped { id: 2 }, user(false)];
        assert!(stopped_resends(&items, 1), "still says so after Send again");
        assert!(!stopped_resends(&[user(false), Item::Stopped { id: 2 }], 1));
    }

    #[test]
    fn only_a_reply_still_streaming_at_the_end_is_drawn_whole_on_arrival() {
        let reply = |id, live| Item::Assistant {
            id,
            text: "Working on it".to_string(),
            live,
            interrupted: false,
        };
        let user = Item::User {
            id: 1,
            text: "hi".to_string(),
            unconfirmed: false,
        };
        assert_eq!(live_reply(&[user.clone(), reply(2, true)]), Some(2));
        // Finished, or followed by another row: nothing is typing.
        assert_eq!(live_reply(&[user.clone(), reply(2, false)]), None);
        let error = Item::Error {
            id: 3,
            title: "Claude exited".to_string(),
            detail: String::new(),
        };
        assert_eq!(live_reply(&[reply(2, true), error]), None);
        assert_eq!(live_reply(&[user]), None);
        assert_eq!(live_reply(&[]), None);
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
