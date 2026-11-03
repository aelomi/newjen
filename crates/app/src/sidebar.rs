//! The thread list on the left: the project switcher, threads grouped by what needs you, and
//! the usage footer (see `design/newjen.pen` and the v3 mocks).
//!
//! The decisions come first as plain functions, tested without a window: which group a row
//! sits in and in what order, what a row says, where the list sits at each window width, which
//! thread ⌘J goes to. The elements that draw them come after; the chat view wires them to the
//! threads.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AnyElement, App, ElementId, FontWeight, IntoElement, MouseButton, ParentElement, Pixels,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, list::ListItem};
use newjen_core::Status;
use newjen_core::steps::SidebarGroup;
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

/// The groups, top to bottom.
pub const GROUPS: [SidebarGroup; 3] = [
    SidebarGroup::NeedsYou,
    SidebarGroup::Working,
    SidebarGroup::Done,
];

/// A section header's label.
pub fn group_label(group: SidebarGroup) -> &'static str {
    match group {
        SidebarGroup::NeedsYou => "Needs you",
        SidebarGroup::Working => "Working",
        SidebarGroup::Done => "Done",
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

/// One line of the list: a section's header or a thread (its index in the view's thread order).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Header { group: SidebarGroup, count: usize },
    Thread(usize),
}

/// The list from the threads' `(draft, group)`, in the view's thread order: drafts first and
/// without a header, then a header and its threads for each group that has any, in
/// [`GROUPS`] order. A group keeps its threads in the view's order, so a thread that moves
/// between groups slots in by age, not by when it changed.
pub fn display_order(threads: &[(bool, SidebarGroup)]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = threads
        .iter()
        .enumerate()
        .filter(|(_, (draft, _))| *draft)
        .map(|(ix, _)| Entry::Thread(ix))
        .collect();
    for group in GROUPS {
        let members: Vec<usize> = threads
            .iter()
            .enumerate()
            .filter(|(_, (draft, of))| !draft && *of == group)
            .map(|(ix, _)| ix)
            .collect();
        if members.is_empty() {
            continue;
        }
        entries.push(Entry::Header {
            group,
            count: members.len(),
        });
        entries.extend(members.into_iter().map(Entry::Thread));
    }
    entries
}

/// The threads in the order the list shows them: what ⌘1 to ⌘9 and ⌘⇧[ ⌘⇧] count through.
pub fn thread_order(entries: &[Entry]) -> Vec<usize> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Thread(ix) => Some(*ix),
            Entry::Header { .. } => None,
        })
        .collect()
}

/// Where thread `ix` sits among `entries` (headers count), to scroll to it.
pub fn entry_of(entries: &[Entry], ix: usize) -> Option<usize> {
    entries.iter().position(|entry| *entry == Entry::Thread(ix))
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

/// What the row's status icon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// An approval, a usage limit or a question waits for you.
    Attention,
    Failed,
    Working,
    Done,
    Draft,
}

pub fn mark(group: SidebarGroup, look: &RowLook, draft: bool) -> Mark {
    if draft {
        return Mark::Draft;
    }
    match group {
        SidebarGroup::NeedsYou | SidebarGroup::Done if look.dot == Dot::Failed => Mark::Failed,
        SidebarGroup::NeedsYou => Mark::Attention,
        SidebarGroup::Working => Mark::Working,
        SidebarGroup::Done => Mark::Done,
    }
}

/// The row's second line: what the thread is doing (core's activity line), else the row look's
/// own words. A finished thread's look only says how long ago, which the row's time already
/// does, so it says "Done".
pub fn detail_line(
    activity: Option<String>,
    look: &RowLook,
    group: SidebarGroup,
    draft: bool,
) -> String {
    if draft {
        return DRAFT_LINE.to_string();
    }
    if let Some(line) = activity.filter(|line| !line.trim().is_empty()) {
        return line;
    }
    match (group, look.dot) {
        (SidebarGroup::Done, Dot::None | Dot::Unread) => "Done".to_string(),
        _ => look.detail.clone(),
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
/// it shows [`EMPTY_LIST`] under that draft's row. A draft kept beside saved threads is a row
/// of its own.
pub fn is_empty_list(drafts: &[bool]) -> bool {
    drafts.len() <= 1 && drafts.iter().all(|draft| *draft)
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

/// "No threads yet. Your first message starts one."
pub fn empty_caption(cx: &App) -> AnyElement {
    div()
        .px_2p5()
        .py_2()
        .text_sm()
        .text_color(crate::theme::material(cx).glass_muted)
        .child(EMPTY_LIST)
        .into_any_element()
}

/// "Needs you 2": a small muted label and its count.
pub fn section_header(group: SidebarGroup, count: usize, cx: &App) -> AnyElement {
    h_flex()
        .flex_none()
        .items_baseline()
        .gap_1p5()
        .px_2p5()
        .pt_3()
        .pb_1()
        .text_sm()
        .text_color(crate::theme::material(cx).glass_muted)
        .child(group_label(group))
        .child(div().opacity(0.7).child(count_label(count)))
        .into_any_element()
}

/// The row at the foot of the list that opens the Usage view: a gauge, "Usage" and the meter
/// of the fullest window.
pub fn usage_row(
    selected: bool,
    meter: Option<AnyElement>,
    on_press: OnPress,
    cx: &App,
) -> AnyElement {
    let muted = crate::theme::material(cx).glass_muted;
    div()
        .id("usage-row")
        .flex_none()
        .pb_2()
        .child(
            ListItem::new("usage-row-item")
                .w_full()
                .px_2p5()
                .py_1p5()
                .rounded_lg()
                .selected(selected)
                .accessibility_label("Usage")
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(move |_, window, cx| on_press(window, cx))
                .debug_selector(|| "usage-row".to_string())
                .child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(ui::icon(
                            IconName::Gauge,
                            gpui_kit::component::Size::Small,
                            muted,
                        ))
                        .child(div().flex_1().text_color(muted).child("Usage"))
                        .children(meter),
                ),
        )
        .into_any_element()
}

/// What one thread row shows.
pub struct Row {
    /// The thread's id, which keys the row's state (its menu).
    pub id: SharedString,
    pub title: SharedString,
    pub mark: Mark,
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
}

fn mark_icon(mark: Mark, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let size = gpui_kit::component::Size::Small;
    let muted = crate::theme::material(cx).glass_muted;
    match mark {
        Mark::Attention => ui::icon(IconName::CircleAlert, size, colors.warning).into_any_element(),
        Mark::Failed => ui::icon(IconName::CircleAlert, size, colors.danger).into_any_element(),
        Mark::Working => Spinner::new()
            .with_size(size)
            .color(colors.cyan)
            .into_any_element(),
        Mark::Done => ui::icon(IconName::CircleCheck, size, muted).into_any_element(),
        Mark::Draft => ui::icon(IconName::SquarePen, size, muted).into_any_element(),
    }
}

/// One thread: its status icon, its title (cut with "…") and time, and what it does now. A
/// click shows it; a right click offers "Archive thread".
pub fn thread_row(row: Row, on_press: OnPress, on_archive: OnPress, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let material = crate::theme::material(cx);
    let detail_color = if row.attention {
        colors.warning
    } else {
        material.glass_muted
    };
    let archivable = row.archivable;
    let label = format!("{}: {}", row.title, row.detail);
    let full_title = row.title.clone();
    let cut = full_title.chars().count() > TITLE_TOOLTIP_CHARS;
    let line = h_flex()
        .items_center()
        .gap_1()
        .min_w_0()
        .text_sm()
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
                .when(row.strong, |title| title.font_weight(FontWeight::SEMIBOLD)),
        )
        .children(row.time.map(|time| {
            div()
                .flex_none()
                .text_sm()
                .text_color(material.glass_muted)
                .child(time)
        }));
    ListItem::new((ElementId::from("thread-row"), row.id))
        .w_full()
        .px_2p5()
        .py_1p5()
        .rounded_lg()
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
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .child(mark_icon(row.mark, cx)),
                )
                .child(v_flex().flex_1().min_w_0().child(title).child(line)),
        )
        .debug_selector(|| "thread-row".to_string())
        .context_menu(move |menu, _, _| {
            let on_archive = on_archive.clone();
            menu.min_w(MENU_WIDTH).item(
                PopupMenuItem::new("Archive thread")
                    .icon(IconName::Archive)
                    .disabled(!archivable)
                    .on_click(move |_, window, cx| on_archive(window, cx)),
            )
        })
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

    use SidebarGroup::{Done, NeedsYou, Working};

    #[test]
    fn the_list_goes_drafts_then_needs_you_working_done_without_empty_groups() {
        // slot order is newest first: a draft, then threads in mixed groups.
        let threads = [
            (false, Done),
            (true, Done),
            (false, Working),
            (false, NeedsYou),
            (false, Done),
            (false, NeedsYou),
        ];
        assert_eq!(
            display_order(&threads),
            vec![
                Entry::Thread(1),
                Entry::Header {
                    group: NeedsYou,
                    count: 2
                },
                Entry::Thread(3),
                Entry::Thread(5),
                Entry::Header {
                    group: Working,
                    count: 1
                },
                Entry::Thread(2),
                Entry::Header {
                    group: Done,
                    count: 2
                },
                Entry::Thread(0),
                Entry::Thread(4),
            ]
        );
        let order = thread_order(&display_order(&threads));
        assert_eq!(order, vec![1, 3, 5, 2, 0, 4]);
        assert_eq!(entry_of(&display_order(&threads), 2), Some(5));
        // A group with no thread has no header.
        assert_eq!(
            display_order(&[(false, Working)]),
            vec![
                Entry::Header {
                    group: Working,
                    count: 1
                },
                Entry::Thread(0)
            ]
        );
        assert!(display_order(&[]).is_empty());
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
            detail_line(Some("Answered".into()), &done, Done, false),
            "Answered"
        );
        assert_eq!(detail_line(None, &done, Done, false), "Done");
        assert_eq!(detail_line(Some("  ".into()), &done, Done, false), "Done");
        let mut waiting = facts(Status::Waiting);
        waiting.waiting_tool = Some("Bash".into());
        let look = row_look(&waiting, NOW);
        assert_eq!(
            detail_line(None, &look, NeedsYou, false),
            "Needs approval · Bash"
        );
        assert_eq!(detail_line(None, &done, Done, true), "Draft");
    }

    #[test]
    fn the_status_icon_follows_the_group() {
        let failed = row_look(
            &RowFacts {
                failure: Some("x".into()),
                ..facts(Status::Failed)
            },
            NOW,
        );
        let done = row_look(&facts(Status::Done), NOW);
        assert_eq!(mark(NeedsYou, &failed, false), Mark::Failed);
        assert_eq!(mark(Done, &failed, false), Mark::Failed);
        assert_eq!(mark(NeedsYou, &done, false), Mark::Attention);
        assert_eq!(mark(Working, &done, false), Mark::Working);
        assert_eq!(mark(Done, &done, false), Mark::Done);
        assert_eq!(mark(Done, &done, true), Mark::Draft);
    }
}
