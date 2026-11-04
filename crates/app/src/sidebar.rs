//! The thread list on the left: the project switcher, Pinned and the Queue of what waits on you,
//! the read list under them, a dock with In flight, Snoozed and Settled, and the usage footer.
//!
//! The decisions come first as plain functions, tested without a window: where each thread
//! shows and in what order, what a row says, where the list sits at each window width, which
//! thread ⌘J goes to. The elements that draw them come after; the chat view wires them to the
//! threads.

mod dock;
mod motion;

pub use dock::{
    Clock, Confirm, Dock, Flight, Fold, Panel, PanelGroup, PanelRow, SheetKind, TRAY_ROWS,
    TrayItem, TrayRow, dock,
};
pub use motion::{
    Counted, Counter, Frame, HEADER_HEIGHT, Key, Lane, Look, Motion, Sight, Zone, slot,
};

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, FontWeight, IntoElement, MouseButton,
    ParentElement, Pixels, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, list::ListItem};
use newjen_core::Status;
use newjen_core::calendar::SnoozeUntil;
use newjen_core::steps::Placement;
use newjen_core::store::ListState;
use newjen_core::transcript::STOPPED_ON_QUIT;

use crate::format::{elapsed, relative_time};
use crate::theme::SIDEBAR;
use crate::ui::{self, IconName};

/// Below this window width the list hides, so the chat keeps the whole window (screen 11). ⌘B
/// then opens it as a drawer over the chat.
pub const NARROW_WINDOW: f32 = 760.0;

/// The detail line of a thread with no message yet.
pub const DRAFT_DETAIL: &str = "No messages yet";

/// What a draft's second line says in the list.
pub const DRAFT_LINE: &str = "Draft";

/// A thread waiting for an approval, in its row ("Needs approval · Bash") and in the title
/// bar's pill on the same screen.
pub const NEEDS_APPROVAL: &str = "Needs approval";

pub const MAKING_COPY_DETAIL: &str = "Creating a worktree…";

/// What the game's title says while threads work in the tray.
pub const NOTHING_NEEDS_YOU: &str = "Nothing needs you";

/// What the game's title says when nothing is in flight either.
pub const ALL_CLEAR: &str = "All clear";

/// How far the game sits above the foot of the list, which is the top of the dock.
pub const GAME_LIFT: Pixels = px(30.);

/// Secondary text on the glass: a row's detail line.
pub fn muted(cx: &App) -> gpui::Hsla {
    crate::theme::material(cx).glass_muted.opacity(0.62)
}

/// The quietest text on the glass: headers, counts and times.
pub fn faint(cx: &App) -> gpui::Hsla {
    crate::theme::material(cx).glass_muted.opacity(0.42)
}

/// A click on a row or a button: the chat view's handler.
pub type OnPress = Rc<dyn Fn(&mut Window, &mut App)>;

/// A choice of when a snoozed thread wakes: the chat view's handler.
pub type OnSnooze = Rc<dyn Fn(SnoozeUntil, &mut Window, &mut App)>;

/// A click on the Snoozed or Settled row: the chat view's handler.
pub type OnOpen = Rc<dyn Fn(SheetKind, &mut Window, &mut App)>;

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
    /// Blue, with a slow pulse: Claude is working, or a command it started still runs in the
    /// background.
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
    pub copy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    Project,
    Worktree,
    MakingWorktree,
}

/// What a row needs to know about its thread. The chat view reads it from the thread.
#[derive(Clone, Debug, PartialEq)]
pub struct RowFacts {
    pub status: Status,
    /// No message sent yet.
    pub draft: bool,
    pub folder: Folder,
    pub unread: bool,
    /// The tool waiting for an approval, named as its card names it ("Bash").
    pub waiting_tool: Option<String>,
    /// How long the running turn has been going.
    pub working_for: Option<Duration>,
    /// Why the last turn failed: the title of its error ("Claude exited").
    pub failure: Option<String>,
    /// The thread's last row is the "Stopped when Newjen quit" notice.
    pub stopped_on_quit: bool,
    /// How many commands Claude keeps running in the background (a dev server, say).
    pub background_tasks: usize,
    /// The thread's last activity (Unix milliseconds).
    pub updated_at: i64,
}

/// "Background task running", or "3 background tasks running": a thread whose turn is over
/// while commands Claude started keep running, in its row and its title bar.
pub fn background_detail(count: usize) -> String {
    match count {
        1 => "Background task running".to_string(),
        count => format!("{count} background tasks running"),
    }
}

/// A thread's dot and detail line at `now_ms`. The most urgent state wins: an approval, then
/// working, a command still running in the background once the turn is over (however it
/// ended), failed, stopped, a finished turn not seen yet, and last a quiet "4m ago".
pub fn row_look(facts: &RowFacts, now_ms: i64) -> RowLook {
    // "Needs approval · Bash", or just "Needs approval" when the part after the dot is unknown.
    let with = |head: &str, tail: Option<String>| match tail.filter(|tail| !tail.is_empty()) {
        Some(tail) => format!("{head} · {tail}"),
        None => head.to_string(),
    };
    let copy = facts.folder != Folder::Project;
    if facts.draft {
        return RowLook {
            dot: Dot::None,
            detail: DRAFT_DETAIL.to_string(),
            strong: false,
            copy,
        };
    }
    if facts.folder == Folder::MakingWorktree {
        return RowLook {
            dot: Dot::Working,
            detail: MAKING_COPY_DETAIL.to_string(),
            strong: false,
            copy,
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
        _ if facts.background_tasks > 0 => {
            (Dot::Working, background_detail(facts.background_tasks))
        }
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
        copy,
    }
}

/// A count in a header: "7", or "99+" from 100 up.
pub fn count_label(count: usize) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

/// What the list needs to know about each thread, in the view's thread order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Listed {
    pub draft: bool,
    /// Where the thread shows: its own placement, or the one it is held in while on screen
    /// (see [`held`]).
    pub placement: Placement,
    /// An approval or a question waits for an answer.
    pub waits: bool,
    pub list: ListState,
}

/// A section of the scrolling list that has a header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Pinned,
    Queue,
}

/// One line of the scrolling list: a section's header, a thread (its index in the view's thread
/// order), or the gap above the read list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// `needs` counts the rows that wait for an answer.
    Header {
        section: Section,
        count: usize,
        needs: usize,
    },
    Thread(usize),
    Gap,
}

/// The room between the Queue and the read list.
pub const READ_GAP: Pixels = px(14.);

/// Where each thread shows, as indexes into the view's thread order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The scrolling list: drafts, Pinned, Queue, then the read list.
    pub entries: Vec<Entry>,
    /// The In flight tray.
    pub in_flight: Vec<usize>,
    /// Snoozed, the one that wakes first on top.
    pub snoozed: Vec<usize>,
    /// Settled, the latest on top.
    pub settled: Vec<usize>,
}

/// The layout from the threads, in the view's thread order. Drafts come first and have no
/// header. Pinned keeps the order of the pins. The Queue and the read list put the thread that
/// landed last on top, so a row only moves when something lands, never because its status
/// changed. A section has a header only while it has rows, and the read list, which has no
/// header, sits 14px under whatever is above it.
pub fn display_order(threads: &[Listed]) -> Layout {
    let members = |placement: Placement| -> Vec<usize> {
        threads
            .iter()
            .enumerate()
            .filter(|(_, thread)| !thread.draft && thread.placement == placement)
            .map(|(ix, _)| ix)
            .collect()
    };
    let mut entries: Vec<Entry> = threads
        .iter()
        .enumerate()
        .filter(|(_, thread)| thread.draft)
        .map(|(ix, _)| Entry::Thread(ix))
        .collect();

    let mut pinned = members(Placement::Pinned);
    pinned.sort_by_key(|ix| threads[*ix].list.pinned_at);
    let mut queue = members(Placement::Queue);
    queue.sort_by_key(|ix| std::cmp::Reverse(threads[*ix].list.landed_at));
    let mut read = members(Placement::Read);
    read.sort_by_key(|ix| std::cmp::Reverse(threads[*ix].list.landed_at));

    for (section, rows) in [(Section::Pinned, &pinned), (Section::Queue, &queue)] {
        if rows.is_empty() {
            continue;
        }
        entries.push(Entry::Header {
            section,
            count: rows.len(),
            needs: rows.iter().filter(|ix| threads[**ix].waits).count(),
        });
        entries.extend(rows.iter().copied().map(Entry::Thread));
    }
    if !read.is_empty() && !(pinned.is_empty() && queue.is_empty()) {
        entries.push(Entry::Gap);
    }
    entries.extend(read.into_iter().map(Entry::Thread));

    let mut snoozed = members(Placement::Snoozed);
    snoozed.sort_by_key(|ix| threads[*ix].list.snoozed_until);
    let mut settled = members(Placement::Settled);
    settled.sort_by_key(|ix| std::cmp::Reverse(threads[*ix].list.settled_at));
    Layout {
        entries,
        in_flight: members(Placement::InFlight),
        snoozed,
        settled,
    }
}

/// The threads in the order ⌘1 to ⌘9 and ⌘⇧[ ⌘⇧] count through: what the list shows from the
/// top, then the In flight tray. Snoozed and Settled threads are not in it.
pub fn thread_order(layout: &Layout) -> Vec<usize> {
    layout
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Thread(ix) => Some(*ix),
            Entry::Header { .. } | Entry::Gap => None,
        })
        .chain(layout.in_flight.iter().copied())
        .collect()
}

/// Where the thread on screen shows. It stays in the Queue or the read list where it was when
/// it was opened, whatever happens to it there, until the user switches away, so it never moves
/// under them. The user's own moves (pin, snooze, settle) apply at once. A thread opened from
/// the tray that lands while on screen moves to the top of the Queue.
pub fn held(held: Placement, now: Placement) -> Placement {
    use Placement::*;
    match (held, now) {
        (_, Pinned | Snoozed | Settled) => now,
        (Queue | Read, _) => held,
        (InFlight, InFlight) => InFlight,
        (InFlight, _) => Queue,
        (Pinned | Snoozed | Settled, _) => now,
    }
}

/// Where the thread on screen shows: its placement and the landing time its row sorts by. Both
/// stay as they were when the thread was opened, so the row keeps its exact position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub placement: Placement,
    pub landed_at: i64,
}

/// What the thread on screen shows after a render. `previous` is what it showed before, when it
/// was on screen then too. A held place keeps its landing time even when the thread lands
/// again. A place that moves, by the user's own move or because a thread opened from the tray
/// landed, takes the thread's own landing time and so goes to the top.
pub fn hold(previous: Option<Held>, now: Placement, landed_at: i64) -> Held {
    match previous {
        Some(at) if held(at.placement, now) == at.placement => at,
        Some(at) => Held {
            placement: held(at.placement, now),
            landed_at,
        },
        None => Held {
            placement: now,
            landed_at,
        },
    }
}

/// ⌘J: the thread that needs you after `active` in the list's order, wrapping around. `order`
/// is [`thread_order`], and `needs[ix]` says whether thread `ix` is in the "Needs you" group.
/// `None` when nothing else needs you (the thread on screen needs no jump to itself).
pub fn next_needs_you(order: &[usize], active: usize, needs: &[bool]) -> Option<usize> {
    let from = order
        .iter()
        .position(|ix| *ix == active)
        .map_or(0, |at| at + 1);
    (0..order.len())
        .map(|offset| order[(from + offset) % order.len()])
        .find(|ix| *ix != active && needs.get(*ix).copied().unwrap_or(false))
}

/// A row's time on the right: "now", "4m", "1h", "3d", "2w", "11mo", "2y".
pub fn compact_time(then_ms: i64, now_ms: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let secs = (now_ms - then_ms).max(0) / 1000;
    if secs < MINUTE {
        "now".to_string()
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else if secs < 14 * DAY {
        format!("{}d", secs / DAY)
    } else if secs < 60 * DAY {
        format!("{}w", secs / (7 * DAY))
    } else if secs < 365 * DAY {
        format!("{}mo", secs / (30 * DAY))
    } else {
        format!("{}y", secs / (365 * DAY))
    }
}

/// The row's second line: what the thread is doing (core's activity line), else the row look's
/// own words. A finished thread's look only says how long ago, which the row's time already
/// does, so it says "Done".
pub fn detail_line(activity: Option<String>, look: &RowLook, draft: bool) -> String {
    if draft {
        return DRAFT_LINE.to_string();
    }
    if let Some(line) = activity.filter(|line| !line.trim().is_empty()) {
        return line;
    }
    match look.dot {
        Dot::None | Dot::Unread => "Done".to_string(),
        _ => look.detail.clone(),
    }
}

/// How the thread list is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    /// A column beside the chat.
    Docked,
    /// Floating over the chat from the left (narrow windows, after ⌘B).
    Drawer,
    /// Out of sight; the title bar shows a button that brings it back.
    Hidden,
}

/// How the list is shown at `window_width`: docked in a wide window unless the user hid it,
/// hidden in a narrow one unless the user opened the drawer.
pub fn mode(window_width: f32, hidden_by_user: bool, drawer_open: bool) -> SidebarMode {
    if window_width >= NARROW_WINDOW {
        if hidden_by_user {
            SidebarMode::Hidden
        } else {
            SidebarMode::Docked
        }
    } else if drawer_open {
        SidebarMode::Drawer
    } else {
        SidebarMode::Hidden
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

/// Whether the spirit fox game fills the list: no thread in Pinned, the Queue or the read
/// list. Drafts do not count, so a lone "New thread" draft row still has the game below it, and
/// neither do the In flight tray, Snoozed or Settled.
pub fn shows_game(threads: &[Listed]) -> bool {
    threads.iter().all(|thread| {
        thread.draft
            || matches!(
                thread.placement,
                Placement::InFlight | Placement::Snoozed | Placement::Settled
            )
    })
}

/// The game's title: that nothing needs you while a thread works in the tray, else all clear.
pub fn game_title(threads: &[Listed]) -> &'static str {
    if threads
        .iter()
        .any(|thread| !thread.draft && thread.placement == Placement::InFlight)
    {
        NOTHING_NEEDS_YOU
    } else {
        ALL_CLEAR
    }
}

/// What the panel-left button's dot says while the list is out of sight. Amber, for a thread
/// that needs you, wins over blue, for an update that is ready to install.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListBadge {
    pub attention: bool,
    pub update: bool,
}

/// Whether the title bar's panel-left button gets an amber dot: the list is out of sight and
/// a thread other than the one on screen waits for an approval (screen 11).
pub fn hidden_needs_you(statuses: &[Status], active: usize) -> bool {
    statuses
        .iter()
        .enumerate()
        .any(|(ix, status)| ix != active && *status == Status::Waiting)
}

// -------------------------------------------------------------------------------------------
// Elements
// -------------------------------------------------------------------------------------------

/// The narrowest a row's menu gets.
const MENU_WIDTH: Pixels = px(200.);

/// A title longer than this may be cut in the list, so it gets a tooltip with the whole of it.
const TITLE_TOOLTIP_CHARS: usize = 24;

/// The top of the docked list, under the traffic lights: room for them (unless the window is
/// full screen, where they hide), then the button that hides the list. Drags the window like
/// a title bar.
pub fn header(on_toggle: OnPress, window: &mut Window, cx: &mut App) -> AnyElement {
    let _ = window;
    ui::glass_title_bar(cx)
        .child(toggle_button("sidebar-hide", "Hide sidebar", on_toggle, cx))
        .into_any_element()
}

/// The panel-left button that hides or shows the list (⌘B).
pub fn toggle_button(
    id: &'static str,
    tooltip: &'static str,
    on_toggle: OnPress,
    cx: &App,
) -> AnyElement {
    ui::icon_button(id, IconName::PanelLeft, tooltip, cx)
        .debug_selector(|| "sidebar-toggle".to_string())
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .into_any_element()
}

/// The project row's callbacks and state.
pub struct ProjectBar {
    pub name: SharedString,
    /// Opens the project chooser; `None` when no chooser is wired.
    pub on_switch: Option<OnPress>,
    pub on_new_thread: OnPress,
}

/// The project menu: the chooser.
fn project_menu(menu: PopupMenu, chooser: &Option<OnPress>) -> PopupMenu {
    menu.min_w(MENU_WIDTH)
        .when_some(chooser.clone(), |menu, chooser| {
            menu.item(
                PopupMenuItem::new("Switch project…")
                    .on_click(move |_, window, cx| chooser(window, cx)),
            )
        })
}

/// The project switcher and the new-thread button. A click on the project's name opens the
/// chooser; the chevron beside it, and a right click on the name, open the project menu.
pub fn project_bar(bar: ProjectBar, cx: &App) -> AnyElement {
    let material = crate::theme::material(cx);
    let ProjectBar {
        name,
        on_switch,
        on_new_thread,
    } = bar;
    let chooser = on_switch.clone();
    let menu_chooser = on_switch.clone();
    let switcher = div()
        .id("switch-project")
        .debug_selector(|| "switch-project".to_string())
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .px_1p5()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .hover(move |row| row.bg(material.glass_hover))
        .child(ui::monogram(&name, cx))
        .child(
            ui::ellipsis(name)
                .flex_1()
                .font_weight(FontWeight::SEMIBOLD)
                .text_base(),
        )
        .when_some(on_switch, |row, chooser| {
            row.on_click(move |_, window, cx| chooser(window, cx))
        })
        .context_menu(move |menu, _, _| project_menu(menu, &chooser));
    let chevron = ui::icon_button("project-menu", IconName::ChevronsUpDown, "Project menu", cx)
        .debug_selector(|| "project-menu".to_string())
        .dropdown_menu(move |menu, _, _| project_menu(menu, &menu_chooser));
    h_flex()
        .flex_none()
        .items_center()
        .gap_1()
        .px_2()
        .pt_1()
        .pb_2()
        .child(switcher)
        .child(chevron)
        .child(
            ui::icon_button("new-thread", IconName::SquarePen, "New thread (⌘N)", cx)
                .debug_selector(|| "new-thread".to_string())
                .on_click(move |_, window, cx| on_new_thread(window, cx)),
        )
        .into_any_element()
}

/// A quiet line above the usage row saying why desktop notifications are off, with a button
/// that hides it.
pub fn attention_note(note: &'static str, on_dismiss: OnPress, cx: &App) -> AnyElement {
    h_flex()
        .id("attention-note")
        .debug_selector(|| "attention-note".to_string())
        .flex_none()
        .items_start()
        .gap_1()
        .pl_2p5()
        .pr_1()
        .py_1p5()
        .text_xs()
        .text_color(crate::theme::material(cx).glass_muted)
        .child(div().flex_1().min_w_0().child(note))
        .child(
            ui::icon_button("dismiss-attention-note", IconName::Close, "Dismiss", cx)
                .debug_selector(|| "dismiss-attention-note".to_string())
                .on_click(move |_, window, cx| on_dismiss(window, cx)),
        )
        .into_any_element()
}

/// The panel of rows under the project bar: scrolls, and `scroll` brings a row into view. The
/// rows are its first children (the scrollbar comes last), so a row's place is its index.
pub fn list_panel(
    scroll: &gpui::ScrollHandle,
    entries: Vec<AnyElement>,
) -> gpui::Stateful<gpui::Div> {
    use gpui_kit::component::scroll::ScrollableElement as _;
    div()
        .id("threads")
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .overflow_y_scroll()
        .track_scroll(scroll)
        .children(entries)
        .vertical_scrollbar(scroll)
}

/// The column: the header under the traffic lights, then the list and the footer.
pub fn column(header: AnyElement, list: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    v_flex()
        .flex_none()
        .w(SIDEBAR)
        .h_full()
        .text_color(theme.sidebar_foreground)
        .child(header)
        .child(v_flex().flex_1().min_h_0().px_2().children(list))
}

/// The scrolling list with the game anchored over its foot, just above the dock.
pub fn list_area(list: impl IntoElement, game: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(list)
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom(GAME_LIFT)
                .child(game),
        )
}

/// Which edge the old value of a crossfading count lines up with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
}

/// A count. While it changes, the old value fades out under the new one fading in.
pub fn number(count: Counted, edge: Edge) -> AnyElement {
    let Some(was) = count.was else {
        return div().child(count_label(count.value)).into_any_element();
    };
    let old = div()
        .absolute()
        .top_0()
        .opacity(1. - count.mix)
        .child(count_label(was));
    let old = match edge {
        Edge::Left => old.left_0(),
        Edge::Right => old.right_0(),
    };
    div()
        .relative()
        .child(div().opacity(count.mix).child(count_label(count.value)))
        .child(old)
        .into_any_element()
}

/// How much of the "need you" group shows: it fades in when the first row starts to wait and out
/// when the last one is answered.
fn need_group(needs: Counted) -> Option<(Counted, f32)> {
    match (needs.value, needs.was) {
        (0, Some(was)) if was > 0 => Some((Counted::steady(was), 1. - needs.mix)),
        (0, _) => None,
        (value, Some(0)) => Some((Counted::steady(value), needs.mix)),
        _ => Some((needs, 1.)),
    }
}

/// A section's header: a small muted label and its count. The Queue's also says how many rows
/// wait for an answer, in amber, with the key that jumps to them.
pub fn section_header(section: Section, count: Counted, needs: Counted, cx: &App) -> AnyElement {
    let material = crate::theme::material(cx);
    let warning = cx.theme().colors.warning;
    let label = match section {
        Section::Pinned => "Pinned",
        Section::Queue => "Queue",
    };
    let need = (section == Section::Queue)
        .then(|| need_group(needs))
        .flatten();
    h_flex()
        .debug_selector(|| "section-header".to_string())
        .flex_none()
        .h(HEADER_HEIGHT)
        .items_center()
        .gap_2()
        .px_2()
        .pt_3()
        .pb(px(5.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(material.glass_muted)
        .child(label)
        .child(number(count, Edge::Left))
        .when_some(need, |header, (needs, opacity)| {
            header.child(div().flex_1()).child(
                h_flex()
                    .items_center()
                    .gap(px(5.))
                    .text_color(warning)
                    .opacity(opacity)
                    .child(div().size(px(6.)).rounded_full().bg(warning))
                    .child(
                        h_flex()
                            .gap(px(3.))
                            .child(number(needs, Edge::Left))
                            .child("need you"),
                    )
                    .child(ui::key_caps("cmd-j", cx)),
            )
        })
        .into_any_element()
}

/// What one thread row shows.
#[derive(Clone)]
pub struct Row {
    /// The thread's id, which keys the row's state (its menu).
    pub id: SharedString,
    pub title: SharedString,
    pub dot: Dot,
    pub detail: String,
    /// The second line in the warning color: something waits for you.
    pub attention: bool,
    /// How long ago, on the right; none for a draft.
    pub time: Option<String>,
    /// The thread works in a worktree.
    pub in_copy: bool,
    /// The title in the semibold weight: a finished turn you have not seen.
    pub strong: bool,
    /// The thread on screen.
    pub selected: bool,
    pub archivable: bool,
    /// The row is the memory of one that left, drawn while it fades.
    pub ghost: bool,
}

impl Row {
    /// What a row that has left is drawn as: not selected, and with a hover group of its own so
    /// that the live row of the same thread, wherever it is now, is not hovered with it.
    pub fn leaving(mut self) -> Row {
        self.selected = false;
        self.ghost = true;
        self
    }

    fn hover_group(&self) -> SharedString {
        let kind = if self.ghost { "ghost" } else { "live" };
        SharedString::from(format!("thread-row-{kind}-{}", self.id))
    }
}

/// The height of a thread row.
pub const ROW_HEIGHT: Pixels = px(48.);

/// What a row can do beyond opening: the buttons that show on hover and the right-click menu.
pub struct RowActions {
    pub pinned: bool,
    /// The thread can be put away: not while it works, waits on you or is being prepared.
    pub settlable: bool,
    pub on_pin: OnPress,
    pub on_snooze: OnSnooze,
    pub on_settle: OnPress,
    pub on_archive: OnPress,
}

/// "Later today · in 3 hours": a snooze choice in a menu.
pub fn snooze_label(until: SnoozeUntil) -> String {
    format!("{} · {}", until.label(), until.hint())
}

fn snooze_menu(menu: PopupMenu, on_snooze: &OnSnooze) -> PopupMenu {
    SnoozeUntil::ALL
        .into_iter()
        .fold(menu.min_w(MENU_WIDTH), |menu, until| {
            let on_snooze = on_snooze.clone();
            menu.item(
                PopupMenuItem::new(snooze_label(until))
                    .on_click(move |_, window, cx| on_snooze(until, window, cx)),
            )
        })
}

/// The Snooze and Settle buttons at the right of a row, which show while the pointer is on it.
fn hover_actions(
    id: &SharedString,
    actions: &RowActions,
    group: SharedString,
    cx: &App,
) -> AnyElement {
    let fill = crate::theme::material(cx).popover;
    let on_snooze = actions.on_snooze.clone();
    let on_settle = actions.on_settle.clone();
    h_flex()
        .absolute()
        .right(px(6.))
        .top(px(7.))
        .gap(px(2.))
        .pl(px(18.))
        .rounded(px(6.))
        .opacity(0.)
        .group_hover(group, |actions| actions.opacity(1.))
        .bg(gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(fill.opacity(0.), 0.),
            gpui::linear_color_stop(fill, 0.4),
        ))
        .child(
            ui::icon_button(
                (ElementId::from("snooze"), id.clone()),
                IconName::Clock,
                "Snooze",
                cx,
            )
            .debug_selector(|| "row-snooze".to_string())
            .dropdown_menu(move |menu, _, _| snooze_menu(menu, &on_snooze)),
        )
        .when(actions.settlable, |actions| {
            actions.child(
                ui::icon_button(
                    (ElementId::from("settle"), id.clone()),
                    IconName::Check,
                    "Settle",
                    cx,
                )
                .debug_selector(|| "row-settle".to_string())
                .on_click(move |_, window, cx| on_settle(window, cx)),
            )
        })
        .into_any_element()
}

/// The status dot before a title: 8px, in the thread's color. Working pulses; with Reduce
/// Motion it holds still.
///
/// `grown` (0 to 1) is how far a mark that has just arrived has come: it fades in and grows from
/// 85% of its size. With Reduce Motion it only fades.
pub fn status_dot(dot: Dot, id: impl Into<ElementId>, grown: f32, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let muted = crate::theme::material(cx).glass_muted;
    let size = if cx.reduce_motion() {
        8.
    } else {
        8. * (0.85 + 0.15 * grown)
    };
    let mark = div().flex_none().size(px(size)).rounded_full();
    let dot = match dot {
        Dot::None => mark.into_any_element(),
        Dot::Approval => mark.bg(colors.warning).into_any_element(),
        Dot::Failed => mark.bg(colors.danger).into_any_element(),
        Dot::Unread => mark.bg(cx.theme().sidebar_foreground).into_any_element(),
        Dot::Stopped => mark
            .border(px(1.5))
            .border_color(muted.opacity(0.7))
            .into_any_element(),
        Dot::Working => pulse(mark.bg(colors.info), id, 0., cx),
    };
    div()
        .flex_none()
        .size(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .opacity(grown)
        .child(dot)
        .into_any_element()
}

const PULSE: Duration = Duration::from_millis(1200);

/// A slow fade between full and 45% ink, `phase` (0 to 1) into the loop.
pub fn pulse(element: gpui::Div, id: impl Into<ElementId>, phase: f32, cx: &App) -> AnyElement {
    if cx.reduce_motion() {
        return element.into_any_element();
    }
    element
        .with_animation(
            id,
            Animation::new(PULSE).repeat(),
            move |element, progress| {
                let loop_at = (progress + phase).fract();
                let wave = gpui::ease_in_out(1. - (2. * loop_at - 1.).abs());
                element.opacity(1. - 0.55 * wave)
            },
        )
        .into_any_element()
}

/// One thread: its status dot, its title (cut with "…") and time, and what it does now. A
/// click shows it. On hover it offers Snooze and Settle, and a right click Pin, Snooze, Settle
/// and Archive.
pub fn thread_row(
    row: Row,
    actions: Option<RowActions>,
    on_press: OnPress,
    grown: f32,
    cx: &App,
) -> AnyElement {
    let colors = &cx.theme().colors;
    let material = crate::theme::material(cx);
    let detail_color = if row.attention {
        colors.warning
    } else {
        material.glass_muted
    };
    let archivable = row.archivable;
    let group = row.hover_group();
    let label = format!("{}: {}", row.title, row.detail);
    let full_title = row.title.clone();
    let cut = full_title.chars().count() > TITLE_TOOLTIP_CHARS;
    let dot_id = (ElementId::from("thread-dot"), row.id.clone());
    let line = h_flex()
        .items_center()
        .gap_1()
        .min_w_0()
        .text_size(px(12.))
        .text_color(detail_color)
        .children(row.in_copy.then(|| {
            ui::icon(
                IconName::Layers,
                gpui_kit::component::Size::XSmall,
                detail_color,
            )
            .flex_none()
        }))
        .child(ui::ellipsis(row.detail));
    let title = h_flex()
        .items_center()
        .gap_2()
        .child(
            ui::ellipsis(row.title)
                .flex_1()
                .text_size(px(13.))
                .when(row.strong, |title| title.font_weight(FontWeight::SEMIBOLD)),
        )
        .children(row.time.map(|time| {
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(material.glass_muted)
                .child(time)
        }));
    let hover = actions
        .as_ref()
        .map(|actions| hover_actions(&row.id, actions, group.clone(), cx));
    let item = ListItem::new((ElementId::from("thread-row"), row.id))
        .w_full()
        .h(ROW_HEIGHT)
        .px(px(8.))
        .py(px(7.))
        .rounded(px(7.))
        .selected(row.selected)
        .accessibility_label(label)
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| on_press(window, cx))
        .when(cut, |row| {
            row.tooltip(move |window, cx| Tooltip::new(full_title.clone()).build(window, cx))
        })
        .child(
            h_flex()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(16.))
                        .h(px(18.))
                        .flex()
                        .items_center()
                        .pl(px(3.))
                        .child(status_dot(row.dot, dot_id, grown, cx)),
                )
                .child(v_flex().flex_1().min_w_0().child(title).child(line)),
        )
        .debug_selector(|| "thread-row".to_string())
        .context_menu(move |menu, window, cx| {
            let Some(actions) = &actions else {
                return menu;
            };
            let (on_pin, on_settle, on_archive) = (
                actions.on_pin.clone(),
                actions.on_settle.clone(),
                actions.on_archive.clone(),
            );
            let on_snooze = actions.on_snooze.clone();
            let settlable = actions.settlable;
            menu.min_w(MENU_WIDTH)
                .item(
                    PopupMenuItem::new(if actions.pinned { "Unpin" } else { "Pin" })
                        .on_click(move |_, window, cx| on_pin(window, cx)),
                )
                .submenu("Snooze", window, cx, move |menu, _, _| {
                    snooze_menu(menu, &on_snooze)
                })
                .item(
                    PopupMenuItem::new("Settle")
                        .disabled(!settlable)
                        .on_click(move |_, window, cx| on_settle(window, cx)),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Archive thread")
                        .icon(IconName::Archive)
                        .disabled(!archivable)
                        .on_click(move |_, window, cx| on_archive(window, cx)),
                )
        });
    div()
        .relative()
        .flex_none()
        .group(group)
        .child(item)
        .children(hover)
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
            folder: Folder::Project,
            unread: false,
            waiting_tool: None,
            working_for: None,
            failure: None,
            stopped_on_quit: false,
            background_tasks: 0,
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
    fn a_command_left_running_in_the_background_shows_as_working() {
        let mut done = facts(Status::Done);
        done.background_tasks = 1;
        let look = row_look(&done, NOW);
        assert_eq!(
            (look.dot, look.detail.as_str()),
            (Dot::Working, "Background task running")
        );
        assert!(!look.strong);

        // Unread still makes the title strong; the line says what is going on now.
        done.unread = true;
        done.background_tasks = 3;
        let look = row_look(&done, NOW);
        assert_eq!(
            (look.dot, look.detail.as_str()),
            (Dot::Working, "3 background tasks running")
        );
        assert!(look.strong);

        // The user stopped the turn, or it failed, but what it started in the background still
        // runs (and "Try again" would stop it).
        for status in [Status::Stopped, Status::Failed] {
            let mut over = facts(status);
            over.background_tasks = 2;
            over.failure = Some("Overloaded".to_string());
            let look = row_look(&over, NOW);
            assert_eq!(
                (look.dot, look.detail.as_str()),
                (Dot::Working, "2 background tasks running")
            );
        }

        // A running turn or an approval says more.
        for status in [Status::Working, Status::Waiting] {
            let mut busy = facts(status);
            busy.background_tasks = 1;
            assert_ne!(row_look(&busy, NOW).detail, "Background task running");
        }
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
                copy: false,
            }
        );
    }

    #[test]
    fn a_thread_in_a_worktree_shows_a_copy_icon_and_says_while_it_is_made() {
        let mut making = facts(Status::Idle);
        making.folder = Folder::MakingWorktree;
        assert_eq!(
            row_look(&making, NOW),
            RowLook {
                dot: Dot::Working,
                detail: "Creating a worktree…".to_string(),
                strong: false,
                copy: true,
            }
        );
        let mut working = facts(Status::Working);
        working.folder = Folder::Worktree;
        working.working_for = Some(Duration::from_secs(48));
        let look = row_look(&working, NOW);
        assert_eq!((look.detail.as_str(), look.copy), ("Working · 48s", true));
        let mut draft = facts(Status::Idle);
        draft.draft = true;
        draft.folder = Folder::Worktree;
        assert!(row_look(&draft, NOW).copy);
        assert!(!row_look(&facts(Status::Done), NOW).copy);
    }

    #[test]
    fn the_list_hides_below_760_and_toggles_into_a_drawer() {
        assert_eq!(mode(1200.0, false, false), SidebarMode::Docked);
        assert_eq!(mode(760.0, false, false), SidebarMode::Docked);
        assert_eq!(mode(1200.0, true, false), SidebarMode::Hidden);
        // Wide: the drawer never shows, even if it was left open in a narrow window.
        assert_eq!(mode(1200.0, false, true), SidebarMode::Docked);
        assert_eq!(mode(759.0, false, false), SidebarMode::Hidden);
        assert_eq!(mode(640.0, false, true), SidebarMode::Drawer);
        // The user's choice for wide windows does not keep the drawer shut.
        assert_eq!(mode(640.0, true, true), SidebarMode::Drawer);

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
    fn the_game_shows_until_a_thread_sits_in_the_list() {
        use Placement::*;
        assert!(shows_game(&[]));
        assert!(shows_game(&[listed(true, Read)]));
        assert!(shows_game(&[listed(true, Read), listed(true, Read)]));
        assert!(!shows_game(&[listed(false, Read)]));
        assert!(!shows_game(&[listed(false, Queue)]));
        assert!(!shows_game(&[listed(false, Pinned)]));
        assert!(!shows_game(&[listed(true, Read), listed(false, Read)]));
        assert!(shows_game(&[
            listed(true, Read),
            listed(false, InFlight),
            listed(false, Snoozed),
            listed(false, Settled),
        ]));
    }

    #[test]
    fn the_games_title_says_whether_anything_is_in_flight() {
        use Placement::*;
        assert_eq!(game_title(&[listed(true, Read)]), ALL_CLEAR);
        assert_eq!(
            game_title(&[listed(true, Read), listed(false, Settled)]),
            ALL_CLEAR
        );
        assert_eq!(
            game_title(&[listed(true, Read), listed(false, InFlight)]),
            NOTHING_NEEDS_YOU
        );
    }

    #[test]
    fn the_hidden_list_calls_for_you_only_for_other_threads() {
        let statuses = [Status::Done, Status::Waiting, Status::Working];
        assert!(hidden_needs_you(&statuses, 0));
        // The waiting thread is the one on screen: its dock already asks.
        assert!(!hidden_needs_you(&statuses, 1));
        assert!(!hidden_needs_you(&[Status::Working, Status::Failed], 0));
    }

    fn listed(draft: bool, placement: Placement) -> Listed {
        Listed {
            draft,
            placement,
            waits: false,
            list: ListState::default(),
        }
    }

    fn landed(placement: Placement, landed_at: i64) -> Listed {
        Listed {
            list: ListState {
                landed_at,
                ..ListState::default()
            },
            ..listed(false, placement)
        }
    }

    fn header(section: Section, count: usize, needs: usize) -> Entry {
        Entry::Header {
            section,
            count,
            needs,
        }
    }

    #[test]
    fn the_list_goes_drafts_pinned_queue_then_the_read_list_newest_landing_first() {
        use Placement::*;
        let threads = [
            landed(Read, 10),
            listed(true, Read),
            landed(Read, 30),
            Listed {
                waits: true,
                ..landed(Queue, 5)
            },
            landed(Queue, 40),
            Listed {
                list: ListState {
                    pinned_at: Some(2),
                    landed_at: 99,
                    ..ListState::default()
                },
                ..listed(false, Pinned)
            },
            Listed {
                list: ListState {
                    pinned_at: Some(1),
                    ..ListState::default()
                },
                ..listed(false, Pinned)
            },
            landed(InFlight, 50),
        ];
        let layout = display_order(&threads);
        assert_eq!(
            layout.entries,
            vec![
                Entry::Thread(1),
                header(Section::Pinned, 2, 0),
                Entry::Thread(6),
                Entry::Thread(5),
                header(Section::Queue, 2, 1),
                Entry::Thread(4),
                Entry::Thread(3),
                Entry::Gap,
                Entry::Thread(2),
                Entry::Thread(0),
            ]
        );
        assert_eq!(layout.in_flight, vec![7]);
        assert_eq!(thread_order(&layout), vec![1, 6, 5, 4, 3, 2, 0, 7]);
    }

    #[test]
    fn a_header_shows_only_while_it_has_rows_and_the_read_list_has_none() {
        use Placement::*;
        assert!(display_order(&[]).entries.is_empty());
        assert_eq!(
            display_order(&[landed(Read, 1), landed(Read, 2)]).entries,
            vec![Entry::Thread(1), Entry::Thread(0)]
        );
        assert_eq!(
            display_order(&[landed(Queue, 1)]).entries,
            vec![header(Section::Queue, 1, 0), Entry::Thread(0)]
        );
        assert_eq!(
            display_order(&[listed(true, Read), landed(Read, 1)]).entries,
            vec![Entry::Thread(0), Entry::Thread(1)]
        );
    }

    #[test]
    fn a_row_moves_only_when_something_lands() {
        use Placement::*;
        let before = [landed(Read, 10), landed(Read, 20)];
        assert_eq!(display_order(&before).entries[0], Entry::Thread(1));
        // The older thread lands again: it goes on top.
        let after = [landed(Read, 30), landed(Read, 20)];
        assert_eq!(display_order(&after).entries[0], Entry::Thread(0));
    }

    #[test]
    fn parked_threads_wait_in_their_panels_by_date() {
        use Placement::*;
        let snoozed = |until| Listed {
            list: ListState {
                snoozed_until: Some(until),
                ..ListState::default()
            },
            ..listed(false, Snoozed)
        };
        let settled = |at| Listed {
            list: ListState {
                settled_at: Some(at),
                ..ListState::default()
            },
            ..listed(false, Settled)
        };
        let layout = display_order(&[snoozed(30), settled(1), snoozed(10), settled(5)]);
        assert_eq!(layout.snoozed, vec![2, 0]);
        assert_eq!(layout.settled, vec![3, 1]);
        assert!(layout.entries.is_empty());
        assert!(thread_order(&layout).is_empty());
    }

    #[test]
    fn the_thread_on_screen_stays_where_it_was_opened() {
        use Placement::*;
        assert_eq!(held(Queue, Read), Queue);
        assert_eq!(held(Read, Queue), Read);
        assert_eq!(held(Read, InFlight), Read);
        assert_eq!(held(Queue, InFlight), Queue);
        assert_eq!(held(InFlight, InFlight), InFlight);
        assert_eq!(held(InFlight, Read), Queue);
        assert_eq!(held(InFlight, Queue), Queue);
        for user_move in [Pinned, Snoozed, Settled] {
            assert_eq!(held(Queue, user_move), user_move);
            assert_eq!(held(Read, user_move), user_move);
            assert_eq!(held(InFlight, user_move), user_move);
        }
        for was in [Pinned, Snoozed, Settled] {
            assert_eq!(held(was, Read), Read);
            assert_eq!(held(was, Queue), Queue);
        }
    }

    #[test]
    fn a_row_that_left_is_not_selected_and_shares_no_hover_group_with_the_live_row() {
        let live = Row {
            id: "t1".into(),
            title: "Title".into(),
            dot: Dot::None,
            detail: "Done".to_string(),
            attention: false,
            time: None,
            in_copy: false,
            strong: false,
            selected: true,
            archivable: true,
            ghost: false,
        };
        let ghost = live.clone().leaving();
        assert!(live.selected && !ghost.selected);
        assert_ne!(live.hover_group(), ghost.hover_group());
        assert_eq!(ghost.id, live.id);
    }

    #[test]
    fn a_held_row_keeps_its_landing_time_and_a_moved_one_takes_the_new_time() {
        use Placement::*;
        let opened = hold(None, Read, 100);
        assert_eq!(
            opened,
            Held {
                placement: Read,
                landed_at: 100
            }
        );
        assert_eq!(hold(Some(opened), Read, 900), opened);
        assert_eq!(hold(Some(opened), Queue, 900), opened);
        assert_eq!(hold(Some(opened), InFlight, 900), opened);
        let queued = hold(None, Queue, 100);
        assert_eq!(hold(Some(queued), Read, 900), queued);

        let tray = hold(None, InFlight, 100);
        assert_eq!(hold(Some(tray), InFlight, 900), tray);
        assert_eq!(
            hold(Some(tray), Read, 900),
            Held {
                placement: Queue,
                landed_at: 900
            }
        );

        assert_eq!(
            hold(Some(opened), Pinned, 900),
            Held {
                placement: Pinned,
                landed_at: 900
            }
        );
        let pinned = hold(Some(opened), Pinned, 900);
        assert_eq!(hold(Some(pinned), Pinned, 950), pinned);
        assert_eq!(
            hold(Some(pinned), Read, 960),
            Held {
                placement: Read,
                landed_at: 960
            }
        );
    }

    #[test]
    fn jumping_to_what_needs_you_goes_to_the_next_one_and_wraps() {
        // Display order 1, 3, 5, 2, 0, 4; threads 3 and 5 need you.
        let order = [1, 3, 5, 2, 0, 4];
        let mut needs = [false; 6];
        needs[3] = true;
        needs[5] = true;
        assert_eq!(next_needs_you(&order, 1, &needs), Some(3));
        assert_eq!(next_needs_you(&order, 3, &needs), Some(5));
        // After the last one it wraps to the first.
        assert_eq!(next_needs_you(&order, 5, &needs), Some(3));
        assert_eq!(next_needs_you(&order, 4, &needs), Some(3));
        // Only the thread on screen needs you: nowhere to go.
        needs[5] = false;
        assert_eq!(next_needs_you(&order, 3, &needs), None);
        assert_eq!(next_needs_you(&order, 0, &needs), Some(3));
        needs[3] = false;
        assert_eq!(next_needs_you(&order, 0, &needs), None);
        assert_eq!(next_needs_you(&[], 0, &[]), None);
    }

    #[test]
    fn counts_stop_at_99() {
        assert_eq!(count_label(0), "0");
        assert_eq!(count_label(99), "99");
        assert_eq!(count_label(100), "99+");
        assert_eq!(count_label(12_345), "99+");
    }

    #[test]
    fn the_time_on_a_row_is_short() {
        let ago = |secs: i64| compact_time(NOW - secs * 1000, NOW);
        assert_eq!(ago(5), "now");
        assert_eq!(ago(120), "2m");
        assert_eq!(ago(3600), "1h");
        assert_eq!(ago(86_400 * 3), "3d");
        assert_eq!(ago(86_400 * 21), "3w");
        assert_eq!(ago(86_400 * 330), "11mo");
        assert_eq!(ago(86_400 * 800), "2y");
        // A clock that moved back reads as now.
        assert_eq!(compact_time(NOW + 5000, NOW), "now");
    }

    #[test]
    fn the_second_line_prefers_the_activity_and_never_repeats_the_time() {
        let done = row_look(&facts(Status::Done), NOW);
        assert_eq!(
            detail_line(Some("Answered".into()), &done, false),
            "Answered"
        );
        assert_eq!(detail_line(None, &done, false), "Done");
        assert_eq!(detail_line(Some("  ".into()), &done, false), "Done");
        let mut waiting = facts(Status::Waiting);
        waiting.waiting_tool = Some("Bash".into());
        let look = row_look(&waiting, NOW);
        assert_eq!(detail_line(None, &look, false), "Needs approval · Bash");
        assert_eq!(detail_line(None, &done, true), "Draft");
    }
}
