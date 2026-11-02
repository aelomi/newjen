//! The Changes card: a floating panel right of the chat that lists every file the thread
//! touched, and the file rows the review pane reuses.
//!
//! While a turn runs the numbers are an estimate from the agent's edit tool calls (it never
//! touches the folder), grown one finished call at a time and counted off the UI thread. Once
//! the turn is done and snapshotted they come from one read-only jj call, also off the UI
//! thread. A failed call keeps the estimate, marked as one, and is asked again a few times.

use super::*;
use std::collections::HashMap;

use gpui::{HighlightStyle, StyledText};
use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::component::diff::{DiffFile, DiffFileStatus};
use gpui_kit::component::tooltip::Tooltip;
use newjen_core::changes::{ChangeKind, ChangeSummary, Estimate, FileStat, edit_stats};
use newjen_core::landing;

use crate::title_bar::ChangesToggle;

pub(super) const CARD_WIDTH: f32 = 280.0;
/// The chat keeps at least this much width beside the card; with less the card stays away.
const MAIN_MIN_WIDTH: f32 = 520.0;
const FADE: Duration = Duration::from_millis(150);
/// A toggle counts as a mouse toggle for this long: the fade it starts must not outlast it.
const TOGGLE_WINDOW: Duration = Duration::from_millis(400);

#[derive(Default)]
pub(super) struct ChangesCard {
    threads: HashMap<String, Tracked>,
    /// The file a card row was clicked on, until the review opens on it.
    pending_file: Option<String>,
    /// When the card was last toggled with the mouse; keys and other changes never fade.
    toggled_at: Option<Instant>,
    /// The card is on screen, so the review toolbar need not offer its own button.
    pub(super) on_screen: bool,
    /// The thread on screen has changes to review.
    pub(super) has_changes: bool,
}

/// jj is asked again this many times for a transcript length it failed on.
const ASKS: u8 = 3;
const RETRY: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Tracked {
    estimate: Estimate,
    /// The count of finished tool calls the estimate was last brought up to.
    tools_done: Option<usize>,
    estimating: bool,
    /// jj's answer and the transcript length it was asked at.
    exact: Option<(usize, ChangeSummary)>,
    asked: Option<usize>,
    /// The transcript length jj last failed at, and how many times in a row.
    failed: Option<(usize, u8)>,
    loading: bool,
}

/// Whether the user wants the card (a window setting, so every project of the window agrees).
#[derive(Default)]
struct Shown(HashMap<gpui::WindowId, bool>);

impl gpui::Global for Shown {}

fn wanted(window: &Window, cx: &App) -> bool {
    cx.try_global::<Shown>()
        .and_then(|shown| shown.0.get(&window.window_handle().window_id()))
        .copied()
        .unwrap_or(true)
}

fn set_wanted(window: &Window, shown: bool, cx: &mut App) {
    let id = window.window_handle().window_id();
    cx.default_global::<Shown>().0.insert(id, shown);
}

/// One file as a row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileRow {
    pub path: String,
    pub name: String,
    pub dir: String,
    pub added: usize,
    pub removed: usize,
    pub kind: ChangeKind,
    pub binary: bool,
}

impl FileRow {
    fn new(path: &str, kind: ChangeKind, added: usize, removed: usize, binary: bool) -> Self {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        Self {
            path: path.to_string(),
            name: name.to_string(),
            dir: dir.to_string(),
            added,
            removed,
            kind,
            binary,
        }
    }

    pub(super) fn from_stat(stat: &FileStat) -> Self {
        Self::new(&stat.path, stat.kind, stat.added, stat.removed, stat.binary)
    }

    pub(super) fn from_diff(file: &DiffFile) -> Self {
        let kind = match file.status() {
            DiffFileStatus::Added => ChangeKind::Added,
            DiffFileStatus::Deleted => ChangeKind::Deleted,
            DiffFileStatus::Renamed | DiffFileStatus::Copied => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        Self::new(
            file.path(),
            kind,
            file.additions(),
            file.deletions(),
            file.is_binary(),
        )
    }

    fn icon(&self) -> ui::IconName {
        match self.kind {
            ChangeKind::Added => ui::IconName::FilePlus,
            ChangeKind::Deleted => ui::IconName::FileMinus,
            _ => {
                let code = self.name.rsplit_once('.').is_some_and(|(_, ext)| {
                    matches!(
                        ext,
                        "rs" | "ts"
                            | "tsx"
                            | "js"
                            | "jsx"
                            | "py"
                            | "go"
                            | "c"
                            | "h"
                            | "cpp"
                            | "swift"
                            | "kt"
                            | "java"
                            | "rb"
                            | "sh"
                    )
                });
                if code {
                    ui::IconName::FileCode
                } else {
                    ui::IconName::FileText
                }
            }
        }
    }
}

/// The files and lines a list of rows adds up to.
pub(super) fn totals(rows: &[FileRow]) -> (usize, usize) {
    (
        rows.iter().map(|row| row.added).sum(),
        rows.iter().map(|row| row.removed).sum(),
    )
}

/// `12427` as `12,427`.
pub(super) fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub(super) fn files_label(count: usize) -> String {
    if count == 1 {
        "1 file".to_string()
    } else {
        format!("{} files", thousands(count))
    }
}

/// "+161" and "−4" in the theme's green and red.
pub(super) fn counts(added: usize, removed: usize, binary: bool, cx: &App) -> AnyElement {
    let colors = &cx.theme().colors;
    let theme = cx.theme();
    h_flex()
        .flex_none()
        .gap_1p5()
        .text_sm()
        .when(binary, |row| {
            row.child(div().text_color(colors.muted_foreground).child("binary"))
        })
        .when(!binary && added > 0, |row| {
            row.child(
                div()
                    .text_color(theme.success)
                    .child(format!("+{}", thousands(added))),
            )
        })
        .when(!binary && removed > 0, |row| {
            row.child(
                div()
                    .text_color(theme.danger)
                    .child(format!("\u{2212}{}", thousands(removed))),
            )
        })
        .into_any_element()
}

/// The card's totals; an estimate gets a muted "≈" before them and says why on hover.
fn totals_label(added: usize, removed: usize, exact: bool, cx: &App) -> AnyElement {
    let counted = counts(added, removed, false, cx);
    if exact {
        return counted;
    }
    h_flex()
        .id("changes-estimate")
        .flex_none()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().colors.muted_foreground)
                .child("\u{2248}"),
        )
        .child(counted)
        .tooltip(|window, cx| {
            Tooltip::new("Estimated from the agent's edits; exact after the turn").build(window, cx)
        })
        .into_any_element()
}

/// A file's row: icon, name, the quiet folder, and the counts at the right edge.
pub(super) fn file_row(
    id: impl Into<ElementId>,
    row: &FileRow,
    selected: bool,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let mut text = row.name.clone();
    let mut highlights = Vec::new();
    if !row.dir.is_empty() {
        text.push_str("  ");
        let start = text.len();
        text.push_str(&row.dir);
        highlights.push((
            start..text.len(),
            HighlightStyle {
                color: Some(muted),
                ..Default::default()
            },
        ));
    }
    let path = SharedString::from(row.path.clone());
    h_flex()
        .id(id)
        .w_full()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .cursor_pointer()
        .text_sm()
        .when(selected, |row| row.bg(theme.list_active))
        .when(!selected, |row| row.hover(|row| row.bg(theme.list_hover)))
        .tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
        .child(ui::icon(row.icon(), gpui_kit::component::Size::Small, muted).flex_none())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(StyledText::new(text).with_highlights(highlights)),
        )
        .child(counts(row.added, row.removed, row.binary, cx))
}

impl ChangesCard {
    /// What the card lists for `thread`: jj's answer while it is current, the estimate
    /// otherwise. Nothing once the work has landed.
    /// The second value is false for an estimate.
    fn summary(&self, thread: &Thread) -> Option<(&ChangeSummary, bool)> {
        if thread.landing_finished() {
            return None;
        }
        let tracked = self.threads.get(thread.id())?;
        let length = thread.transcript().items().len();
        let (summary, exact) = match &tracked.exact {
            Some((asked, exact)) if *asked == length => (exact, true),
            _ => (tracked.estimate.summary(), false),
        };
        (!summary.is_empty()).then_some((summary, exact))
    }
}

impl ChatView {
    /// Keeps the active thread's numbers current: the estimate when a tool call finishes, and
    /// jj's answer once the turn has ended and been snapshotted.
    pub(super) fn sync_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entity = self.thread().clone();
        let thread = entity.read(cx);
        let id = thread.id().to_string();
        let items = thread.transcript().items();
        let length = items.len();
        let done = items
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    Item::Tool {
                        state: ToolState::Done,
                        ..
                    }
                )
            })
            .count();
        let tracked = self.changes.threads.entry(id.clone()).or_default();
        if tracked.tools_done != Some(done) && !tracked.estimating {
            tracked.tools_done = Some(done);
            let fresh = tracked.estimate.unseen(items);
            if !fresh.is_empty() {
                tracked.estimating = true;
                let root = thread.cwd().to_path_buf();
                let counted = cx.background_spawn(async move { edit_stats(&fresh, &root) });
                let id = id.clone();
                cx.spawn_in(window, async move |view, cx| {
                    let counted = counted.await;
                    let _ = view.update_in(cx, |view, _, cx| {
                        let tracked = view.changes.threads.entry(id).or_default();
                        tracked.estimate.add(counted);
                        tracked.estimating = false;
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        let quiet = !is_busy(thread.status())
            && thread.preparing().is_none()
            && !thread.work_pending()
            && thread.background_tasks().is_empty()
            && !thread.landing_finished();
        let Some(base) = thread.review_base().map(str::to_string) else {
            return;
        };
        if !quiet || tracked.loading || tracked.asked == Some(length) {
            return;
        }
        tracked.asked = Some(length);
        tracked.loading = true;
        let cwd = thread.cwd().to_path_buf();
        let tools = self.tools.clone();
        let task = cx.background_spawn(async move { landing::changed_files(&tools, &cwd, &base) });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let mut retry = false;
            let _ = view.update_in(cx, |view, _, cx| {
                let tracked = view.changes.threads.entry(id.clone()).or_default();
                tracked.loading = false;
                match result {
                    Ok(summary) => {
                        tracked.exact = Some((length, summary));
                        tracked.failed = None;
                    }
                    Err(_) => {
                        let tries = match tracked.failed {
                            Some((at, tries)) if at == length => tries + 1,
                            _ => 1,
                        };
                        tracked.failed = Some((length, tries));
                        retry = tries < ASKS;
                    }
                }
                cx.notify();
            });
            if retry {
                cx.background_executor().timer(RETRY).await;
                let _ = view.update_in(cx, |view, _, cx| {
                    let tracked = view.changes.threads.entry(id).or_default();
                    if tracked.asked == Some(length) {
                        tracked.asked = None;
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Whether the thread on screen has changes to show, and whether the card fits beside
    /// the chat in a window `width` pixels wide with the list docked.
    fn changes_facts(&self, placement: Placement, width: f32, cx: &App) -> (bool, bool) {
        let has =
            self.changes.summary(self.thread().read(cx)).is_some() && self.review_panel.is_none();
        let room = width - f32::from(SIDEBAR) - 2.0 * f32::from(CARD_INSET) - CARD_WIDTH;
        (
            has,
            placement == Placement::Docked && room >= MAIN_MIN_WIDTH,
        )
    }

    /// The header button that shows and hides the card; where the card cannot fit it opens
    /// the review instead. `None` while there is nothing to show.
    pub(super) fn changes_toggle(
        &self,
        placement: Placement,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<ChangesToggle> {
        let (has, fits) = self.changes_facts(placement, width, cx);
        if !has {
            return None;
        }
        let view = cx.weak_entity();
        if !fits {
            return Some(ChangesToggle {
                shown: false,
                tooltip: "Review changes",
                on_press: Rc::new(move |_, window, cx| {
                    let _ = view.update(cx, |view, cx| view.open_review(window, cx));
                }),
            });
        }
        let shown = wanted(window, cx);
        Some(ChangesToggle {
            shown,
            tooltip: if shown {
                "Hide changes"
            } else {
                "Show changes"
            },
            on_press: Rc::new(move |event, window, cx| {
                let by_mouse = !event.is_keyboard();
                let _ = view.update(cx, |view, cx| {
                    view.changes.toggled_at = by_mouse.then(Instant::now);
                    set_wanted(window, !wanted(window, cx), cx);
                    cx.notify();
                });
            }),
        })
    }

    /// The card, if it is to be on screen, and the width it takes from the chat.
    pub(super) fn render_changes_card(
        &mut self,
        placement: Placement,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        let (has, fits) = self.changes_facts(placement, width, cx);
        self.changes.on_screen = false;
        self.changes.has_changes = has;
        if !has || !fits {
            return None;
        }
        let thread = self.thread().read(cx);
        let (summary, exact) = self.changes.summary(thread)?;
        let summary = summary.clone();
        let working = is_busy(thread.status()) || thread.preparing().is_some();
        let fade = self
            .changes
            .toggled_at
            .is_some_and(|at| at.elapsed() < TOGGLE_WINDOW);
        let sample = Presence::new("changes-card", wanted(window, cx))
            .transition(Transition::new(if fade { FADE } else { Duration::ZERO }))
            .sample(window, cx);
        if !sample.should_render() {
            return None;
        }
        self.changes.on_screen = true;
        let card = self
            .changes_card(&summary, exact, working, cx)
            .opacity(sample.progress);
        Some((card.into_any_element(), CARD_WIDTH + f32::from(CARD_INSET)))
    }

    fn changes_card(
        &self,
        summary: &ChangeSummary,
        exact: bool,
        working: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let colors = cx.theme().colors;
        let rows: Vec<FileRow> = summary.files.iter().map(FileRow::from_stat).collect();
        let list = v_flex()
            .id("changes-files")
            .flex_1()
            .min_h_0()
            .gap_0p5()
            .px_1p5()
            .overflow_y_scroll()
            .children(rows.iter().enumerate().map(|(ix, row)| {
                let path = row.path.clone();
                file_row(("changes-file", ix), row, false, cx).on_click(cx.listener(
                    move |view, _, window, cx| view.open_review_at(Some(path.clone()), window, cx),
                ))
            }));
        let review = ui::button(
            "changes-review",
            if self.review_loading {
                "Loading diff…"
            } else {
                "Review"
            },
            if working {
                ui::Look::Outline
            } else {
                ui::Look::Primary
            },
            ui::Size::Md,
            cx,
        )
        .w_full()
        .on_click(cx.listener(|view, _, window, cx| view.open_review(window, cx)));
        let (added, removed) = totals(&rows);
        ui::card(v_flex(), cx)
            .id((
                ElementId::from("changes"),
                SharedString::from(self.thread().entity_id().to_string()),
            ))
            .flex_none()
            .w(px(CARD_WIDTH))
            .h_full()
            .child(
                h_flex()
                    .flex_none()
                    .h(TITLE_BAR_HEIGHT)
                    .px_3()
                    .gap_2()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Changes"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(colors.muted_foreground)
                            .child(files_label(rows.len())),
                    )
                    .child(totals_label(added, removed, exact, cx)),
            )
            .child(list)
            .child(div().flex_none().p_2().child(review))
    }

    /// A card row's click: the review, opened on that file.
    pub(super) fn open_review_at(
        &mut self,
        file: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let idle = !self.review_loading;
        self.changes.pending_file = file;
        self.open_review(window, cx);
        if !(idle && self.review_loading) {
            self.changes.pending_file = None;
        }
    }

    /// The file the review should open on, once, if a card row asked for one.
    pub(super) fn take_pending_file(&mut self) -> Option<String> {
        self.changes.pending_file.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
diff --git a/src/config/parser.rs b/src/config/parser.rs
index 1111111..2222222 100644
--- a/src/config/parser.rs
+++ b/src/config/parser.rs
@@ -1,3 +1,4 @@
 use std::fmt;
-fn old() {}
+fn new() {}
+fn extra() {}
 fn keep() {}
diff --git a/Cargo.toml b/Cargo.toml
index 3333333..4444444 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1,2 +1,3 @@
 [package]
+name = \"x\"
 version = \"1\"
diff --git a/tests/parser_test.rs b/tests/parser_test.rs
new file mode 100644
index 0000000..5555555
--- /dev/null
+++ b/tests/parser_test.rs
@@ -0,0 +1,2 @@
+fn a() {}
+fn b() {}
";

    #[test]
    fn a_patch_splits_into_one_row_per_file_and_the_totals_add_up() {
        let files = DiffFile::parse(PATCH).unwrap();
        let rows: Vec<FileRow> = files.iter().map(FileRow::from_diff).collect();
        let names: Vec<_> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["parser.rs", "Cargo.toml", "parser_test.rs"]);
        let dirs: Vec<_> = rows.iter().map(|row| row.dir.as_str()).collect();
        assert_eq!(dirs, ["src/config", "", "tests"]);
        assert_eq!(
            rows.iter()
                .map(|row| (row.added, row.removed))
                .collect::<Vec<_>>(),
            [(2, 1), (1, 0), (2, 0)]
        );
        assert_eq!(rows[2].kind, ChangeKind::Added);
        assert_eq!(rows[0].kind, ChangeKind::Modified);
        assert_eq!(totals(&rows), (5, 1));
        assert_eq!(rows[0].icon(), ui::IconName::FileCode);
        assert_eq!(rows[1].icon(), ui::IconName::FileText);
        assert_eq!(rows[2].icon(), ui::IconName::FilePlus);
    }

    #[test]
    fn rows_from_the_summary_match_rows_from_the_patch() {
        let stat = FileStat {
            path: "src/config/parser.rs".into(),
            old_path: None,
            kind: ChangeKind::Modified,
            added: 2,
            removed: 1,
            binary: false,
        };
        let from_patch = FileRow::from_diff(&DiffFile::parse(PATCH).unwrap()[0]);
        assert_eq!(FileRow::from_stat(&stat), from_patch);
    }

    #[test]
    fn counts_use_thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(12_427), "12,427");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(files_label(1), "1 file");
        assert_eq!(files_label(3), "3 files");
    }
}
