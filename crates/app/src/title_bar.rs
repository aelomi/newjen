//! The bar across the top of the chat (screens 8–11): the thread's title and folder in the
//! middle; the "N allowed" chip with its list of remembered rules and the status pill on the
//! right; and, while the thread list is out of sight, the button that brings it back on the
//! left.
//!
//! Ely's `TitleBar` centers one plain string, and the mock wants a semibold title beside a mono
//! path, so the bar is built from Ely's `drag_region` (which gives it window dragging and
//! double-click zoom) the way `TitleBar` builds itself.

use std::rc::Rc;

use ely_gpui_component::agent::{AgentState, AgentStatus};
use ely_gpui_component::buttons::{Button, ButtonVariant};
use ely_gpui_component::data_display::{Badge, Tone};
use ely_gpui_component::layout::{h_stack, v_stack};
use ely_gpui_component::overlays::Popover;
use ely_gpui_component::primitives::{Icon, IconName};
use ely_gpui_component::shell::drag_region;
use ely_gpui_component::theme::{ActiveTheme, ControlSize, IconSize, TextSize};
use ely_gpui_component::typography::{Ellipsis, MiddleEllipsis};
use gpui::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use newjen_core::{Grant, PermissionAnswer, Status};

use crate::keys::{self, ApprovalKey};
use crate::sidebar::{self, OnPress};

/// The bar's side padding, as in the mock.
const BAR_PADDING: f32 = 14.0;
/// About how wide one character of the path (12 px mono) is, and the share of the chat's
/// width the path may take beside the title.
const PATH_CHAR: f32 = 7.2;
const PATH_SHARE: f32 = 0.25;
/// The path is never cut shorter than this many characters, nor kept longer.
const PATH_MIN_CHARS: usize = 12;
const PATH_MAX_CHARS: usize = 48;
/// The grants popover's width, as in the mock (330 px with its own padding).
const GRANTS_WIDTH: f32 = 306.0;
/// Everything in the grants popover that is not a rule's row, top to bottom: its padding and
/// border (`p_3`), the header (title, the sub line wrapped to two lines, `pt_1p5` + `pb_2`),
/// the footer (`mt_0p5`, `pt_2`, a line, `pb_1p5`, its border) and the gaps between them.
const GRANTS_CHROME: f32 = 26.0 + 74.0 + 36.0 + 4.0;
/// Room kept between the popover and the window's bottom edge (and the small drop it opens
/// with under the chip).
const GRANTS_MARGIN: f32 = 16.0;
/// The rows always get at least this much, about two rules.
const GRANT_ROWS_MIN: f32 = 100.0;

/// Called with a grant's rule when its Revoke button is pressed.
pub type OnRevoke = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What the bar shows.
pub struct Bar {
    /// The thread's title, e.g. "Fix the flaky orphan test" or "New thread".
    pub title: SharedString,
    /// The folder the thread's claude works in, as `~/…`, already cut to fit.
    pub path: String,
    /// Set while the thread list is out of sight: the button that shows it, and whether
    /// another thread waits for an approval (an amber dot on the button).
    pub show_list: Option<(OnPress, bool)>,
    /// The thread's "Always allow" rules.
    pub grants: Vec<Grant>,
    pub status: Status,
    /// How many commands Claude keeps running in the background.
    pub background_tasks: usize,
}

/// The most height the grants popover's rows may take in a window `viewport_height` pixels
/// tall. The popover hangs from the title bar; Ely does not limit its height, so without this
/// a long list would run off the bottom of the window with its Revoke buttons. Past this the
/// rows scroll, and the header and footer stay put.
pub fn grant_rows_max_height(viewport_height: f32, title_bar_height: f32) -> f32 {
    (viewport_height - title_bar_height - GRANTS_CHROME - GRANTS_MARGIN).max(GRANT_ROWS_MIN)
}

/// How many characters of the path fit beside the title in a chat `main_width` pixels wide.
/// The title gives way first (it is cut with "…"); the path keeps about a quarter of the bar,
/// cut in its middle (`format::short_path`) so its last folders stay readable.
pub fn path_budget(main_width: f32) -> usize {
    ((main_width * PATH_SHARE / PATH_CHAR).max(0.0) as usize).clamp(PATH_MIN_CHARS, PATH_MAX_CHARS)
}

/// Draws the bar.
pub fn render(bar: Bar, on_revoke: OnRevoke, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let height = theme.titlebar_height();
    let inset = f32::from(theme.traffic_light_inset().to_pixels(window.rem_size()));
    let title_size = theme.text_size(TextSize::Base);
    let path_size = theme.text_size(TextSize::Sm);
    let mono = theme.mono_family.clone();
    let fullscreen = window.is_fullscreen();

    // Left: room for the traffic lights, which sit over this bar while the list is hidden, then
    // the button that shows the list.
    let left = div().flex_1().flex().items_center().when_some(
        bar.show_list,
        |left, (on_press, attention)| {
            left.when(!fullscreen, |left| left.pl(px(inset - BAR_PADDING)))
                .child(show_list_button(on_press, attention, cx))
        },
    );
    let middle = h_stack()
        .min_w_0()
        .gap_2()
        .child(
            div()
                .min_w_0()
                .text_size(title_size)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.fg)
                .child(Ellipsis::new(bar.title)),
        )
        .when(!bar.path.is_empty(), |middle| {
            middle.child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .font_family(mono)
                    .text_size(path_size)
                    .text_color(colors.fg_subtle)
                    .child(bar.path),
            )
        });
    let right = div().flex_1().flex().justify_end().child(
        // Over the drag region, so a press here is the control's, not a window drag.
        h_stack()
            .gap_1p5()
            .occlude()
            .when(!bar.grants.is_empty(), |right| {
                right.child(grants_chip(bar.grants, on_revoke, cx))
            })
            .children(status_pill(bar.status, bar.background_tasks)),
    );
    drag_region("title-bar", window, cx)
        .flex_none()
        .h(height)
        .flex()
        .items_center()
        .gap_2()
        .px(px(BAR_PADDING))
        .bg(colors.bg)
        .child(left)
        .child(middle)
        .child(right)
        .into_any_element()
}

/// The panel-left button, with an amber dot while a hidden thread needs an approval.
fn show_list_button(on_press: OnPress, attention: bool, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    div()
        .relative()
        .occlude()
        .child(sidebar::toggle_button(
            "sidebar-show",
            "Show sidebar",
            on_press,
        ))
        .when(attention, |button| {
            button.child(
                div()
                    .absolute()
                    .top(px(3.0))
                    .right(px(3.0))
                    .size(px(7.0))
                    .rounded_full()
                    .bg(colors.warning)
                    .border_1()
                    .border_color(colors.bg),
            )
        })
        .into_any_element()
}

/// How the status pill looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pill {
    /// Ely's agent pill (with its spinner while working).
    Agent(AgentState),
    /// A badge with a dot: this word, in this tone.
    Badge(&'static str, Tone),
    /// A blue badge with a dot: this many commands still run in the background.
    Background(usize),
}

/// The pill for a thread's status, worded as the mocks and the thread list word it (screens
/// 8–11): "Needs approval" in amber, a quiet "Idle" once a turn is done, a quiet "Stopped",
/// and "Background task running" instead of those (and of failed) while a command Claude
/// started still runs. Ely's agent pill stays for working (its spinner shows life) and failed.
/// A thread with no message yet has none.
pub fn pill(status: Status, background_tasks: usize) -> Option<Pill> {
    Some(match status {
        Status::Working => Pill::Agent(AgentState::Working),
        Status::Waiting => Pill::Badge(sidebar::NEEDS_APPROVAL, Tone::Warning),
        _ if background_tasks > 0 => Pill::Background(background_tasks),
        Status::Failed => Pill::Agent(AgentState::Failed),
        // The turn's own "Done in 4.2s" line and the list's "Done · 4m ago" already say it
        // finished; the pill says what the thread is doing now.
        Status::Done => Pill::Badge("Idle", Tone::Neutral),
        Status::Stopped => Pill::Badge("Stopped", Tone::Neutral),
        Status::Idle => return None,
    })
}

fn status_pill(status: Status, background_tasks: usize) -> Option<AnyElement> {
    Some(match pill(status, background_tasks)? {
        Pill::Agent(state) => AgentStatus::new("agent-status", state).into_any_element(),
        Pill::Badge(label, tone) => Badge::new(label).dot().tone(tone).into_any_element(),
        Pill::Background(count) => Badge::new(sidebar::background_detail(count))
            .dot()
            .tone(Tone::Info)
            .into_any_element(),
    })
}

/// "🛡 2 allowed": opens the list of the thread's remembered rules.
fn grants_chip(grants: Vec<Grant>, on_revoke: OnRevoke, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = theme.colors.clone();
    let label_size = theme.text_size(TextSize::Xs);
    let label = format!("{} allowed", grants.len());
    Popover::with_opener(
        "grants",
        move |toggle| {
            div()
                .id("grants-chip")
                .flex()
                .items_center()
                .gap(px(5.0))
                .px(px(9.0))
                .py(px(3.0))
                .rounded(px(10.0))
                .bg(colors.hover)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(move |_, window, cx| toggle(window, cx))
                .child(
                    Icon::new(IconName::Shield)
                        .size(IconSize::Xs)
                        .color(colors.fg_muted),
                )
                .child(
                    div()
                        .text_size(label_size)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.fg_muted)
                        .child(label),
                )
        },
        move |_close, window, cx| grants_panel(&grants, &on_revoke, window, cx),
    )
    .into_any_element()
}

/// The popover's content (the "Allowed rules popover" frame of the mock).
fn grants_panel(grants: &[Grant], on_revoke: &OnRevoke, window: &Window, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let colors = &theme.colors;
    let small = theme.text_size(TextSize::Xs);
    let rem = window.rem_size();
    let rows_height = grant_rows_max_height(
        f32::from(window.viewport_size().height),
        f32::from(theme.titlebar_height().to_pixels(rem)),
    );
    let header = v_stack()
        .gap_0p5()
        .px_2()
        .pt_1p5()
        .pb_2()
        .child(
            div()
                .text_size(theme.text_size(TextSize::Base))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.fg)
                .child("Always allowed in this thread"),
        )
        .child(
            div()
                .text_size(small)
                .text_color(colors.fg_subtle)
                .child("Claude runs these without asking, until you revoke them."),
        );
    let rows = grants.iter().enumerate().map(|(ix, grant)| {
        let parts = grant_parts(&grant.rule, &grant.label);
        let rule = grant.rule.clone();
        let on_revoke = on_revoke.clone();
        h_stack()
            .gap_2()
            .px_2()
            .py_1p5()
            .child(
                div()
                    .flex_none()
                    .text_size(small)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.fg_muted)
                    .child(parts.tool),
            )
            .child(
                v_stack()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .font_family(theme.mono_family.clone())
                            .text_size(theme.text_size(TextSize::Sm))
                            .text_color(colors.fg)
                            .child(MiddleEllipsis::new(parts.shown)),
                    )
                    .children(parts.note.map(|note| {
                        div()
                            .min_w_0()
                            .text_size(small)
                            .text_color(colors.fg_subtle)
                            .child(Ellipsis::new(note))
                    })),
            )
            .child(
                Button::new(("revoke", ix), "Revoke")
                    .variant(ButtonVariant::Outline)
                    .size(ControlSize::Sm)
                    .on_click(move |_, window, cx| on_revoke(&rule, window, cx)),
            )
    });
    let footer = div()
        .mt_0p5()
        .px_2()
        .pt_2()
        .pb_1p5()
        .border_t_1()
        .border_color(colors.border)
        .text_size(small)
        .text_color(colors.fg_subtle)
        .child("A revoke takes effect before your next message.");
    // Only the rows scroll (it needs an id to keep its scroll position).
    let rows = v_stack()
        .id("grant-rows")
        .max_h(px(rows_height))
        .overflow_y_scroll()
        .gap_0p5()
        .children(rows);
    v_stack()
        .w(px(GRANTS_WIDTH))
        .gap_0p5()
        // ⌘↵ allows the pending request (see `keys::key_route`), but not from in here, even
        // with Tab on a Revoke button: someone reading the allowed rules is not looking at the
        // request. Esc goes on, so the popover still closes.
        .on_key_down(|event, _, cx| {
            if keys::approval_key(event) == Some(ApprovalKey::Answer(PermissionAnswer::AllowOnce)) {
                cx.stop_propagation();
            }
        })
        .child(header)
        .child(rows)
        .child(footer)
        .into_any_element()
}

/// One remembered rule as the popover shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantParts {
    /// The tool, e.g. "Bash", or "MCP" for one MCP tool.
    pub tool: String,
    /// The rule on one line, e.g. `touch one.txt` or `pencil · get_style`.
    pub shown: String,
    /// What it allows, in words: "exact command", "git add with any arguments", "this tool
    /// only". `None` when the rule says it best itself.
    pub note: Option<String>,
}

/// Splits a grant (its CLI `rule`, such as `Bash(git add:*)`, and its `label`, such as
/// `git add:*`) into what the popover shows.
pub fn grant_parts(rule: &str, label: &str) -> GrantParts {
    // One line: a multi-line command's line breaks are shown as ↵, as the approval dock does.
    let shown = label.replace('\n', " ↵ ");
    let tool = rule.split('(').next().unwrap_or(rule);
    if !rule.contains('(') && tool.starts_with("mcp__") {
        return GrantParts {
            tool: "MCP".to_string(),
            shown,
            note: Some("this tool only".to_string()),
        };
    }
    // In a Bash rule a trailing `:*` or ` *` means "any arguments"; another `*` is a pattern.
    let note = if tool == "Bash" {
        let prefix = label
            .strip_suffix(":*")
            .or_else(|| label.strip_suffix(" *"))
            .map(str::trim_end)
            .filter(|prefix| !prefix.is_empty() && !prefix.contains('*'));
        Some(match prefix {
            Some(prefix) => format!("{} with any arguments", prefix.replace('\n', " ↵ ")),
            None if label.contains('*') => "a pattern".to_string(),
            None => "exact command".to_string(),
        })
    } else {
        label.contains('*').then(|| "a pattern".to_string())
    };
    GrantParts {
        tool: tool.to_string(),
        shown,
        note,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(rule: &str, label: &str) -> (String, String, Option<String>) {
        let parts = grant_parts(rule, label);
        (parts.tool, parts.shown, parts.note)
    }

    #[test]
    fn the_path_keeps_a_quarter_of_the_bar() {
        assert_eq!(path_budget(640.0), 22);
        assert_eq!(path_budget(960.0), 33);
        // Never too short to read, never so long it pushes the title away.
        assert_eq!(path_budget(100.0), PATH_MIN_CHARS);
        assert_eq!(path_budget(4000.0), PATH_MAX_CHARS);
    }

    #[test]
    fn the_pill_says_what_the_list_says() {
        assert_eq!(
            pill(Status::Waiting, 0),
            Some(Pill::Badge("Needs approval", Tone::Warning))
        );
        assert_eq!(
            pill(Status::Done, 0),
            Some(Pill::Badge("Idle", Tone::Neutral))
        );
        assert_eq!(
            pill(Status::Stopped, 0),
            Some(Pill::Badge("Stopped", Tone::Neutral))
        );
        assert_eq!(
            pill(Status::Working, 0),
            Some(Pill::Agent(AgentState::Working))
        );
        assert_eq!(
            pill(Status::Failed, 0),
            Some(Pill::Agent(AgentState::Failed))
        );
        assert_eq!(pill(Status::Idle, 0), None);
    }

    #[test]
    fn a_command_left_running_in_the_background_replaces_idle() {
        assert_eq!(pill(Status::Done, 1), Some(Pill::Background(1)));
        assert_eq!(pill(Status::Stopped, 2), Some(Pill::Background(2)));
        // The failed turn's error row is on screen right below.
        assert_eq!(pill(Status::Failed, 1), Some(Pill::Background(1)));
        // A running turn or an approval says more.
        assert_eq!(
            pill(Status::Working, 1),
            Some(Pill::Agent(AgentState::Working))
        );
        assert_eq!(
            pill(Status::Waiting, 1),
            Some(Pill::Badge("Needs approval", Tone::Warning))
        );
        // The same words as the thread's row.
        assert_eq!(sidebar::background_detail(1), "Background task running");
        assert_eq!(sidebar::background_detail(2), "2 background tasks running");
    }

    #[test]
    fn a_long_grant_list_scrolls_inside_the_window() {
        // At the smallest window, rows up to the bottom edge, header and footer included.
        let small = grant_rows_max_height(480.0, 38.0);
        assert_eq!(small, 480.0 - 38.0 - GRANTS_CHROME - GRANTS_MARGIN);
        assert!(38.0 + GRANTS_CHROME + small + GRANTS_MARGIN <= 480.0);
        // About five 50 px rules fit before the rows scroll at 640×480, a dozen at 760 tall.
        assert!((250.0..300.0).contains(&small), "{small}");
        assert!(grant_rows_max_height(760.0, 38.0) > 500.0);
        // A tiny height still leaves room for a couple of rules.
        assert_eq!(grant_rows_max_height(100.0, 38.0), GRANT_ROWS_MIN);
    }

    #[test]
    fn grants_read_like_the_popover_mock() {
        assert_eq!(
            parts("Bash(touch one.txt)", "touch one.txt"),
            (
                "Bash".to_string(),
                "touch one.txt".to_string(),
                Some("exact command".to_string())
            )
        );
        assert_eq!(
            parts("Bash(git add:*)", "git add:*"),
            (
                "Bash".to_string(),
                "git add:*".to_string(),
                Some("git add with any arguments".to_string())
            )
        );
        assert_eq!(
            parts("mcp__pencil__get_style", "pencil · get_style"),
            (
                "MCP".to_string(),
                "pencil · get_style".to_string(),
                Some("this tool only".to_string())
            )
        );
    }

    #[test]
    fn patterns_and_other_tools_are_named_honestly() {
        assert_eq!(
            parts("Bash(npm test *)", "npm test *").2.as_deref(),
            Some("npm test with any arguments")
        );
        assert_eq!(
            parts("Bash(rm *.log)", "rm *.log").2.as_deref(),
            Some("a pattern")
        );
        // A bare `*` allows anything: never "with any arguments" of nothing.
        assert_eq!(parts("Bash(*)", "*").2.as_deref(), Some("a pattern"));
        assert_eq!(
            parts("WebFetch(domain:*.example.com)", "domain:*.example.com"),
            (
                "WebFetch".to_string(),
                "domain:*.example.com".to_string(),
                Some("a pattern".to_string())
            )
        );
        assert_eq!(
            parts("WebFetch(domain:example.com)", "domain:example.com").2,
            None
        );
        // Escaped brackets in the CLI rule do not confuse the tool name.
        assert_eq!(
            parts(r"Bash(sed 's/\\\(a\\\)/b/' f)", r"sed 's/\(a\)/b/' f").0,
            "Bash"
        );
        // A multi-line command stays on one line.
        assert_eq!(
            parts("Bash(echo a\necho b)", "echo a\necho b").1,
            "echo a ↵ echo b"
        );
    }
}
