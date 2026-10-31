//! Transcript rows. The chat view supplies reply states and lays out the column.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, IntoElement, ParentElement, Styled, Window, div, prelude::*, px,
};
use gpui_kit::base::{h_flex, v_flex};
use newjen_core::transcript::{
    BACKGROUND_STOPPED_NOTE, EXPIRED_NOTE, FRESH_START_NOTE, STOPPED_ON_QUIT,
    STOPPED_ON_QUIT_DETAIL, STOPPED_TOOL_NOTE, STOPPED_UNSENT_DETAIL, UNREADABLE_NOTE,
};
use newjen_core::{Item, ToolState};

use crate::approval::ScopeRow;
use crate::format::{cap, cap_ends, turn_caption};

use crate::ui::{self, Look, StepState, ToolCard};
use gpui_kit::component::ActiveTheme as _;

// Bound large tool outputs to keep transcript layout responsive.
const TOOL_TEXT_LINES: usize = 200;
const TOOL_TEXT_CHARS: usize = 20_000;
// Error output usually ends with its summary.
const FAILED_HEAD: usize = 8;
const FAILED_TAIL: usize = 16;
const FAILED_LINE_CHARS: usize = 240;
/// The shortest the hairlines beside the fresh-start note get in a narrow chat.
const HAIRLINE_MIN: f32 = 24.0;

/// Called when the user presses "Try again" on an error, or Continue on a stopped thread.
pub type OnRestart = Rc<dyn Fn(&mut Window, &mut App)>;

/// Draws a transcript item with its owned reply state and available actions.
pub fn item(
    item: &Item,
    reply: Option<gpui_kit::component::text::TextView>,
    retry: Option<&OnRestart>,
    resume: Option<(&OnRestart, &'static str)>,
    resends: bool,
    cx: &App,
) -> AnyElement {
    match item {
        Item::User { text, .. } => ui::user(text.clone(), cx).into_any_element(),
        Item::Assistant { .. } => ui::assistant(div().children(reply), cx).into_any_element(),
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
            let mut card = ToolCard::new(("tool", *id), display_name.clone(), step_state(*state));
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
        } => error(("error", *id).into(), title, detail, &[], retry, cx),
        Item::Stopped { id } => stopped_notice(*id, resume, resends, cx),
        Item::FreshStart { .. } => fresh_start(cx),
        Item::Unreadable { .. } => caption_row(ui::IconName::Info, UNREADABLE_NOTE.to_string(), cx),
    }
}

/// Where Claude's memory of the conversation starts over: its reason, centered between two
/// hairlines. The hairlines never shrink away and a label too long for one line wraps centered
/// between them: the reason is a long sentence, and the chat column is narrower than it in
/// small windows.
fn fresh_start(cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let line = || {
        div()
            .flex_1()
            .min_w(px(HAIRLINE_MIN))
            .border_t_1()
            .border_color(colors.border)
    };
    h_flex()
        .gap_3()
        .child(line())
        .child(
            div()
                .min_w_0()
                .text_center()
                .text_xs()
                .text_color(colors.muted_foreground)
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

/// The label of that Continue button: "Send again" when it sends the user's last message
/// again because Claude never started its turn (`Thread::continue_resends`).
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
    v_flex()
        .gap_2()
        .child(
            gpui_kit::component::alert::Alert::new(("stopped", id), stopped_detail(resends))
                .title(STOPPED_ON_QUIT)
                .icon(ui::IconName::CirclePause),
        )
        .children(resume.map(|(resume, label)| {
            let resume = resume.clone();
            h_flex().child(
                ui::button(("continue", id), label, Look::Secondary, ui::Size::Md, cx)
                    .on_click(move |_, window, cx| resume(window, cx)),
            )
        }))
        .into_any_element()
}

/// How a tool call's card looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolLook {
    /// A tool card in this step state.
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

/// A quiet tool card: the tool card's frame and header (screen 9 shows them side by side), with
/// a muted icon in place of the status mark and the note where the time goes.
fn quiet_tool(name: &str, summary: &str, icon: QuietIcon, note: String, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let icon = match icon {
        QuietIcon::Expired => ui::IconName::Clock,
        QuietIcon::Stopped => ui::IconName::CirclePause,
    };
    let header = h_flex()
        .gap_2()
        .px_3()
        .py_2()
        .child(div().flex_none().child(ui::icon(
            icon,
            gpui_kit::component::Size::Small,
            colors.muted_foreground,
        )))
        .child(
            div()
                .min_w_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(colors.foreground)
                .child(ui::ellipsis(name.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(colors.muted_foreground)
                .child(ui::ellipsis(summary.to_string())),
        )
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .text_color(colors.muted_foreground)
                .child(note),
        );
    div()
        .w_full()
        .rounded(theme.radius)
        .border_1()
        .border_color(colors.border)
        .bg(colors.background)
        .child(header)
        .into_any_element()
}

/// An error that happened in the app rather than in Claude (for example `claude` could not be
/// started). A retry button is shown only when another attempt is available.
pub fn error(
    id: ElementId,
    title: &str,
    detail: &str,
    output: &[String],
    retry: Option<&OnRestart>,
    cx: &App,
) -> AnyElement {
    ui::assistant(
        ui::ErrorNotice {
            id,
            title: title.to_string().into(),
            detail: detail.to_string().into(),
            output: output.iter().cloned().map(Into::into).collect(),
            retry: retry.cloned(),
        },
        cx,
    )
    .into_any_element()
}

pub fn held(id: ElementId, title: &str, detail: &str, cx: &App) -> AnyElement {
    ui::assistant(
        gpui_kit::component::alert::Alert::info(id, detail.to_string()).title(title.to_string()),
        cx,
    )
    .into_any_element()
}

pub fn making_copy(took: &str, last_line: Option<&str>, cx: &App) -> AnyElement {
    use gpui_kit::component::Sizable as _;
    let color = cx.theme().colors.muted_foreground;
    h_flex()
        .debug_selector(|| "making-copy".to_string())
        .items_start()
        .gap_2()
        .child(
            div().flex_none().pt_0p5().child(
                gpui_kit::component::spinner::Spinner::new()
                    .small()
                    .color(color),
            ),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(ui::caption(format!("Creating a Rift copy · {took}"), cx))
                .children(last_line.map(|line| {
                    div()
                        .min_w_0()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_xs()
                        .text_color(color)
                        .child(ui::ellipsis(line.to_string()))
                })),
        )
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
        ui::IconName::Square
    } else {
        ui::IconName::Check
    };
    caption_row(icon, caption, cx)
}

/// The quiet line under the approval dock saying what "Always allow" does, with the remembered
/// rule in the mono font. Kept to one line (the rule is already cut to fit, see
/// `approval::scope_row`), because the dock counts it as exactly one row.
pub fn scope(row: &ScopeRow, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let color = theme.colors.muted_foreground;
    // The gaps stand in for the spaces around the rule, so they are trimmed off the words.
    let words = h_flex()
        .gap_1()
        .min_w_0()
        .child(ui::caption(row.before.trim_end().to_string(), cx).text_color(color))
        .children(row.code.clone().map(|code| {
            div()
                .min_w_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_xs()
                .text_color(theme.colors.muted_foreground)
                .text_ellipsis_middle()
                .child(code)
        }))
        .when(!row.after.is_empty(), |line| {
            line.child(ui::caption(row.after.trim_start().to_string(), cx).text_color(color))
        });
    h_flex()
        .gap_1p5()
        .px_1()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(ui::icon(
            ui::IconName::Info,
            gpui_kit::component::Size::XSmall,
            color,
        ))
        .child(words)
        .into_any_element()
}

/// A small icon and a caption, both in the subtle text color.
fn caption_row(icon: ui::IconName, caption: String, cx: &App) -> AnyElement {
    let color = cx.theme().colors.muted_foreground;
    h_flex()
        .gap_1p5()
        .child(ui::icon(icon, gpui_kit::component::Size::XSmall, color))
        .child(ui::caption(caption, cx).text_color(color))
        .into_any_element()
}

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
    fn tool_states_map_to_steps() {
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
        // A failed call retains its result.
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
