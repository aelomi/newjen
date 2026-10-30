//! The thread list on the left (screens 8–12 of `design/newjen.pen`): the project, "New
//! thread", and one row per thread with a dot and a line saying how it is doing.
//!
//! The decisions come first as plain functions, tested without a window: what a row says,
//! where the list sits at each window width, which thread ⌘⇧] goes to. The elements that draw
//! them come after; the chat view wires them to the threads.

use std::rc::Rc;
use std::time::Duration;

use ely_gpui_component::buttons::IconButton;
use ely_gpui_component::layout::v_stack;
use ely_gpui_component::lists::ListItem;
use ely_gpui_component::menus::{ContextMenu, Menu, MenuItem};
use ely_gpui_component::motion::{self, Pulse};
use ely_gpui_component::primitives::{Backdrop, Icon, IconName, Place};
use ely_gpui_component::shell::drag_region;
use ely_gpui_component::theme::{ActiveTheme, ControlSize, Elevation, IconSize, TextSize};
use ely_gpui_component::typography::{Ellipsis, MiddleEllipsis};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Div, ElementId, Font, FontWeight, IntoElement,
    ParentElement, Pixels, SharedString, Styled, TextRun, Window, div, font, prelude::*, px,
};
use newjen_core::Status;
use newjen_core::transcript::STOPPED_ON_QUIT;

use crate::format::{elapsed, relative_time, short_path};

/// Below this window width the list hides, so the chat keeps the whole window (screen 11). ⌘B
/// then opens it as a drawer over the chat.
pub const NARROW_WINDOW: f32 = 760.0;

/// The detail line of a thread with no message yet.
pub const DRAFT_DETAIL: &str = "No messages yet";

/// A thread waiting for an approval, in its row ("Needs approval · Bash") and in the title
/// bar's pill on the same screen.
pub const NEEDS_APPROVAL: &str = "Needs approval";

/// What the list says when it has no thread yet (screen 12, "Empty").
pub const EMPTY_LIST: &str = "No threads yet. Your first message starts one.";

/// A click on a row or a button: the chat view's handler.
pub type OnPress = Rc<dyn Fn(&mut Window, &mut App)>;

// -------------------------------------------------------------------------------------------
// Decisions
// -------------------------------------------------------------------------------------------

/// The dot before a thread's title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    /// No dot: nothing to look at.
    None,
    /// Amber: an approval is waiting for you.
    Approval,
    /// Blue, with a slow pulse: Claude is working.
    Working,
    /// Red: the last turn failed.
    Failed,
    /// A hollow ring: the turn was stopped.
    Stopped,
    /// Solid, in the text color: a finished turn you have not looked at yet.
    Unread,
}

/// How a thread's row looks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowLook {
    pub dot: Dot,
    /// The line under the title, e.g. "Working · 1m 12s".
    pub detail: String,
    /// The title in the semibold weight: something happened there that you have not seen.
    pub strong: bool,
}

/// What a row needs to know about its thread. The chat view reads it from the thread.
#[derive(Clone, Debug, PartialEq)]
pub struct RowFacts {
    pub status: Status,
    /// No message sent yet.
    pub draft: bool,
    pub unread: bool,
    /// The tool waiting for an approval, named as its card names it ("Bash").
    pub waiting_tool: Option<String>,
    /// How long the running turn has been going.
    pub working_for: Option<Duration>,
    /// Why the last turn failed: the title of its error ("Claude exited").
    pub failure: Option<String>,
    /// The thread's last row is the "Stopped when Newjen quit" notice.
    pub stopped_on_quit: bool,
    /// The thread's last activity (Unix milliseconds).
    pub updated_at: i64,
}

/// A thread's dot and detail line at `now_ms`. The most urgent state wins: an approval, then
/// working, failed, stopped, a finished turn not seen yet, and last a quiet "4m ago".
pub fn row_look(facts: &RowFacts, now_ms: i64) -> RowLook {
    // "Needs approval · Bash", or just "Needs approval" when the part after the dot is unknown.
    let with = |head: &str, tail: Option<String>| match tail.filter(|tail| !tail.is_empty()) {
        Some(tail) => format!("{head} · {tail}"),
        None => head.to_string(),
    };
    if facts.draft {
        return RowLook {
            dot: Dot::None,
            detail: DRAFT_DETAIL.to_string(),
            strong: false,
        };
    }
    let ago = relative_time(facts.updated_at, now_ms);
    let (dot, detail) = match facts.status {
        Status::Waiting => (
            Dot::Approval,
            with(NEEDS_APPROVAL, facts.waiting_tool.clone()),
        ),
        Status::Working => (
            Dot::Working,
            with(
                "Working",
                facts.working_for.map(|since| elapsed(since.as_secs())),
            ),
        ),
        Status::Failed => (Dot::Failed, with("Failed", facts.failure.clone())),
        Status::Stopped if facts.stopped_on_quit => (Dot::Stopped, STOPPED_ON_QUIT.to_string()),
        Status::Stopped => (Dot::Stopped, "Stopped".to_string()),
        Status::Done | Status::Idle if facts.unread => (Dot::Unread, format!("Done · {ago}")),
        Status::Done | Status::Idle => (Dot::None, ago),
    };
    RowLook {
        dot,
        detail,
        strong: facts.unread,
    }
}

/// The short reason after "Failed · ": the error's title ("Claude exited"), except for an
/// error Claude reported itself, whose title ("Claude stopped") says nothing about why; then
/// the first line of what it said ("Not logged in · Please run /login").
pub fn failure_reason(title: &str, detail: &str) -> String {
    const SAYS_NOTHING: &str = "Claude stopped";
    let first_line = detail.lines().map(str::trim).find(|line| !line.is_empty());
    match first_line {
        Some(line) if title == SAYS_NOTHING => line.to_string(),
        _ => title.to_string(),
    }
}

/// Where the thread list is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A column beside the chat.
    Docked,
    /// Floating over the chat from the left (narrow windows, after ⌘B).
    Drawer,
    /// Out of sight; the title bar shows a button that brings it back.
    Hidden,
}

/// Where the list is at `window_width`: docked in a wide window unless the user hid it,
/// hidden in a narrow one unless the user opened the drawer.
pub fn placement(window_width: f32, hidden_by_user: bool, drawer_open: bool) -> Placement {
    if window_width >= NARROW_WINDOW {
        if hidden_by_user {
            Placement::Hidden
        } else {
            Placement::Docked
        }
    } else if drawer_open {
        Placement::Drawer
    } else {
        Placement::Hidden
    }
}

/// ⌘B or a panel-left button: the new `(hidden_by_user, drawer_open)`. In a wide window it
/// shows or hides the docked list; in a narrow one it opens or closes the drawer, and the
/// choice for wide windows stays as it was.
pub fn toggled(window_width: f32, hidden_by_user: bool, drawer_open: bool) -> (bool, bool) {
    if window_width >= NARROW_WINDOW {
        (!hidden_by_user, false)
    } else {
        (hidden_by_user, !drawer_open)
    }
}

/// The row ⌘⇧] (`forward`) or ⌘⇧[ shows after row `active` of `len`: the next or previous one,
/// wrapping around at the ends like a Mac app's tabs. `None` when there is nowhere to go.
pub fn step(active: usize, len: usize, forward: bool) -> Option<usize> {
    if len < 2 || active >= len {
        return None;
    }
    Some(if forward {
        (active + 1) % len
    } else {
        (active + len - 1) % len
    })
}

/// ⌘1 to ⌘9: the row at `place` (from 0), if the list is that long.
pub fn jump(place: usize, len: usize) -> Option<usize> {
    (place < len).then_some(place)
}

/// Which row to show after the one on screen, at `removed`, leaves a list that had `len` rows:
/// the row that moves up into its place, else the one above it. `None` when it was the only
/// one. The answer counts rows after the removal.
pub fn after_removal(removed: usize, len: usize) -> Option<usize> {
    if len <= 1 {
        None
    } else if removed + 1 < len {
        Some(removed)
    } else {
        Some(removed - 1)
    }
}

/// Where row `ix` is after row `removed` (a different one) leaves the list.
pub fn shifted(ix: usize, removed: usize) -> usize {
    if removed < ix { ix - 1 } else { ix }
}

/// Whether the list counts as empty: nothing in it but one draft, the thread on screen. Then
/// it shows [`EMPTY_LIST`] under a highlighted "New thread" row rather than a lone "New
/// thread" row (screen 12, "Empty"). A draft kept beside saved threads is a row of its own.
pub fn is_empty_list(drafts: &[bool]) -> bool {
    drafts.len() <= 1 && drafts.iter().all(|draft| *draft)
}

/// Whether the title bar's panel-left button gets an amber dot: the list is out of sight and
/// a thread other than the one on screen waits for an approval (screen 11).
pub fn hidden_needs_you(statuses: &[Status], active: usize) -> bool {
    statuses
        .iter()
        .enumerate()
        .any(|(ix, status)| ix != active && *status == Status::Waiting)
}

/// The gap between the project's name and its path (`gap_2`).
const NAME_GAP: f32 = 8.0;
/// A path shorter than this many characters says nothing useful, so it is left out.
const PATH_MIN_CHARS: usize = 6;

/// The project row's path after a name `name_width` pixels wide, in `room` pixels: its `~/path`
/// cut in the middle to what fits (at `path_char` pixels a character), or none when the name
/// takes the room (screen 12). The name is never cut for the path's sake.
pub fn project_path(path: &str, name_width: f32, path_char: f32, room: f32) -> Option<String> {
    let left = room - name_width - NAME_GAP;
    let path_chars = (left / path_char.max(1.0)).max(0.0) as usize;
    (path_chars >= PATH_MIN_CHARS).then(|| short_path(path, path_chars))
}

/// How wide `text` is in `font` at `size`, as the window's text system lays it out, with the
/// fallback fonts it picks for characters the font lacks (Chinese or Japanese names, emoji).
fn measured(text: &str, font: Font, size: Pixels, window: &Window) -> f32 {
    let run = TextRun {
        len: text.len(),
        font,
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line =
        window
            .text_system()
            .shape_line(SharedString::from(text.to_string()), size, &[run], None);
    f32::from(line.width)
}

// -------------------------------------------------------------------------------------------
// Elements
// -------------------------------------------------------------------------------------------

/// The size of a row's dot, and of the box that keeps every title lined up whether or not it
/// has a dot.
const DOT: f32 = 7.0;
const DOT_BOX: f32 = 10.0;

/// The dot before a title. `id` keys the working pulse.
pub fn dot(dot: Dot, id: ElementId, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let solid = |color| div().size(px(DOT)).rounded_full().bg(color);
    let mark = match dot {
        Dot::None => None,
        Dot::Approval => Some(solid(colors.warning).into_any_element()),
        Dot::Failed => Some(solid(colors.danger).into_any_element()),
        Dot::Unread => Some(solid(colors.fg).into_any_element()),
        Dot::Stopped => Some(
            div()
                .size(px(DOT))
                .rounded_full()
                .border_1()
                .border_color(colors.fg_subtle)
                .into_any_element(),
        ),
        // Ely's pulse: a thin ring that widens and fades, and none under reduced motion.
        Dot::Working => Some(
            Pulse::new(id)
                .size(px(DOT))
                .child(solid(colors.focus))
                .into_any_element(),
        ),
    };
    div()
        .size(px(DOT_BOX))
        .flex()
        .items_center()
        .justify_center()
        .children(mark)
        .into_any_element()
}

/// The top of the docked list, under the traffic lights: room for them (unless the window is
/// full screen, where they hide), then the button that hides the list. Drags the window like
/// a title bar.
pub fn header(on_toggle: OnPress, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let inset = theme.traffic_light_inset();
    let height = theme.titlebar_height();
    let fullscreen = window.is_fullscreen();
    drag_region("sidebar-header", window, cx)
        .flex_none()
        .h(height)
        .flex()
        .items_center()
        .justify_end()
        .when(!fullscreen, |header| header.pl(inset))
        .pr_2()
        .child(toggle_button("sidebar-hide", "Hide sidebar", on_toggle))
        .into_any_element()
}

/// The panel-left button that hides or shows the list (⌘B).
pub fn toggle_button(id: &'static str, tooltip: &'static str, on_toggle: OnPress) -> IconButton {
    IconButton::new(id, IconName::PanelLeft)
        .size(ControlSize::Sm)
        .tooltip(tooltip)
        .on_click(move |_, window, cx| on_toggle(window, cx))
}

/// The project: a folder icon, its name and, when there is room after it, its `~/path`.
/// `room` is the width its text has.
pub fn project_row(name: &str, path: &str, room: f32, window: &Window, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let rem = window.rem_size();
    let name_size = theme.text_size(TextSize::Base);
    let path_size = theme.text_size(TextSize::Xs);
    let mut name_font = font(theme.font_family.clone());
    name_font.weight = FontWeight::SEMIBOLD;
    let name_width = measured(name, name_font, name_size.to_pixels(rem), window);
    let path_char = measured(
        "0",
        font(theme.mono_family.clone()),
        path_size.to_pixels(rem),
        window,
    );
    let path = project_path(path, name_width, path_char, room);
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_2p5()
        .pt_2()
        .pb_2p5()
        .child(
            Icon::new(IconName::Folder)
                .size(IconSize::Sm)
                .color(theme.colors.fg_muted),
        )
        .child(
            // The name keeps its width; only a name wider than the whole row is cut.
            div()
                .flex_none()
                .max_w(px(room.max(0.0)))
                .text_size(name_size)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.colors.fg)
                .child(Ellipsis::new(name.to_string())),
        )
        .children(path.map(|path| {
            // The path gives way instead: if its characters are wider than estimated (say
            // Japanese folder names), the row cuts it further in its middle.
            div()
                .min_w_0()
                .font_family(theme.mono_family.clone())
                .text_size(path_size)
                .text_color(theme.colors.fg_subtle)
                .child(MiddleEllipsis::new(path))
        }))
        .into_any_element()
}

/// The thread list's column, `header` over `list` on the sunken color of the mock. Docked
/// beside the chat, and the same column as the drawer over it in narrow windows. A 240 px
/// column like Ely's `Sidebar`, which cannot be used as it is: it folds to an icon rail with an
/// animated width, and this list hides at once (⌘B).
pub fn column(header: AnyElement, list: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    let colors = &theme.colors;
    v_stack()
        .flex_none()
        .w(theme.sidebar_width(false))
        .h_full()
        .bg(colors.sunken)
        .border_r_1()
        .border_color(colors.border)
        .child(header)
        .child(
            v_stack()
                .flex_1()
                .min_h_0()
                .px_2()
                .pb_2()
                .gap_0p5()
                .children(list),
        )
}

/// The list as a drawer over the chat in narrow windows: the docked `column`, sliding in from
/// the left over Ely's scrim exactly as Ely's `Sheet` slides (same time and curve, none under
/// Reduce Motion). A click on the scrim calls `on_close`. Not Ely's `Sheet` itself: its
/// "Threads" heading sits at the top where the window's traffic lights are, with no room kept
/// for them, while this column's header keeps that room as the docked list does.
pub fn drawer(column: Div, on_close: OnPress, window: &Window, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let travel = theme.sidebar_width(false).to_pixels(window.rem_size());
    let slide =
        Animation::new(motion::duration(motion::BASE, cx)).with_easing(motion::ease_out_cubic);
    let panel = column
        .shadow(theme.elevation(Elevation::Modal))
        .with_animation("drawer-in", slide, move |panel, t| {
            panel.ml(-(travel * (1.0 - t)))
        });
    Backdrop::new("threads-drawer")
        .place(Place::Left)
        .on_dismiss(move |window, cx| on_close(window, cx))
        .child(panel)
        .into_any_element()
}

/// "+ New thread ⌘N", in the quiet color. Highlighted (in the text color) while the empty
/// list's draft is on screen.
pub fn new_thread_row(selected: bool, on_press: OnPress, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let ink = if selected { colors.fg } else { colors.fg_muted };
    ListItem::new("new-thread", "New thread")
        .quiet(!selected)
        .leading(Icon::new(IconName::Plus).size(IconSize::Xs).color(ink))
        .trailing("⌘N")
        .selected(selected)
        .on_click(move |_, window, cx| on_press(window, cx))
        .into_any_element()
}

/// "No threads yet. Your first message starts one."
pub fn empty_caption(cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .px_2p5()
        .py_2()
        .text_size(theme.text_size(TextSize::Sm))
        .text_color(theme.colors.fg_subtle)
        .child(EMPTY_LIST)
        .into_any_element()
}

/// What one thread row shows.
pub struct Row {
    /// The thread's id, which keys the row's state (its menu, its pulse).
    pub id: SharedString,
    pub title: SharedString,
    pub look: RowLook,
    /// The thread on screen.
    pub selected: bool,
}

/// One thread: its dot, its title (cut with "…") and its detail line. A click shows it; a
/// right click offers "Archive thread".
pub fn thread_row(row: Row, on_press: OnPress, on_archive: OnPress, cx: &App) -> AnyElement {
    let lead = dot(
        row.look.dot,
        (ElementId::from("pulse"), row.id.clone()).into(),
        cx,
    );
    // Ely's `detail` line is in the subtle color, as the mock's detail line is (its
    // `description` line is brighter, close to the title).
    let item = ListItem::new((ElementId::from("thread-row"), row.id.clone()), row.title)
        .detail(row.look.detail)
        .leading(lead)
        .strong(row.look.strong)
        .selected(row.selected)
        .on_click(move |_, window, cx| on_press(window, cx));
    let menu = Menu::new().item(
        MenuItem::new("Archive thread")
            .icon(IconName::Archive)
            .on_click(move |window, cx| on_archive(window, cx)),
    );
    ContextMenu::new((ElementId::from("thread-menu"), row.id), menu)
        .w_full()
        .child(item)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000_000_000;

    fn facts(status: Status) -> RowFacts {
        RowFacts {
            status,
            draft: false,
            unread: false,
            waiting_tool: None,
            working_for: None,
            failure: None,
            stopped_on_quit: false,
            updated_at: NOW - 4 * 60 * 1000,
        }
    }

    #[test]
    fn each_status_gets_its_dot_and_line() {
        let mut waiting = facts(Status::Waiting);
        waiting.waiting_tool = Some("Bash".to_string());
        let look = row_look(&waiting, NOW);
        assert_eq!(
            (look.dot, look.detail.as_str()),
            (Dot::Approval, "Needs approval · Bash")
        );

        let mut working = facts(Status::Working);
        working.working_for = Some(Duration::from_secs(72));
        let look = row_look(&working, NOW);
        assert_eq!(
            (look.dot, look.detail.as_str()),
            (Dot::Working, "Working · 1m 12s")
        );

        let mut failed = facts(Status::Failed);
        failed.failure = Some("Claude exited".to_string());
        let look = row_look(&failed, NOW);
        assert_eq!(
            (look.dot, look.detail.as_str()),
            (Dot::Failed, "Failed · Claude exited")
        );

        let mut quit = facts(Status::Stopped);
        quit.stopped_on_quit = true;
        let look = row_look(&quit, NOW);
        assert_eq!(
            (look.dot, look.detail.as_str()),
            (Dot::Stopped, "Stopped when Newjen quit")
        );
        let look = row_look(&facts(Status::Stopped), NOW);
        assert_eq!((look.dot, look.detail.as_str()), (Dot::Stopped, "Stopped"));

        let look = row_look(&facts(Status::Done), NOW);
        assert_eq!((look.dot, look.detail.as_str()), (Dot::None, "4m ago"));
        assert!(!look.strong);
    }

    #[test]
    fn a_failure_says_why_in_a_few_words() {
        assert_eq!(
            failure_reason("Claude exited", "The claude process exited with code 1."),
            "Claude exited"
        );
        assert_eq!(
            failure_reason(
                "Claude stopped",
                "\nNot logged in · Please run /login\nmore"
            ),
            "Not logged in · Please run /login"
        );
        assert_eq!(failure_reason("Claude stopped", ""), "Claude stopped");
    }

    #[test]
    fn an_unseen_finished_turn_is_a_solid_dot_and_a_strong_title() {
        let mut done = facts(Status::Done);
        done.unread = true;
        let look = row_look(&done, NOW);
        assert_eq!(look.dot, Dot::Unread);
        assert_eq!(look.detail, "Done · 4m ago");
        assert!(look.strong);
    }

    #[test]
    fn missing_details_leave_just_the_state() {
        assert_eq!(
            row_look(&facts(Status::Waiting), NOW).detail,
            "Needs approval"
        );
        assert_eq!(row_look(&facts(Status::Working), NOW).detail, "Working");
        let mut failed = facts(Status::Failed);
        failed.failure = Some(String::new());
        assert_eq!(row_look(&failed, NOW).detail, "Failed");
    }

    #[test]
    fn a_draft_says_no_messages_yet() {
        let mut draft = facts(Status::Idle);
        draft.draft = true;
        draft.unread = true;
        let look = row_look(&draft, NOW);
        assert_eq!(
            look,
            RowLook {
                dot: Dot::None,
                detail: DRAFT_DETAIL.to_string(),
                strong: false,
            }
        );
    }

    #[test]
    fn the_list_hides_below_760_and_toggles_into_a_drawer() {
        assert_eq!(placement(1200.0, false, false), Placement::Docked);
        assert_eq!(placement(760.0, false, false), Placement::Docked);
        assert_eq!(placement(1200.0, true, false), Placement::Hidden);
        // Wide: the drawer never shows, even if it was left open in a narrow window.
        assert_eq!(placement(1200.0, false, true), Placement::Docked);
        assert_eq!(placement(759.0, false, false), Placement::Hidden);
        assert_eq!(placement(640.0, false, true), Placement::Drawer);
        // The user's choice for wide windows does not keep the drawer shut.
        assert_eq!(placement(640.0, true, true), Placement::Drawer);

        // ⌘B in a wide window hides or shows the column.
        assert_eq!(toggled(1200.0, false, false), (true, false));
        assert_eq!(toggled(1200.0, true, false), (false, false));
        // ⌘B in a narrow one opens or closes the drawer and keeps the wide choice.
        assert_eq!(toggled(640.0, false, false), (false, true));
        assert_eq!(toggled(640.0, true, true), (true, false));
    }

    #[test]
    fn previous_and_next_wrap_around() {
        assert_eq!(step(0, 3, true), Some(1));
        assert_eq!(step(2, 3, true), Some(0));
        assert_eq!(step(0, 3, false), Some(2));
        assert_eq!(step(1, 3, false), Some(0));
        assert_eq!(step(0, 1, true), None);
        assert_eq!(step(0, 0, false), None);
        assert_eq!(step(5, 3, true), None);
    }

    #[test]
    fn jumps_go_only_to_rows_that_exist() {
        assert_eq!(jump(0, 3), Some(0));
        assert_eq!(jump(2, 3), Some(2));
        assert_eq!(jump(3, 3), None);
        assert_eq!(jump(8, 0), None);
    }

    #[test]
    fn after_a_removal_the_next_row_moves_into_place() {
        // [a, B, c] without B: c moved up into B's place.
        assert_eq!(after_removal(1, 3), Some(1));
        // [a, b, C] without C: b, the one above.
        assert_eq!(after_removal(2, 3), Some(1));
        assert_eq!(after_removal(0, 2), Some(0));
        assert_eq!(after_removal(0, 1), None);
        // Other rows move up only when the removed one was above them.
        assert_eq!(shifted(3, 1), 2);
        assert_eq!(shifted(1, 3), 1);
    }

    #[test]
    fn only_a_lone_draft_counts_as_an_empty_list() {
        assert!(is_empty_list(&[]));
        assert!(is_empty_list(&[true]));
        assert!(!is_empty_list(&[false]));
        // A draft beside a saved thread, or two drafts, are rows of their own.
        assert!(!is_empty_list(&[true, false]));
        assert!(!is_empty_list(&[true, true]));
    }

    #[test]
    fn the_hidden_list_calls_for_you_only_for_other_threads() {
        let statuses = [Status::Done, Status::Waiting, Status::Working];
        assert!(hidden_needs_you(&statuses, 0));
        // The waiting thread is the one on screen: its dock already asks.
        assert!(!hidden_needs_you(&statuses, 1));
        assert!(!hidden_needs_you(&[Status::Working, Status::Failed], 0));
    }

    #[test]
    fn a_long_project_name_takes_the_row_from_its_path() {
        // The docked list's row: 182 px for the text, 6.6 px a mono character.
        let (room, char) = (182.0, 6.6);
        assert_eq!(
            project_path("~/Newjen", 50.0, char, room).as_deref(),
            Some("~/Newjen")
        );
        let path = project_path("~/Developer/clients/acme/platform/app", 22.0, char, room)
            .expect("room for a short path");
        assert!(
            path.ends_with("/platform/app") || path.ends_with("/app"),
            "{path}"
        );
        assert!(
            path.chars().count() as f32 * char <= room - 22.0 - NAME_GAP,
            "{path}"
        );
        // A name as wide as the row leaves no path, whatever the path.
        assert_eq!(project_path("~/x", room, char, room), None);
        // What decides is the name's measured width, not its number of characters: a
        // ten-character Japanese name drawn 130 px wide leaves room for a stub only.
        assert_eq!(
            project_path("~/日本語のプロジェクト", 150.0, char, room),
            None
        );
        let stub = project_path("~/日本語のプロジェクト", 130.0, char, room);
        assert!(stub.is_some_and(|stub| stub.chars().count() <= 6));
    }
}
