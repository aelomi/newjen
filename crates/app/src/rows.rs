//! Transcript rows. The chat view supplies reply states and lays out the column.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, Entity, IntoElement, ParentElement,
    SharedString, Styled, Window, div, prelude::*, px,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::Disableable as _;
use newjen_core::steps::{Step, StepKind, WorkBlock, WorkState};
use newjen_core::transcript::{
    FRESH_START_NOTE, STOPPED_ON_QUIT, STOPPED_ON_QUIT_DETAIL, STOPPED_UNSENT_DETAIL,
    UNREADABLE_NOTE,
};
use newjen_core::{Item, Thread, ToolState};

use crate::approval::ScopeRow;
use crate::format::{cap, cap_ends, elapsed, turn_caption};

use crate::ui::{self, Look, Mark, Part, QuietMark, StepState, StepView, ToolCard, WorkBlockView};
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

/// What a row needs besides the item itself.
pub struct ItemCtx<'a> {
    pub reply: Option<gpui_kit::component::text::TextView>,
    pub retry: Option<&'a OnRestart>,
    pub resume: Option<(&'a OnRestart, &'static str)>,
    pub resends: bool,
    /// No turn runs and nothing waits: a message still unconfirmed was never sent.
    pub idle: bool,
    /// Sends the unconfirmed message again, where that is possible (see `can_resend`).
    pub again: Option<&'a OnRestart>,
    pub footer: Option<&'a TurnFooter>,
}

/// What a transcript item becomes in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Plain,
    /// The row that draws work block `n`: the turn's first tool call.
    Block(usize),
    /// A tool call or a reply between two steps: drawn inside its block.
    Hidden,
}

/// Lines added and removed, by step.
type Stats = Rc<RefCell<HashMap<StatKey, Option<(usize, usize)>>>>;

/// A step by its first call and how many calls it covers: a step grows while its next call
/// merges in, and its totals then change.
type StatKey = (u64, usize);

/// How a thread's items map to rows: one work block per turn that used tools, in place of its
/// tool calls. Rebuilt on every draw, so only the line counts of finished edits are kept.
#[derive(Default)]
pub struct Plan {
    thread: String,
    blocks: Vec<Rc<WorkBlock>>,
    slots: Vec<Slot>,
    /// Lines added and removed by a step that edits files.
    stats: Stats,
}

impl Plan {
    pub fn refresh(
        &mut self,
        thread: &str,
        items: &[Item],
        blocks: Vec<WorkBlock>,
        root: Option<&Path>,
    ) {
        if self.thread != thread {
            self.thread = thread.to_string();
            self.stats.borrow_mut().clear();
        }
        self.slots = vec![Slot::Plain; items.len()];
        self.blocks = blocks.into_iter().map(Rc::new).collect();
        let mut stats = self.stats.borrow_mut();
        for (index, block) in self.blocks.iter().enumerate() {
            let Some(tools) = block.tools.clone().filter(|tools| tools.end <= items.len()) else {
                continue;
            };
            self.slots[tools.start] = Slot::Block(index);
            for (ix, item) in items
                .iter()
                .enumerate()
                .take(tools.end)
                .skip(tools.start + 1)
            {
                if matches!(item, Item::Tool { .. } | Item::Assistant { .. }) {
                    self.slots[ix] = Slot::Hidden;
                }
            }
            let Some(root) = root else { continue };
            for step in &block.steps {
                let edits = matches!(step.label.kind, StepKind::Edit | StepKind::Create);
                let Some(&first) = step.item_ids.first() else {
                    continue;
                };
                let key = (first, step.item_ids.len());
                if !edits || step.state != ToolState::Done || stats.contains_key(&key) {
                    continue;
                }
                let calls: Vec<Item> = items[tools.clone()]
                    .iter()
                    .filter(|item| step.item_ids.contains(&item.id()))
                    .cloned()
                    .collect();
                let summary = newjen_core::changes::estimate_from_items(&calls, root);
                stats.insert(
                    key,
                    (!summary.is_empty()).then(|| (summary.added(), summary.removed())),
                );
            }
        }
    }

    pub fn slot(&self, ix: usize) -> Slot {
        self.slots.get(ix).copied().unwrap_or(Slot::Plain)
    }

    /// Whether the stopped turn ending at `ix` already says so in its work block's header.
    pub fn stop_is_in_block(&self, ix: usize) -> bool {
        self.blocks.iter().any(|block| {
            block.turn.end == ix + 1 && block.tools.is_some() && block.state == WorkState::Stopped
        })
    }
}

/// The text the copy button of a finished turn's footer copies: its last reply.
pub fn turn_answer(items: &[Item], end: usize) -> Option<String> {
    items[..end.min(items.len())]
        .iter()
        .rev()
        .take_while(|item| !matches!(item, Item::User { .. } | Item::TurnEnd { .. }))
        .find_map(|item| match item {
            Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
}

fn steps_label(count: usize) -> String {
    if count == 1 {
        "1 step".to_string()
    } else {
        format!("{} steps", crate::format::thousands(count))
    }
}

fn mark_of(state: ToolState) -> Mark {
    match state {
        ToolState::Running => Mark::Working,
        ToolState::Waiting => Mark::Waiting,
        ToolState::Done => Mark::Done,
        ToolState::Failed | ToolState::Denied => Mark::Failed,
        ToolState::Expired => Mark::Quiet(QuietMark::Expired),
    }
}

/// The work block of one turn: a box with the turn's state in its header and its steps below
/// while it runs, folded to "Worked for 4m 3s · 7 steps" once it ends.
pub fn work_block(plan: &Plan, index: usize, thread: &Entity<Thread>, cx: &App) -> AnyElement {
    let Some(block) = plan.blocks.get(index).cloned() else {
        return div().into_any_element();
    };
    let first = block
        .steps
        .first()
        .and_then(|step| step.item_ids.first())
        .copied()
        .unwrap_or_default();
    let id = ElementId::NamedInteger(format!("block:{}", thread.read(cx).id()).into(), first);
    let live = block.is_live();
    let (mark, title, count) = match block.state {
        WorkState::Working => (Mark::Working, "Working".to_string(), None),
        WorkState::WaitingForYou => (Mark::Waiting, "Waiting for you".to_string(), None),
        state => {
            let line = block.summary(elapsed);
            let (lead, count) = match line.as_deref().map(|line| line.split_once(" · ")) {
                Some(Some((lead, count))) => (lead.to_string(), Some(count.to_string())),
                Some(None) => (line.clone().unwrap_or_default(), None),
                None => ("Worked".to_string(), None),
            };
            let mark = match state {
                WorkState::Stopped => Mark::Stopped,
                WorkState::Failed => Mark::Failed,
                _ => Mark::Done,
            };
            (mark, lead, count)
        }
    };
    let count = count.unwrap_or_else(|| steps_label(block.step_count));
    let shown = live
        .then(|| {
            block
                .elapsed
                .map(|took| SharedString::from(elapsed(took.as_secs())))
        })
        .flatten();
    let parts = {
        let thread = thread.clone();
        let stats = plan.stats.clone();
        Rc::new(move |cx: &App| parts(&block, &thread, &stats.borrow(), cx))
    };
    WorkBlockView::new(id, mark, title, count, live, parts)
        .elapsed(shown)
        .into_any_element()
}

fn parts(
    block: &WorkBlock,
    thread: &Entity<Thread>,
    stats: &HashMap<StatKey, Option<(usize, usize)>>,
    cx: &App,
) -> Vec<Part> {
    let reader = thread.read(cx);
    let items = reader.transcript().items();
    let Some(tools) = block.tools.clone().filter(|tools| tools.end <= items.len()) else {
        return Vec::new();
    };
    let at: HashMap<u64, usize> = items[tools.clone()]
        .iter()
        .enumerate()
        .map(|(offset, item)| (item.id(), tools.start + offset))
        .collect();
    let mut narration = items[tools.clone()]
        .iter()
        .enumerate()
        .filter_map(|(offset, item)| match item {
            Item::Assistant { text, .. } if !text.trim().is_empty() => {
                Some((tools.start + offset, text.trim()))
            }
            _ => None,
        })
        .peekable();
    let mut out = Vec::with_capacity(block.steps.len());
    for step in &block.steps {
        let first_at = step
            .item_ids
            .first()
            .and_then(|id| at.get(id))
            .copied()
            .unwrap_or(usize::MAX);
        while let Some((_, text)) = narration.next_if(|(ix, _)| *ix < first_at) {
            out.push(Part::Narration(cap(text, 4, 400).into()));
        }
        let stat = step
            .item_ids
            .first()
            .and_then(|id| stats.get(&(*id, step.item_ids.len())))
            .copied()
            .flatten();
        out.push(Part::Step(step_view(
            step,
            items,
            &at,
            stat,
            thread,
            reader.id(),
        )));
    }
    out
}

fn step_view(
    step: &Step,
    items: &[Item],
    at: &HashMap<u64, usize>,
    stat: Option<(usize, usize)>,
    thread: &Entity<Thread>,
    thread_id: &str,
) -> StepView {
    let call = (step.item_ids.len() == 1)
        .then(|| {
            step.item_ids
                .first()
                .and_then(|id| at.get(id))
                .and_then(|ix| items.get(*ix))
        })
        .flatten();
    let (state, output) = match call {
        Some(Item::Tool { state, output, .. }) => (Some(*state), output.as_deref()),
        _ => (None, None),
    };
    let quiet = state.and_then(|state| match tool_look(state, output) {
        ToolLook::Quiet { icon, note } => Some((icon, note)),
        ToolLook::Card(_) => None,
    });
    let (mark, mut note) = match quiet {
        Some((icon, note)) => (
            Mark::Quiet(match icon {
                QuietIcon::Expired => QuietMark::Expired,
                QuietIcon::Stopped => QuietMark::Paused,
            }),
            Some(note),
        ),
        None => (mark_of(step.state), None),
    };
    let mut quote = None;
    if step.state == ToolState::Denied {
        note = Some("Denied".to_string());
        quote = output
            .map(str::trim)
            .filter(|text| {
                !text.is_empty()
                    && *text != newjen_core::protocol::DENY_MESSAGE
                    && *text != "Denied"
            })
            .map(|text| format!("\u{201c}{}\u{201d}", cap(text, 2, 240)));
    }
    let ids = step.item_ids.clone();
    let reader = thread.clone();
    StepView {
        key: ElementId::NamedInteger(
            format!("step:{thread_id}").into(),
            step.item_ids.first().copied().unwrap_or_default(),
        ),
        mark,
        headline: step.label.headline.clone().into(),
        detail: step.label.detail.clone().map(Into::into),
        mono: step.label.kind == StepKind::Run,
        waiting: step.state == ToolState::Waiting,
        note: note.map(Into::into),
        quote: quote.map(Into::into),
        stat,
        details: Rc::new(move |cx: &App| details(reader.read(cx), &ids)),
        opens: true,
    }
}

/// The raw calls of a step, for when its row is opened.
fn details(thread: &Thread, ids: &[u64]) -> Vec<ui::Detail> {
    let items = thread.transcript().items();
    ids.iter()
        .filter_map(|id| items.iter().find(|item| item.id() == *id))
        .filter_map(|item| match item {
            Item::Tool {
                id,
                display_name,
                summary,
                input,
                state,
                output,
                ..
            } => {
                let body = tool_body(*state, input, output.as_deref());
                let title = if summary.is_empty() {
                    display_name.clone()
                } else {
                    format!("{display_name} {summary}")
                };
                Some(ui::Detail {
                    id: *id,
                    title: Some(title.into()),
                    arguments: body.arguments.map(Into::into),
                    result: body.result.map(Into::into),
                    failed: step_state(*state) == StepState::Failed,
                })
            }
            _ => None,
        })
        .collect()
}

/// The footer of the turn that ends at `ix`, when the thread knows its model.
pub fn footer_for(model: Option<String>, items: &[Item], ix: usize) -> TurnFooter {
    TurnFooter {
        model,
        answer: turn_answer(items, ix),
    }
}

/// The card shown while a Rift copy is made: the project, a track to the copy, the steps it
/// goes through, and the latest line the hooks printed.
pub struct CopyCard<'a> {
    pub project: &'a str,
    pub took: &'a str,
    pub last_line: Option<&'a str>,
    pub snapshotting: bool,
    pub cancelling: bool,
    pub on_cancel: OnRestart,
}

const TRACK: f32 = 126.;
const FLOW: f32 = 40.;
const FLOW_LOOP: Duration = Duration::from_millis(1200);

pub fn copy_card(card: CopyCard, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let faint = crate::theme::material(cx).glass_hover;
    let cyan = theme.cyan;
    let initial = card
        .project
        .chars()
        .next()
        .map(|letter| letter.to_uppercase().collect::<String>())
        .unwrap_or_default();
    let tile = || {
        v_flex()
            .w(px(96.))
            .h(px(84.))
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1p5()
            .rounded(theme.radius_lg)
    };
    let source = tile()
        .bg(faint)
        .child(
            div()
                .size(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius)
                .bg(theme.info.opacity(0.18))
                .text_color(theme.info)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(initial),
        )
        .child(
            div()
                .max_w(px(88.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_xs()
                .text_color(theme.foreground)
                .child(card.project.to_string()),
        );
    let copy = tile()
        .border_1()
        .border_color(cyan)
        .bg(cyan.opacity(0.08))
        .text_color(cyan)
        .child(ui::icon(
            ui::IconName::Layers,
            gpui_kit::component::Size::Large,
            cyan,
        ))
        .child(div().text_xs().child("copy"));
    let flow = div().absolute().top_0().h_full().w(px(FLOW)).bg(cyan);
    let flow = if cx.reduce_motion() {
        flow.left(px((TRACK - FLOW) / 2.)).into_any_element()
    } else {
        flow.with_animation(
            "copy-flow",
            Animation::new(FLOW_LOOP).repeat(),
            |segment, progress| segment.left(px(-FLOW + progress * (TRACK + FLOW))),
        )
        .into_any_element()
    };
    let track = div()
        .relative()
        .flex_none()
        .w(px(TRACK))
        .h(px(2.))
        .overflow_hidden()
        .rounded_full()
        .bg(theme.border)
        .child(flow);
    let step = |label: &'static str, state: CopyStep| {
        let mark = match state {
            CopyStep::Done => ui::mark(Mark::Done, cx),
            CopyStep::Current => ui::mark(Mark::Working, cx),
            CopyStep::Later => ui::icon(
                ui::IconName::Circle,
                gpui_kit::component::Size::XSmall,
                theme.muted_foreground,
            )
            .into_any_element(),
        };
        h_flex()
            .gap_1p5()
            .items_center()
            .px_3()
            .py_1()
            .rounded_full()
            .when(state == CopyStep::Current, |pill| pill.bg(faint))
            .text_sm()
            .text_color(if state == CopyStep::Later {
                theme.muted_foreground
            } else {
                theme.foreground
            })
            .child(mark)
            .child(label)
    };
    let stage = copy_stage(card.snapshotting, card.last_line.is_some());
    let state = |ix: usize| match ix.cmp(&stage) {
        std::cmp::Ordering::Less => CopyStep::Done,
        std::cmp::Ordering::Equal => CopyStep::Current,
        std::cmp::Ordering::Greater => CopyStep::Later,
    };
    let joint = || div().w_6().h(px(1.)).bg(theme.border);
    let line = match (card.cancelling, card.last_line) {
        (true, _) => "Cancelling\u{2026} Rift is being stopped".to_string(),
        (false, Some(line)) => line.to_string(),
        (false, None) => format!("Creating a Rift copy \u{b7} {}", card.took),
    };
    let cancel = card.on_cancel;
    v_flex()
        .debug_selector(|| "making-copy".to_string())
        .w_full()
        .gap_4()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .child(
            h_flex()
                .justify_center()
                .items_center()
                .gap_3()
                .child(source)
                .child(track)
                .child(copy),
        )
        .child(
            h_flex()
                .justify_center()
                .items_center()
                .gap_2()
                .child(step("Snapshot", state(0)))
                .child(joint())
                .child(step("Copy", state(1)))
                .child(joint())
                .child(step("Setup", state(2))),
        )
        .child(
            h_flex()
                .gap_3()
                .items_center()
                .pt_3()
                .border_t_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(ui::icon(
                            ui::IconName::ChevronRight,
                            gpui_kit::component::Size::XSmall,
                            theme.muted_foreground,
                        ))
                        .child(
                            div()
                                .min_w_0()
                                .font_family(theme.mono_font_family.clone())
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(line),
                        ),
                )
                .child(
                    ui::button("copy-cancel", "Cancel", Look::Ghost, ui::Size::Sm, cx)
                        .debug_selector(|| "copy-cancel".to_string())
                        .disabled(card.cancelling)
                        .child(ui::key_caps("cmd-.", cx))
                        .on_click(move |_, window, cx| cancel(window, cx)),
                ),
        )
        .into_any_element()
}

/// Which of Snapshot (0), Copy (1) and Setup (2) is under way. Rift reports no step names, so
/// Setup is inferred from the first line its hooks print.
fn copy_stage(snapshotting: bool, printed: bool) -> usize {
    if snapshotting {
        0
    } else if printed {
        2
    } else {
        1
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CopyStep {
    Done,
    Current,
    Later,
}

/// The quiet line under a finished turn's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFooter {
    /// The model and effort the thread runs on, "Opus 5.5 · High".
    pub model: Option<String>,
    /// The turn's last reply, for the copy button.
    pub answer: Option<String>,
}

/// Draws a transcript item with its owned reply state and available actions.
pub fn item(item: &Item, ctx: ItemCtx, cx: &App) -> AnyElement {
    let ItemCtx {
        reply,
        retry,
        resume,
        resends,
        idle,
        again,
        footer,
    } = ctx;
    match item {
        Item::User {
            id,
            text,
            unconfirmed,
        } => {
            if *unconfirmed && idle {
                ui::user_pending(
                    ("user", *id).into(),
                    text.clone(),
                    ui::Pending::NotSent(again.cloned()),
                    cx,
                )
            } else {
                ui::user(text.clone(), cx).into_any_element()
            }
        }
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
            let body = tool_body(*state, input, output.as_deref());
            if let Some(arguments) = body.arguments {
                card = card.arguments(arguments);
            }
            if let Some(result) = body.result {
                card = card.result(result);
            }
            card.into_any_element()
        }
        Item::TurnEnd {
            id,
            duration_ms,
            cost_usd,
            stopped,
        } => turn_end(
            *id,
            turn_caption(*duration_ms, *cost_usd, *stopped),
            *stopped,
            footer,
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

struct ToolText {
    arguments: Option<String>,
    result: Option<String>,
}

/// A call's arguments and result as the raw view shows them, bounded so a huge output keeps
/// the transcript responsive.
fn tool_body(state: ToolState, input: &serde_json::Value, output: Option<&str>) -> ToolText {
    let arguments = serde_json::to_string_pretty(input)
        .ok()
        .map(|arguments| cap(&arguments, TOOL_TEXT_LINES, TOOL_TEXT_CHARS));
    // Borrowed, not cloned: this runs on every frame and outputs can be megabytes.
    let result = match state {
        ToolState::Denied => Some("Denied"),
        _ => output,
    };
    let result = result
        .filter(|text| !text.is_empty())
        .map(|result| match state {
            ToolState::Failed => cap_ends(result, FAILED_HEAD, FAILED_TAIL, FAILED_LINE_CHARS),
            _ => cap(result, TOOL_TEXT_LINES, TOOL_TEXT_CHARS),
        });
    ToolText { arguments, result }
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

/// Whether an unconfirmed message can be sent again from its own row: the stopped notice that
/// follows it offers "Send again" (so only while that notice is the last row and idle).
pub fn can_resend(items: &[Item], busy: bool, continue_resends: bool) -> bool {
    !busy && continue_resends && shows_continue(items)
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
    use newjen_core::ToolDisposition;
    match newjen_core::tool_disposition(state, output) {
        ToolDisposition::Expired { note } => ToolLook::Quiet {
            icon: QuietIcon::Expired,
            note,
        },
        ToolDisposition::Stopped { note } => ToolLook::Quiet {
            icon: QuietIcon::Stopped,
            note,
        },
        ToolDisposition::Ordinary => ToolLook::Card(step_state(state)),
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
    let alert = if detail.is_empty() {
        gpui_kit::component::alert::Alert::info(id, title.to_string())
    } else {
        gpui_kit::component::alert::Alert::info(id, detail.to_string()).title(title.to_string())
    };
    ui::assistant(alert, cx).into_any_element()
}

pub fn preparing_work(label: &str, took: &str, last_line: Option<&str>, cx: &App) -> AnyElement {
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
                .child(ui::caption(format!("{label} · {took}"), cx))
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

/// The quiet line under a finished turn's answer: the model and effort, with the turn's time
/// and cost in its tooltip, and a button that copies the answer.
fn turn_end(
    id: u64,
    caption: String,
    stopped: bool,
    footer: Option<&TurnFooter>,
    cx: &App,
) -> AnyElement {
    if stopped {
        return caption_row(ui::IconName::Square, caption, cx);
    }
    let Some(footer) = footer else {
        return caption_row(ui::IconName::Check, caption, cx);
    };
    let model = footer.model.clone().unwrap_or_else(|| "Done".to_string());
    h_flex()
        .debug_selector(|| "turn-footer".to_string())
        .gap_1()
        .items_center()
        .children(
            footer
                .answer
                .clone()
                .map(|answer| ui::CopyButton::new(("answer-copy", id), answer)),
        )
        .child(
            ui::button(("turn-footer", id), model, Look::Ghost, ui::Size::Sm, cx)
                .tooltip(caption)
                .text_color(cx.theme().muted_foreground),
        )
        .into_any_element()
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
    use newjen_core::transcript::{BACKGROUND_STOPPED_NOTE, EXPIRED_NOTE, STOPPED_TOOL_NOTE};

    struct NotSentExample {
        again: Option<OnRestart>,
    }

    impl gpui::Render for NotSentExample {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let user = Item::User {
                id: 1,
                text: "Run the full test suite.".into(),
                unconfirmed: true,
            };
            div().w(px(600.)).child(item(
                &user,
                ItemCtx {
                    reply: None,
                    retry: None,
                    resume: None,
                    resends: true,
                    idle: true,
                    again: self.again.as_ref(),
                    footer: None,
                },
                cx,
            ))
        }
    }

    #[gpui::test]
    fn an_unsent_message_says_so_and_offers_retry_when_it_can_be_sent_again(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let again: OnRestart = Rc::new(|_, _| {});
        let (_, with) = cx.add_window_view(|_, _| NotSentExample { again: Some(again) });
        with.run_until_parked();
        assert!(with.debug_bounds("not-sent-retry").is_some());
        let (_, without) = cx.add_window_view(|_, _| NotSentExample { again: None });
        without.run_until_parked();
        assert!(without.debug_bounds("not-sent-retry").is_none());
    }

    struct NoticeExample;

    impl gpui::Render for NoticeExample {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            v_flex()
                .w(px(600.))
                .child(
                    div()
                        .debug_selector(|| "title-only".to_string())
                        .child(held(
                            "title-only-alert".into(),
                            "Your message is back.",
                            "",
                            cx,
                        )),
                )
                .child(
                    div()
                        .debug_selector(|| "one-line".to_string())
                        .child(ui::assistant(
                            gpui_kit::component::alert::Alert::info(
                                "one-line-alert",
                                "Your message is back.",
                            ),
                            cx,
                        )),
                )
                .child(
                    div()
                        .debug_selector(|| "with-detail".to_string())
                        .child(held(
                            "detail-alert".into(),
                            "Your message is back.",
                            "You can send it when you are ready.",
                            cx,
                        )),
                )
        }
    }

    #[gpui::test]
    fn a_title_only_notice_has_no_empty_detail_line(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| NoticeExample);
        cx.refresh().unwrap();
        let height = cx.debug_bounds("title-only").unwrap().size.height;
        assert_eq!(height, cx.debug_bounds("one-line").unwrap().size.height);
        assert!(height < cx.debug_bounds("with-detail").unwrap().size.height);
    }

    fn tool(id: u64) -> Item {
        Item::Tool {
            id,
            tool_use_id: format!("t{id}"),
            name: "Read".into(),
            display_name: "Read".into(),
            summary: String::new(),
            input: serde_json::json!({}),
            state: ToolState::Done,
            output: None,
            took: None,
            background: false,
        }
    }

    fn reply(id: u64) -> Item {
        Item::Assistant {
            id,
            text: format!("reply {id}"),
            live: false,
            interrupted: false,
        }
    }

    fn block(turn: std::ops::Range<usize>, tools: std::ops::Range<usize>) -> WorkBlock {
        WorkBlock {
            turn,
            tools: Some(tools),
            steps: Vec::new(),
            step_count: 0,
            state: WorkState::Done,
            elapsed: None,
        }
    }

    #[test]
    fn a_turns_tool_calls_and_the_replies_between_them_fold_into_one_block() {
        let items = [
            Item::User {
                id: 1,
                text: "go".into(),
                unconfirmed: false,
            },
            reply(2),
            tool(3),
            reply(4),
            tool(5),
            reply(6),
            Item::TurnEnd {
                id: 7,
                duration_ms: None,
                cost_usd: None,
                stopped: false,
            },
        ];
        let mut plan = Plan::default();
        plan.refresh("a", &items, vec![block(0..7, 2..5)], None);
        let slots: Vec<Slot> = (0..items.len()).map(|ix| plan.slot(ix)).collect();
        assert_eq!(
            slots,
            [
                Slot::Plain,
                Slot::Plain,
                Slot::Block(0),
                Slot::Hidden,
                Slot::Hidden,
                Slot::Plain,
                Slot::Plain
            ]
        );
        // Rows past the plan (the list is one draw ahead) read as plain.
        assert_eq!(plan.slot(99), Slot::Plain);
    }

    #[test]
    fn a_message_between_the_steps_keeps_its_own_row() {
        let items = [
            tool(1),
            Item::User {
                id: 2,
                text: "also".into(),
                unconfirmed: false,
            },
            tool(3),
        ];
        let mut plan = Plan::default();
        plan.refresh("a", &items, vec![block(0..3, 0..3)], None);
        assert_eq!(plan.slot(0), Slot::Block(0));
        assert_eq!(plan.slot(1), Slot::Plain);
        assert_eq!(plan.slot(2), Slot::Hidden);
    }

    #[test]
    fn a_turn_without_tool_calls_has_no_block_and_a_stale_range_is_ignored() {
        let items = [reply(1), tool(2)];
        let mut plan = Plan::default();
        let mut none = block(0..1, 0..1);
        none.tools = None;
        plan.refresh("a", &items, vec![none, block(0..9, 1..9)], None);
        assert_eq!(plan.slot(0), Slot::Plain);
        assert_eq!(plan.slot(1), Slot::Plain);
    }

    #[test]
    fn the_footers_copy_button_takes_the_turns_last_reply() {
        let items = [
            Item::User {
                id: 1,
                text: "a".into(),
                unconfirmed: false,
            },
            reply(2),
            tool(3),
            reply(4),
            Item::TurnEnd {
                id: 5,
                duration_ms: None,
                cost_usd: None,
                stopped: false,
            },
            Item::TurnEnd {
                id: 6,
                duration_ms: None,
                cost_usd: None,
                stopped: false,
            },
        ];
        assert_eq!(turn_answer(&items, 4).as_deref(), Some("reply 4"));
        // A turn that ended without a reply does not borrow the one before it.
        assert_eq!(turn_answer(&items, 5), None);
    }

    #[test]
    fn the_copy_steps_follow_what_rift_has_reported() {
        assert_eq!(copy_stage(true, false), 0);
        assert_eq!(copy_stage(false, false), 1);
        assert_eq!(copy_stage(false, true), 2);
    }

    #[test]
    fn step_counts_read_in_plain_numbers() {
        assert_eq!(steps_label(1), "1 step");
        assert_eq!(steps_label(7), "7 steps");
        assert_eq!(steps_label(12_345), "12,345 steps");
    }

    fn edit(id: u64, new: &str) -> Item {
        Item::Tool {
            id,
            tool_use_id: format!("e{id}"),
            name: "Edit".into(),
            display_name: "Edit".into(),
            summary: String::new(),
            input: serde_json::json!({
                "file_path": format!("/work/f{id}.rs"),
                "old_string": "a",
                "new_string": new,
            }),
            state: ToolState::Done,
            output: None,
            took: None,
            background: false,
        }
    }

    fn edit_step(ids: &[u64]) -> Step {
        Step {
            item_ids: ids.to_vec(),
            state: ToolState::Done,
            label: newjen_core::steps::StepLabel {
                kind: StepKind::Edit,
                headline: "Edited files".into(),
                detail: None,
            },
            took: None,
        }
    }

    #[test]
    fn a_step_that_grows_by_merging_a_call_gets_new_line_counts() {
        let items = [edit(1, "a\nb\nc"), edit(2, "a\nb\nc\nd\ne")];
        let root = Path::new("/work");
        let with = |steps: Vec<Step>, end: usize| WorkBlock {
            steps,
            step_count: end,
            ..block(0..end, 0..end)
        };
        let mut plan = Plan::default();
        plan.refresh(
            "a",
            &items[..1],
            vec![with(vec![edit_step(&[1])], 1)],
            Some(root),
        );
        let first = plan.stats.borrow().get(&(1, 1)).copied().flatten();
        assert!(first.is_some_and(|(added, _)| added > 0));
        plan.refresh(
            "a",
            &items,
            vec![with(vec![edit_step(&[1, 2])], 2)],
            Some(root),
        );
        let merged = plan.stats.borrow().get(&(1, 2)).copied().flatten();
        assert!(merged.unwrap().0 > first.unwrap().0);
    }

    #[test]
    fn a_message_is_sent_again_only_from_the_stopped_notice() {
        let user = Item::User {
            id: 1,
            text: "a".into(),
            unconfirmed: true,
        };
        let items = [user.clone(), Item::Stopped { id: 2 }];
        assert!(can_resend(&items, false, true));
        assert!(!can_resend(&items, true, true));
        assert!(!can_resend(&items, false, false));
        assert!(!can_resend(&items[..1], false, true));
    }

    #[test]
    fn a_stopped_turn_with_tool_calls_says_so_once() {
        let items = [
            tool(1),
            Item::TurnEnd {
                id: 2,
                duration_ms: None,
                cost_usd: None,
                stopped: true,
            },
        ];
        let mut stopped = block(0..2, 0..1);
        stopped.state = WorkState::Stopped;
        let mut plan = Plan::default();
        plan.refresh("a", &items, vec![stopped], None);
        assert!(plan.stop_is_in_block(1));
        assert!(!plan.stop_is_in_block(0));
    }

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
