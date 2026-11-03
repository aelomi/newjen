//! The bar across the top of the chat: the thread's title and where it works (Local or a
//! worktree) in the middle; the shield with its list of rules allowed in the thread, the terminal
//! button and the Changes button on the right; and, while the thread list is out of sight, the
//! button that brings it back on the left.
//!
use std::rc::Rc;

use gpui::{
    Anchor, AnyElement, App, ClickEvent, FocusHandle, FontWeight, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_kit::base::{FocusTrapElement as _, Selectable, h_flex, v_flex};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use newjen_core::{GrantId, GrantView};

use crate::sidebar::{self, OnPress};
use crate::ui::{self, IconName, Look, ellipsis, middle_ellipsis};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::TITLE_BAR_HEIGHT;
use gpui_kit::component::button::{Button, ButtonVariants as _};

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
    /// The popover was opened with the mouse, so it slides in.
    pub pointer: bool,
}

/// What the bar shows.
pub struct Bar {
    /// The thread's title, e.g. "Fix the flaky orphan test" or "New thread".
    pub title: SharedString,
    /// The folder the thread's claude works in, as `~/…`: the where chip's tooltip. Empty while
    /// the worktree is made and its folder is not known yet.
    pub path: String,
    /// The thread works in a worktree (the chip says "Worktree"), not the project folder.
    pub copy: bool,
    /// Set while the thread list is out of sight: the button that shows it, and whether
    /// another thread waits for an approval (an amber dot on the button).
    pub show_list: Option<(OnPress, sidebar::ListBadge)>,
    /// The thread's "Allow in this thread" rules.
    pub grants: Vec<GrantView>,
    pub grants_detail: &'static str,
    /// How many commands Claude keeps running in the background.
    pub background_tasks: usize,
    /// The button that shows and hides the terminal panel.
    pub terminal: TerminalToggle,
    /// The button that shows and hides the Changes card; none while there is nothing to show.
    pub changes: Option<ChangesToggle>,
}

/// Called with the click, so a key press and a mouse click can tell themselves apart.
pub type OnClick = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

pub struct TerminalToggle {
    /// The panel is on screen.
    pub shown: bool,
    pub on_press: OnClick,
}

pub struct ChangesToggle {
    /// The card is on screen.
    pub shown: bool,
    pub tooltip: &'static str,
    pub on_press: OnClick,
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

/// Draws the bar.
pub fn render(bar: Bar, grants: Grants, _window: &mut Window, cx: &mut App) -> AnyElement {
    let lights_over_bar = bar.show_list.is_some();

    // Left: Kit's room for the traffic lights, which sit over this bar while the list is
    // hidden, then the button that shows the list.
    let left = div()
        .flex_none()
        .flex()
        .items_center()
        .when_some(bar.show_list, |left, (on_press, badge)| {
            left.child(show_list_button(on_press, badge, cx))
        });
    let middle = h_flex()
        .flex_1()
        .w_0()
        .min_w_0()
        .gap_2()
        .child(
            gpui_kit::component::label::Label::new(bar.title)
                .min_w_0()
                .text_ellipsis(),
        )
        .child(where_chip(bar.copy, bar.path, cx));
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
            .child(terminal_button(bar.terminal, cx))
            .children(bar.changes.map(|toggle| changes_button(toggle, cx))),
    );
    ui::card_title_bar(cx)
        .when(!lights_over_bar, |bar| bar.pl_4())
        .child(left)
        .child(middle)
        .child(right)
        .into_any_element()
}

/// The bar above the Usage or Settings view: its name and what it shows, with the button that
/// brings the thread list back while it is out of sight.
pub fn pane_header(
    title: &'static str,
    subtitle: &'static str,
    show_list: Option<(OnPress, sidebar::ListBadge)>,
    cx: &mut App,
) -> AnyElement {
    let lights_over_bar = show_list.is_some();
    let muted = cx.theme().colors.muted_foreground;
    ui::card_title_bar(cx)
        .when(!lights_over_bar, |bar| bar.pl_4())
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .when_some(show_list, |left, (on_press, badge)| {
                    left.child(show_list_button(on_press, badge, cx))
                }),
        )
        .child(
            h_flex()
                .flex_1()
                .gap_2()
                .items_baseline()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_sm().text_color(muted).child(subtitle)),
        )
        .into_any_element()
}

/// Where the thread works: the project folder ("Local") or its worktree, quiet beside the
/// title. The full folder is its tooltip.
fn where_chip(copy: bool, path: String, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let (glyph, label) = if copy {
        (IconName::GitBranch, "Worktree")
    } else {
        (IconName::Folder, "Local")
    };
    h_flex()
        .id("where-chip")
        .flex_none()
        .gap_1()
        .items_center()
        .text_sm()
        .text_color(colors.muted_foreground)
        .debug_selector(|| "where-chip".to_string())
        .child(ui::icon(
            glyph,
            gpui_kit::component::Size::Small,
            colors.muted_foreground,
        ))
        .child(
            div()
                .when(copy, |label| {
                    label.debug_selector(|| "worktree-tag".to_string())
                })
                .child(label),
        )
        .when(!path.is_empty(), |chip| {
            chip.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
        })
        .into_any_element()
}

/// The terminal button, lit while the panel is out.
fn terminal_button(toggle: TerminalToggle, cx: &App) -> AnyElement {
    let on_press = toggle.on_press;
    ui::icon_button(
        "terminal-toggle",
        IconName::Terminal,
        if toggle.shown {
            "Hide terminal"
        } else {
            "Show terminal"
        },
        cx,
    )
    .selected(toggle.shown)
    .debug_selector(|| "terminal-toggle".to_string())
    .on_click(move |event, window, cx| on_press(event, window, cx))
    .into_any_element()
}

/// The git-compare button at the right end of the bar, lit while the Changes card is out.
fn changes_button(toggle: ChangesToggle, cx: &App) -> AnyElement {
    let on_press = toggle.on_press;
    ui::icon_button(
        "changes-toggle",
        IconName::GitCompareArrows,
        toggle.tooltip,
        cx,
    )
    .selected(toggle.shown)
    .debug_selector(|| "changes-toggle".to_string())
    .on_click(move |event, window, cx| on_press(event, window, cx))
    .into_any_element()
}

/// The panel-left button, with an amber dot while a hidden thread needs an approval and a blue
/// one while an update waits.
fn show_list_button(on_press: OnPress, badge: sidebar::ListBadge, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let dot = if badge.attention {
        Some(colors.warning)
    } else {
        badge.update.then_some(colors.info)
    };
    let tooltip = if badge.update {
        "Show sidebar · Update ready"
    } else {
        "Show sidebar"
    };
    div()
        .relative()
        .occlude()
        .child(sidebar::toggle_button(
            "sidebar-show",
            tooltip,
            on_press,
            cx,
        ))
        .when_some(dot, |button, dot| {
            button.child(
                div()
                    .debug_selector(|| "sidebar-dot".to_string())
                    .absolute()
                    .top(px(3.0))
                    .right(px(3.0))
                    .size(px(7.0))
                    .rounded_full()
                    .bg(dot)
                    .border_1()
                    .border_color(colors.background),
            )
        })
        .into_any_element()
}

/// A shield with the count of rules allowed in this thread: opens the list of them. Tab stays
/// inside it; Esc, a press outside or focus moving elsewhere closes it.
fn grants_popover(
    grants: Vec<GrantView>,
    detail: &'static str,
    background_tasks: usize,
    state: Grants,
) -> AnyElement {
    let label = grants.len().to_string();
    let pointer = state.pointer;
    let on_open = state.on_open;
    let on_revoke = state.on_revoke;
    let focus = state.focus.clone();
    Popover::new("grants")
        .anchor(Anchor::TopLeft)
        .trigger(GrantsChip { label })
        .overlay_closable(false)
        .appearance(false)
        .open(state.open)
        .on_open_change(move |open, window, cx| on_open(*open, window, cx))
        .track_focus(&state.focus)
        .content(move |_, window, cx| {
            let dismiss = cx.listener(|state, _, window, cx| state.dismiss(window, cx));
            let panel = grants_panel(&grants, detail, background_tasks, &on_revoke, window, cx);
            let inner = div()
                .child(panel)
                .on_mouse_down_out(dismiss)
                .focus_trap("grants-trap", &focus);
            ui::popover_surface("grants-enter", div().p_3().child(inner), pointer, cx)
        })
        .into_any_element()
}

/// The shield button that opens the popover.
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
            .label(self.label.clone())
            .icon(IconName::Shield)
            .ghost()
            .tooltip("Allowed in this thread")
            .accessibility_label(format!("{} allowed in this thread", self.label))
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
                .child("Allowed in this thread"),
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
                path: "~/work/verify-project".into(),
                copy: true,
                show_list: Some((Rc::new(|_, _| {}), sidebar::ListBadge::default())),
                grants: vec![],
                grants_detail: "Claude runs these without asking, until you revoke them.",
                background_tasks: 0,
                terminal: TerminalToggle {
                    shown: false,
                    on_press: Rc::new(|_, _, _| {}),
                },
                changes: None,
            };
            let grants = Grants {
                open: false,
                focus: self.focus.clone(),
                on_open: Rc::new(|_, _, _| {}),
                on_revoke: Rc::new(|_, _, _| {}),
                pointer: false,
            };
            div().w_full().child(render(bar, grants, window, cx))
        }
    }

    #[gpui::test]
    fn a_long_title_gives_way_to_the_where_chip_and_buttons(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, cx| Narrow {
            focus: cx.focus_handle(),
        });
        cx.simulate_resize(gpui::size(px(640.), px(480.)));
        cx.run_until_parked();
        let right = cx.debug_bounds("title-bar-right").unwrap();
        assert!(right.right() <= px(640.), "{right:?}");
        let chip = cx.debug_bounds("where-chip").unwrap();
        assert!(
            chip.size.width > px(40.),
            "the chip keeps its width: {chip:?}"
        );
        assert!(chip.right() <= right.left(), "{chip:?} {right:?}");
        assert!(cx.debug_bounds("worktree-tag").is_some());
        assert!(cx.debug_bounds("terminal-toggle").is_some());
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
