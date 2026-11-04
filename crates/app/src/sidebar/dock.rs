//! The dock below the scrolling list: the In flight tray, the Snoozed and Settled rows, and the
//! lifted panel one of them opens.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ElementId, Entity, FontWeight, Hsla, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, SharedString, Styled, Window, canvas, div, hsla,
    prelude::*, px, rgba,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _};

use super::{Counted, Edge, Key, Look, OnOpen, OnPress, faint, muted, number, pulse, slot};
use crate::ui::{self, IconName};

/// Rows the tray shows before a quiet "+N more".
pub const TRAY_ROWS: usize = 3;

/// The height of a tray row and of the Snoozed and Settled rows.
pub const ROW: Pixels = px(30.);

/// The room under the last dock row.
const FOOT: Pixels = px(4.);

/// The gap between a panel and the row that opened it.
const LIFT: Pixels = px(4.);

/// The height of the In flight header.
pub const TRAY_HEADER: Pixels = px(30.);

/// The height of the rule above the Snoozed and Settled rows, with its margins.
pub const SEP: Pixels = px(13.);

/// A panel row.
const PANEL_ROW: Pixels = px(32.);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SheetKind {
    Snoozed,
    Settled,
}

impl SheetKind {
    pub fn filter_hint(self) -> &'static str {
        match self {
            Self::Snoozed => "Filter snoozed…",
            Self::Settled => "Filter settled…",
        }
    }

    pub fn bulk_label(self) -> &'static str {
        match self {
            Self::Snoozed => "Wake all",
            Self::Settled => "Archive all",
        }
    }
}

/// What the right of a tray row says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clock {
    /// How long the turn has run: "1m 12s".
    Elapsed(String),
    /// A usage limit holds the thread back until it resets.
    Waiting,
    /// Nothing to count: a worktree being made, a command running in the background.
    Quiet,
}

impl Clock {
    pub fn label(&self) -> &str {
        match self {
            Self::Elapsed(text) => text,
            Self::Waiting => "waiting",
            Self::Quiet => "",
        }
    }
}

#[derive(Clone)]
pub struct TrayRow {
    pub id: SharedString,
    pub title: SharedString,
    pub clock: Clock,
    pub selected: bool,
    pub on_press: OnPress,
}

/// The Snoozed or Settled row while it shows: its count, and how far it has grown or faded.
#[derive(Clone, Copy)]
pub struct Fold {
    pub count: Counted,
    pub look: Look,
}

/// A tray row, and how far it has grown or faded.
pub struct TrayItem {
    pub row: TrayRow,
    pub look: Look,
}

/// The In flight header while it shows.
#[derive(Clone, Copy)]
pub struct Flight {
    pub count: Counted,
    pub look: Look,
}

pub struct Dock {
    pub flight: Option<Flight>,
    /// The tray's rows, the first [`TRAY_ROWS`] of the threads in flight, leaving ones too.
    pub tray: Vec<TrayItem>,
    /// Every thread in flight.
    pub tray_total: usize,
    /// The rule above the Snoozed and Settled rows, while it shows.
    pub sep: Option<Look>,
    pub snoozed: Option<Fold>,
    pub settled: Option<Fold>,
    pub open: Option<SheetKind>,
    pub on_open: OnOpen,
    pub panel: Option<Panel>,
    /// 0 to 1: how far everything but the row that opened the panel has dimmed.
    pub dim: f32,
    /// Where the dock was drawn last, for telling a click on it from a click outside the panel.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
}

pub struct PanelRow {
    pub id: SharedString,
    pub title: SharedString,
    pub when: String,
    pub on_press: OnPress,
}

pub struct PanelGroup {
    pub label: &'static str,
    pub rows: Vec<PanelRow>,
}

pub struct Panel {
    pub kind: SheetKind,
    pub filter: Entity<InputState>,
    pub groups: Vec<PanelGroup>,
    /// "Archive N threads?" is asked: the number of threads.
    pub confirming: Option<usize>,
    pub on_bulk: OnPress,
    pub on_cancel: OnPress,
    pub on_confirm: OnPress,
    pub on_dismiss: OnPress,
    pub max_height: Pixels,
    /// 0 to 1: how far up it has come. It stays drawn while it lowers.
    pub lift: f32,
    /// It can be clicked: it is open, not lowering.
    pub live: bool,
    /// How tall it is with all its rows, as of the last draw: the height it grows to.
    pub measured: Rc<Cell<Pixels>>,
}

/// "+2 more", under the rows the tray shows.
pub fn more_label(total: usize) -> Option<String> {
    (total > TRAY_ROWS).then(|| format!("+{} more", total - TRAY_ROWS))
}

/// "Archive 3 threads? Their copies are removed for good."
pub fn archive_question(count: usize) -> String {
    format!(
        "Archive {count} {}? Their copies are removed for good.",
        if count == 1 { "thread" } else { "threads" }
    )
}

fn separator(cx: &App) -> AnyElement {
    div()
        .h(px(1.))
        .mx_2()
        .my_1p5()
        .bg(crate::theme::material(cx).glass_muted.opacity(0.1))
        .into_any_element()
}

fn tray_header(count: Counted, cx: &App) -> AnyElement {
    let blue = cx.theme().colors.info;
    let dot = |phase: f32| {
        pulse(
            div().size(px(4.)).rounded_full().bg(blue),
            ("tray-dot", (phase * 10.) as usize),
            phase,
            cx,
        )
    };
    h_flex()
        .h(TRAY_HEADER)
        .items_center()
        .gap_2()
        .px_2()
        .pt_2()
        .pb_1()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(faint(cx))
        .child("In flight")
        .child(number(count, Edge::Left))
        .child(div().flex_1())
        .child(
            h_flex()
                .gap(px(3.))
                .child(dot(0.))
                .child(dot(0.2))
                .child(dot(0.4)),
        )
        .into_any_element()
}

fn tray_row(row: &TrayRow, dim: f32, cx: &App) -> AnyElement {
    let material = crate::theme::material(cx);
    let blue = cx.theme().colors.info;
    let waiting = row.clock == Clock::Waiting;
    let mark = div().flex_none().size(px(6.)).rounded_full();
    let mark = if waiting {
        mark.border(px(1.5))
            .border_color(faint(cx))
            .into_any_element()
    } else {
        pulse(
            mark.bg(blue),
            (ElementId::from("tray-mark"), row.id.clone()),
            0.,
            cx,
        )
    };
    let on_press = row.on_press.clone();
    div()
        .id((ElementId::from("tray-row"), row.id.clone()))
        .debug_selector(|| "tray-row".to_string())
        .flex_none()
        .h(ROW)
        .flex()
        .items_center()
        .gap(px(7.))
        .pl_2p5()
        .pr_2()
        .rounded(px(6.))
        .cursor_pointer()
        .when(row.selected, |row| row.bg(material.glass_selected))
        .hover(move |row| row.bg(material.glass_hover))
        .when(dim > 0., |row| row.opacity(1. - 0.5 * dim))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| on_press(window, cx))
        .child(div().flex_none().pr(px(3.)).child(mark))
        .child(
            ui::ellipsis(row.title.clone())
                .flex_1()
                .text_size(px(12.5))
                .text_color(cx.theme().sidebar_foreground.opacity(0.86)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(faint(cx))
                .child(row.clock.label().to_string()),
        )
        .into_any_element()
}

fn disclosure(
    kind: SheetKind,
    count: Counted,
    open: Option<SheetKind>,
    dim: f32,
    on_open: &OnOpen,
    cx: &App,
) -> AnyElement {
    let material = crate::theme::material(cx);
    let is_open = open == Some(kind);
    let label = match kind {
        SheetKind::Snoozed => "Snoozed",
        SheetKind::Settled => "Settled",
    };
    let on_open = on_open.clone();
    let color = if is_open {
        cx.theme().sidebar_foreground
    } else {
        muted(cx)
    };
    div()
        .id((ElementId::from("disclosure"), SharedString::from(label)))
        .debug_selector(|| format!("disclosure-{}", label.to_lowercase()))
        .flex_none()
        .h(ROW)
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .rounded(px(6.))
        .cursor_pointer()
        .text_size(px(12.5))
        .text_color(color)
        .when(is_open, |row| row.bg(material.glass_selected))
        .when(!is_open, |row| {
            row.hover(move |row| row.bg(material.glass_hover))
        })
        .when(open.is_some() && !is_open && dim > 0., |row| {
            row.opacity(1. - 0.5 * dim)
        })
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| on_open(kind, window, cx))
        .child(ui::icon(
            if is_open {
                IconName::ChevronDown
            } else {
                IconName::ChevronUp
            },
            gpui_kit::component::Size::XSmall,
            color,
        ))
        .child(label)
        .child(div().flex_1())
        .child(
            div()
                .text_size(px(11.5))
                .text_color(faint(cx))
                .child(number(count, Edge::Right)),
        )
        .into_any_element()
}

/// The dock: In flight, then Snoozed and Settled, each only while it has threads. With a panel
/// open, everything but the row that opened it dims to half.
pub fn dock(dock: Dock, window: &Window, cx: &App) -> Option<AnyElement> {
    let Dock {
        flight,
        tray,
        tray_total,
        sep,
        snoozed,
        settled,
        open,
        on_open,
        panel,
        dim,
        bounds,
    } = dock;
    if flight.is_none() && tray.is_empty() && snoozed.is_none() && settled.is_none() {
        return None;
    }
    let rows_below = |kind: SheetKind| match kind {
        SheetKind::Snoozed => 1. + settled.map_or(0., |fold| fold.look.size),
        SheetKind::Settled => 1.,
    };
    let record = bounds.clone();
    let mut root = v_flex().id("dock").relative().flex_none().pt_0p5().pb(FOOT);
    if let Some(flight) = flight {
        root = root.child(slot(
            &Key::Flight,
            flight.look,
            tray_header(flight.count, cx),
        ));
    }
    for item in &tray {
        root = root.child(slot(
            &Key::Tray(item.row.id.clone()),
            item.look,
            tray_row(&item.row, dim, cx),
        ));
    }
    if let Some(more) = more_label(tray_total) {
        root = root.child(
            div()
                .px_2p5()
                .pt_1()
                .pb_0p5()
                .text_size(px(11.5))
                .text_color(faint(cx))
                .child(more),
        );
    }
    if let Some(look) = sep {
        root = root.child(slot(&Key::Sep, look, separator(cx)));
    }
    for (kind, fold) in [(SheetKind::Snoozed, snoozed), (SheetKind::Settled, settled)] {
        if let Some(fold) = fold {
            root = root.child(slot(
                &Key::Disclosure(kind),
                fold.look,
                disclosure(kind, fold.count, open, dim, &on_open, cx),
            ));
        }
    }
    root = root.child(
        canvas(|_, _, _| {}, move |area, _, _, _| record.set(area))
            .absolute()
            .inset_0(),
    );
    if let Some(panel) = panel {
        let bottom = FOOT + ROW * rows_below(panel.kind) + LIFT;
        root = root.child(sheet(panel, bottom, bounds, window, cx));
    }
    Some(root.into_any_element())
}

/// The panel's fill: solid, so the rows behind it never show through.
fn sheet_fill(dark: bool, card: Hsla) -> Hsla {
    if dark { rgba(0x26262cff).into() } else { card }
}

fn sheet(
    panel: Panel,
    bottom: Pixels,
    dock: Rc<Cell<Bounds<Pixels>>>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let dark = cx.theme().is_dark();
    let fill = sheet_fill(dark, crate::theme::material(cx).card);
    let edge: Hsla = if dark {
        hsla(0., 0., 1., 0.09)
    } else {
        hsla(0., 0., 0., 0.08)
    };
    let on_dismiss = panel.on_dismiss.clone();
    let bar = match panel.confirming {
        Some(count) => confirm_bar(count, &panel, cx),
        None => filter_bar(&panel, cx),
    };
    let body = v_flex()
        .id("sheet-body")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_1()
        .pb(px(6.))
        .children(panel.groups.iter().map(|group| group_block(group, cx)))
        .when(panel.groups.is_empty(), |body| {
            body.child(
                div()
                    .px_2()
                    .py_3()
                    .text_size(px(12.5))
                    .text_color(faint(cx))
                    .child("No matches"),
            )
        });
    let _ = window;
    let level = panel.lift;
    let measured = panel.measured.clone();
    let content = v_flex().relative().min_h_0().child(bar).child(body).child(
        canvas(
            |_, _, _| {},
            move |area, _, _, _| measured.set(area.size.height),
        )
        .absolute()
        .inset_0(),
    );
    let reduce = cx.reduce_motion();
    let sheet = div()
        .id("sheet")
        .debug_selector(|| "sheet".to_string())
        .absolute()
        .left(px(-2.))
        .right(px(-2.))
        .bottom(bottom)
        .rounded(px(12.))
        .overflow_hidden()
        .bg(fill)
        .shadow(vec![
            gpui::BoxShadow {
                color: hsla(0., 0., 0., 0.5 * level),
                offset: gpui::point(px(0.), px(-16.)),
                blur_radius: px(40.),
                spread_radius: px(0.),
                inset: false,
            },
            gpui::BoxShadow {
                color: edge,
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(1.),
                inset: true,
            },
            gpui::BoxShadow {
                color: hsla(0., 0., 1., if dark { 0.06 } else { 0. }),
                offset: gpui::point(px(0.), px(1.)),
                blur_radius: px(0.),
                spread_radius: px(0.),
                inset: true,
            },
        ]);
    let sheet = if level < 1. && !reduce {
        let known = panel.measured.get();
        let natural = if known > px(0.) {
            known
        } else {
            panel.max_height
        };
        sheet.h(natural * level).child(
            content
                .absolute()
                .bottom_0()
                .left_0()
                .right_0()
                .max_h(panel.max_height),
        )
    } else {
        sheet
            .max_h(panel.max_height)
            .flex()
            .flex_col()
            .when(level < 1., |sheet| sheet.opacity(level))
            .child(content)
    };
    if !panel.live {
        return sheet.into_any_element();
    }
    sheet
        .occlude()
        .on_mouse_down_out(move |event: &MouseDownEvent, window, cx| {
            if !dock.get().contains(&event.position) {
                on_dismiss(window, cx);
            }
        })
        .into_any_element()
}

fn filter_bar(panel: &Panel, cx: &App) -> AnyElement {
    let on_bulk = panel.on_bulk.clone();
    let enabled = panel.groups.iter().any(|group| !group.rows.is_empty());
    h_flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .pt_2()
        .pb_1()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(Input::new(&panel.filter).small().prefix(ui::icon(
                    IconName::Search,
                    gpui_kit::component::Size::XSmall,
                    faint(cx),
                ))),
        )
        .child(
            div()
                .id("sheet-bulk")
                .debug_selector(|| "sheet-bulk".to_string())
                .flex_none()
                .px_1()
                .text_size(px(11.5))
                .text_color(muted(cx))
                .when(enabled, |label| {
                    label
                        .cursor_pointer()
                        .hover(|label| label.text_color(cx.theme().sidebar_foreground))
                        .on_click(move |_, window, cx| on_bulk(window, cx))
                })
                .when(!enabled, |label| label.opacity(0.4))
                .child(panel.kind.bulk_label()),
        )
        .into_any_element()
}

fn confirm_bar(count: usize, panel: &Panel, cx: &App) -> AnyElement {
    let on_cancel = panel.on_cancel.clone();
    let on_confirm = panel.on_confirm.clone();
    let pill = |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .flex_none()
            .h(px(24.))
            .px(px(9.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(11.5))
            .child(label)
    };
    h_flex()
        .flex_none()
        .items_center()
        .gap_2()
        .px_2()
        .pt_2()
        .pb_1()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pl_0p5()
                .text_size(px(11.5))
                .line_height(px(15.))
                .text_color(muted(cx))
                .child(archive_question(count)),
        )
        .child(
            pill("sheet-cancel", "Cancel")
                .bg(hsla(0., 0., 1., 0.08))
                .on_click(move |_, window, cx| on_cancel(window, cx)),
        )
        .child(
            pill("sheet-archive", "Archive")
                .bg(hsla(0., 0.84, 0.6, 0.22))
                .text_color(hsla(0., 1., 0.85, 1.))
                .on_click(move |_, window, cx| on_confirm(window, cx)),
        )
        .into_any_element()
}

fn group_block(group: &PanelGroup, cx: &App) -> AnyElement {
    v_flex()
        .child(
            div()
                .px_2()
                .pt(px(9.))
                .pb(px(3.))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(faint(cx))
                .child(group.label.to_uppercase()),
        )
        .children(group.rows.iter().map(|row| panel_row(row, cx)))
        .into_any_element()
}

fn panel_row(row: &PanelRow, cx: &App) -> AnyElement {
    let on_press = row.on_press.clone();
    let id = (ElementId::from("sheet-row"), row.id.clone());
    div()
        .id(id)
        .debug_selector(|| "sheet-row".to_string())
        .flex_none()
        .h(PANEL_ROW)
        .flex()
        .items_center()
        .gap(px(9.))
        .px_2()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(|row| row.bg(hsla(0., 0., 1., 0.06)))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(move |_, window, cx| on_press(window, cx))
        .child(
            div()
                .flex_none()
                .size(px(7.))
                .rounded_full()
                .border(px(1.5))
                .border_color(faint(cx)),
        )
        .child(
            ui::ellipsis(row.title.clone())
                .flex_1()
                .text_size(px(12.5))
                .text_color(cx.theme().sidebar_foreground.opacity(0.86)),
        )
        .child(
            div()
                .flex_none()
                .text_size(px(11.5))
                .text_color(faint(cx))
                .child(row.when.clone()),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_is_opaque_in_both_appearances() {
        assert_eq!(sheet_fill(true, hsla(0., 0., 1., 1.)).a, 1.);
        assert_eq!(sheet_fill(false, hsla(0., 0., 1., 1.)).a, 1.);
    }

    #[test]
    fn the_tray_shows_three_rows_and_counts_the_rest() {
        assert_eq!(more_label(0), None);
        assert_eq!(more_label(3), None);
        assert_eq!(more_label(4).as_deref(), Some("+1 more"));
        assert_eq!(more_label(9).as_deref(), Some("+6 more"));
    }

    #[test]
    fn a_limit_shows_waiting_and_a_turn_its_clock() {
        assert_eq!(Clock::Waiting.label(), "waiting");
        assert_eq!(Clock::Elapsed("1m 12s".into()).label(), "1m 12s");
        assert_eq!(Clock::Quiet.label(), "");
    }

    #[test]
    fn the_archive_question_counts_threads() {
        assert_eq!(
            archive_question(28),
            "Archive 28 threads? Their copies are removed for good."
        );
        assert_eq!(
            archive_question(1),
            "Archive 1 thread? Their copies are removed for good."
        );
    }
}
