//! The bar across the top of the chat (screens 8–11): the thread's title and folder in the
//! middle; the "N allowed" chip with its list of remembered rules and the status pill on the
//! right; and, while the thread list is out of sight, the button that brings it back on the
//! left.
//!
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_kit::base::{FocusTrapElement as _, Selectable, h_flex, v_flex};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Sizable as _, tag::Tag};
use newjen_core::{GrantId, GrantView, Status};

use crate::sidebar::{self, OnPress};
use crate::ui::{self, AgentState, IconName, Look, Tone, ellipsis, middle_ellipsis};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::TITLE_BAR_HEIGHT;
use gpui_kit::component::button::{Button, ButtonVariants as _};

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
/// The footer's second line, when it also says a revoke stops the background commands.
const GRANTS_FOOTER_LINE: f32 = 18.0;
/// Room kept between the popover and the window's bottom edge (and the small drop it opens
/// with under the chip).
const GRANTS_MARGIN: f32 = 16.0;
/// The rows always get at least this much, about two rules.
const GRANT_ROWS_MIN: f32 = 100.0;
/// Called with a grant's rule when its Revoke button is pressed.
pub type OnRevoke = Rc<dyn Fn(&GrantId, &mut Window, &mut App)>;

/// Called when the grants popover opens (`true`) or closes.
pub type OnGrantsOpen = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The grants popover's state, which the chat view owns so it can close the popover when focus
/// leaves it.
pub struct Grants {
    pub open: bool,
    /// Focused while the popover is open.
    pub focus: FocusHandle,
    pub on_open: OnGrantsOpen,
    pub on_revoke: OnRevoke,
}

/// What the bar shows.
pub struct Bar {
    /// The thread's title, e.g. "Fix the flaky orphan test" or "New thread".
    pub title: SharedString,
    /// The folder the thread's claude works in, as `~/…`, already cut to fit.
    pub path: String,
    pub copy: bool,
    /// Set while the thread list is out of sight: the button that shows it, and whether
    /// another thread waits for an approval (an amber dot on the button).
    pub show_list: Option<(OnPress, bool)>,
    /// The thread's "Always allow" rules.
    pub grants: Vec<GrantView>,
    pub grants_detail: &'static str,
    pub pill: Option<Pill>,
    /// How many commands Claude keeps running in the background.
    pub background_tasks: usize,
}

/// The most height the grants popover's rows may take in a window `viewport_height` pixels
/// tall. The popover hangs from the title bar and sizes to its rows, so without this
/// a long list would run off the bottom of the window with its Revoke buttons. Past this the
/// rows scroll, and the header and footer stay put.
pub fn grant_rows_max_height(
    viewport_height: f32,
    title_bar_height: f32,
    background_tasks: usize,
) -> f32 {
    let footer = if background_tasks > 0 {
        GRANTS_FOOTER_LINE
    } else {
        0.0
    };
    (viewport_height - title_bar_height - GRANTS_CHROME - footer - GRANTS_MARGIN)
        .max(GRANT_ROWS_MIN)
}

/// The popover's footer. A revoke stops claude, and the commands it runs in the background
/// stop with it, so it says that too while any run.
pub fn grants_footer(background_tasks: usize) -> String {
    let when = "A revoke takes effect before your next message.";
    match background_tasks {
        0 => when.to_string(),
        1 => format!("{when} It also stops the command running in the background."),
        count => format!("{when} It also stops the {count} commands running in the background."),
    }
}

/// How many characters of the path fit beside the title in a chat `main_width` pixels wide.
/// The title gives way first (it is cut with "…"); the path keeps about a quarter of the bar,
/// cut in its middle (`format::short_path`) so its last folders stay readable.
pub fn path_budget(main_width: f32) -> usize {
    ((main_width * PATH_SHARE / PATH_CHAR).max(0.0) as usize).clamp(PATH_MIN_CHARS, PATH_MAX_CHARS)
}

/// Draws the bar.
pub fn render(bar: Bar, grants: Grants, _window: &mut Window, cx: &mut App) -> AnyElement {
    let colors = cx.theme().colors;
    let lights_over_bar = bar.show_list.is_some();

    // Left: Kit's room for the traffic lights, which sit over this bar while the list is
    // hidden, then the button that shows the list.
    let left = div()
        .flex_none()
        .flex()
        .items_center()
        .when_some(bar.show_list, |left, (on_press, attention)| {
            left.child(show_list_button(on_press, attention, cx))
        });
    let middle = h_flex()
        .flex_1()
        .w_0()
        .min_w_0()
        .gap_2()
        .child(
            gpui_kit::component::label::Label::new(bar.title)
                .flex_1()
                .min_w_0()
                .text_ellipsis(),
        )
        .when(bar.copy, |middle| {
            middle.child(
                div()
                    .flex_none()
                    .debug_selector(|| "rift-copy-tag".to_string())
                    .child(Tag::secondary().small().child("Rift copy")),
            )
        })
        .when(!bar.path.is_empty(), |middle| {
            middle.child(
                div()
                    .min_w_0()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .text_color(colors.muted_foreground)
                    .child(middle_ellipsis(bar.path)),
            )
        });
    let right = div().flex_none().flex().justify_end().pl_2().pr_3().child(
        // Over the drag region, so a press here is the control's, not a window drag.
        h_flex()
            .debug_selector(|| "title-bar-right".to_string())
            .gap_1p5()
            .occlude()
            .when(!bar.grants.is_empty(), |right| {
                right.child(grants_popover(
                    bar.grants,
                    bar.grants_detail,
                    bar.background_tasks,
                    grants,
                ))
            })
            .children(bar.pill.map(status_pill)),
    );
    ui::card_title_bar(cx)
        .when(!lights_over_bar, |bar| bar.pl_4())
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
            cx,
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
                    .border_color(colors.background),
            )
        })
        .into_any_element()
}

/// How the status pill looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pill {
    /// The agent status tag.
    Agent(AgentState),
    /// A badge with a dot: this word, in this tone.
    Badge(&'static str, Tone),
    /// A blue badge with a dot: this many commands still run in the background.
    Background(usize),
}

pub const STARTING: Pill = Pill::Badge("Starting", Tone::Info);

/// The pill for a thread's status, worded as the mocks and the thread list word it (screens
/// 8–11): "Needs approval" in amber, a quiet "Idle" once a turn is done, a quiet "Stopped",
/// and "Background task running" instead of those (and of failed) while a command Claude
/// started still runs. The agent tag stays for working and
/// failed.
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

fn status_pill(pill: Pill) -> AnyElement {
    match pill {
        Pill::Agent(state) => ui::agent_pill(state).into_any_element(),
        Pill::Badge(label, tone) => ui::status_badge(label, tone).into_any_element(),
        Pill::Background(count) => {
            ui::status_badge(sidebar::background_detail(count), Tone::Info).into_any_element()
        }
    }
}

/// "🛡 2 allowed": opens the list of the thread's remembered rules. Tab stays inside it; Esc, a
/// press outside or focus moving elsewhere closes it.
fn grants_popover(
    grants: Vec<GrantView>,
    detail: &'static str,
    background_tasks: usize,
    state: Grants,
) -> AnyElement {
    let label = format!("{} allowed", grants.len());
    let on_open = state.on_open;
    let on_revoke = state.on_revoke;
    let focus = state.focus.clone();
    Popover::new("grants")
        .anchor(Anchor::TopLeft)
        .trigger(GrantsChip { label })
        .overlay_closable(false)
        .open(state.open)
        .on_open_change(move |open, window, cx| on_open(*open, window, cx))
        .track_focus(&state.focus)
        .content(move |_, window, cx| {
            let dismiss = cx.listener(|state, _, window, cx| state.dismiss(window, cx));
            let panel = grants_panel(&grants, detail, background_tasks, &on_revoke, window, cx);
            div()
                .child(panel)
                .on_mouse_down_out(dismiss)
                .focus_trap("grants-trap", &focus)
        })
        .into_any_element()
}

/// The chip that opens the popover.
#[derive(IntoElement)]
struct GrantsChip {
    label: String,
}

impl Selectable for GrantsChip {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl RenderOnce for GrantsChip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Button::new("grants-chip")
            .label(self.label)
            .icon(IconName::Shield)
            .ghost()
            .debug_selector(|| "grants-chip".to_string())
    }
}

/// The popover's content (the "Allowed rules popover" frame of the mock).
fn grants_panel(
    grants: &[GrantView],
    detail: &str,
    background_tasks: usize,
    on_revoke: &OnRevoke,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let colors = &cx.theme().colors;
    let small = gpui::rems(0.75);
    let rows_height = grant_rows_max_height(
        f32::from(window.viewport_size().height),
        f32::from(TITLE_BAR_HEIGHT),
        background_tasks,
    );
    let header = v_flex()
        .gap_0p5()
        .px_2()
        .pt_1p5()
        .pb_2()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.foreground)
                .child("Always allowed in this thread"),
        )
        .child(
            div()
                .text_size(small)
                .text_color(colors.muted_foreground)
                .child(detail.to_string()),
        );
    let rows = grants.iter().enumerate().map(|(ix, grant)| {
        let parts = grant.clone();
        let rule = grant.id.clone();
        let on_revoke = on_revoke.clone();
        h_flex()
            .gap_2()
            .px_2()
            .py_1p5()
            .child(
                div()
                    .flex_none()
                    .text_size(small)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.muted_foreground)
                    .child(parts.tool),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_sm()
                            .text_color(colors.foreground)
                            .child(middle_ellipsis(parts.shown)),
                    )
                    .children(parts.note.map(|note| {
                        div()
                            .min_w_0()
                            .text_size(small)
                            .text_color(colors.muted_foreground)
                            .child(ellipsis(note))
                    })),
            )
            .child(
                ui::button(("revoke", ix), "Revoke", Look::Outline, ui::Size::Sm, cx)
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
        .text_color(colors.muted_foreground)
        .child(grants_footer(background_tasks));
    // Only the rows scroll (it needs an id to keep its scroll position).
    let rows = v_flex()
        .id("grant-rows")
        .max_h(px(rows_height))
        .overflow_y_scroll()
        .gap_0p5()
        .children(rows);
    v_flex()
        .w(px(GRANTS_WIDTH))
        .gap_0p5()
        .child(header)
        .child(rows)
        .child(footer)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Narrow {
        focus: FocusHandle,
    }

    impl gpui::Render for Narrow {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let bar = Bar {
                title: "Reconcile the billing data for the partial reversal handler and every \
                        currency pair"
                    .into(),
                path: "…/verify-project".into(),
                copy: true,
                show_list: Some((Rc::new(|_, _| {}), false)),
                grants: vec![],
                grants_detail: "Claude runs these without asking, until you revoke them.",
                pill: pill(Status::Waiting, 0),
                background_tasks: 0,
            };
            let grants = Grants {
                open: false,
                focus: self.focus.clone(),
                on_open: Rc::new(|_, _, _| {}),
                on_revoke: Rc::new(|_, _, _| {}),
            };
            div().w_full().child(render(bar, grants, window, cx))
        }
    }

    #[gpui::test]
    fn a_long_title_gives_way_to_the_status_pill(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, cx| Narrow {
            focus: cx.focus_handle(),
        });
        cx.simulate_resize(gpui::size(px(640.), px(480.)));
        cx.run_until_parked();
        let right = cx.debug_bounds("title-bar-right").unwrap();
        assert!(right.right() <= px(640.), "{right:?}");
        let tag = cx.debug_bounds("rift-copy-tag").unwrap();
        assert!(tag.size.width > px(40.), "the tag keeps its width: {tag:?}");
        assert!(tag.right() <= right.left(), "{tag:?} {right:?}");
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
        let small = grant_rows_max_height(480.0, 38.0, 0);
        assert_eq!(small, 480.0 - 38.0 - GRANTS_CHROME - GRANTS_MARGIN);
        assert!(38.0 + GRANTS_CHROME + small + GRANTS_MARGIN <= 480.0);
        // About five 50 px rules fit before the rows scroll at 640×480, a dozen at 760 tall.
        assert!((250.0..300.0).contains(&small), "{small}");
        assert!(grant_rows_max_height(760.0, 38.0, 0) > 500.0);
        // A tiny height still leaves room for a couple of rules.
        assert_eq!(grant_rows_max_height(100.0, 38.0, 0), GRANT_ROWS_MIN);
        // The footer's background note takes a second line.
        assert_eq!(
            grant_rows_max_height(480.0, 38.0, 2),
            small - GRANTS_FOOTER_LINE
        );
    }

    #[test]
    fn a_revoke_says_it_also_stops_background_commands() {
        let when = "A revoke takes effect before your next message.";
        assert_eq!(grants_footer(0), when);
        assert_eq!(
            grants_footer(1),
            format!("{when} It also stops the command running in the background.")
        );
        assert_eq!(
            grants_footer(3),
            format!("{when} It also stops the 3 commands running in the background.")
        );
    }
}
