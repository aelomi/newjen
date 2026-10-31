//! The window's content: the thread list and the conversation on screen.
//!
//! `ChatView` is a GPUI *entity* (think of a component instance with its own state that GPUI
//! keeps alive). It owns:
//! - every thread of the project, each a `newjen_core::Thread` in an entity of its own, with a
//!   background task that feeds it its `claude` events, so threads that are not on screen keep
//!   working;
//! - which thread is on screen, and each thread's composer text field;
//! - where the thread list is: docked beside the chat, hidden, or a drawer over the chat in
//!   narrow windows (see `sidebar.rs`).
//!
//! All the conversation logic lives in core (`Thread`); this file turns clicks and keys into
//! calls on the thread on screen and draws what the threads say. Every change ends with
//! `cx.notify()`, which asks GPUI to call `render` again.

use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, ElementId, Entity, FocusHandle, Focusable, IntoElement, KeyDownEvent,
    ParentElement, Pixels, Render, ScrollStrategy, SharedString, Styled, Subscription, Task,
    UniformListScrollHandle, Window, div, font, prelude::*, px, uniform_list,
};
use gpui_kit::base::input::{InputAccessibility, TextareaState};
use gpui_kit::base::{h_flex, v_flex};
use newjen_core::rift::Tools;
use newjen_core::store::now_ms;
use newjen_core::thread::{self, Quitting, keeps_draft};
use newjen_core::transcript::display_name;
use newjen_core::{
    Item, PermissionAnswer, Project, SessionConfig, Status, Store, Thread, ToolState, session_scope,
};

use crate::approval::{self, Room};
use crate::format::{fit_rows, short_path, text_rows, tilde};
use crate::keys::{
    self, JumpToThread, KeyRoute, NewThread, NextThread, PreviousThread, Stop, ToggleSidebar,
};
use crate::reply;
use crate::rows::{self, OnRestart};
use crate::sidebar::{self, OnPress, Placement, RowFacts};
use crate::theme::SIDEBAR;
use crate::title_bar::{self, Bar, Grants, OnGrantsOpen, OnRevoke};
use crate::ui;
use crate::ui::DOCK_METRICS;
use crate::ui::MessageList;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::TITLE_BAR_HEIGHT;

/// The conversation column is capped at this width and centered, as in the mocks.
const COLUMN_WIDTH: Pixels = px(720.0);

/// ⌘↵ and Esc are ignored for this long after a new approval appears, and for this long after
/// each ignored press. Without it a quick double-tap could answer the next queued request
/// before its dock is even visible. (A held key is handled separately: its repeats never
/// answer anything, see `keys::approval_key`.) Mouse clicks are not affected.
const KEY_ANSWER_DELAY: Duration = Duration::from_millis(300);

/// The list column's side padding (`px_2`, see `sidebar::column`).
const LIST_PADDING: f32 = 16.0;
/// The drawer's side padding: Kit's `Sheet` pads its body 16 px a side.
const DRAWER_PADDING: f32 = 32.0;
/// The project row's side padding (`px_2p5`) and its folder icon with the gap after it: what
/// its text has left of the list's width.
const PROJECT_ROW_CHROME: f32 = 20.0 + 14.0 + 8.0;

/// How often the clock ticks: it keeps "Working · 1m 12s" counting.
const TICK: Duration = Duration::from_secs(1);
/// Every this many ticks (once a minute) idle `claude`s are stopped (see `Thread::idle_check`)
/// and "4m ago" moves on.
const MINUTE_TICKS: u64 = 60;

/// A message from the app itself (not reported by Claude), shown as the thread's last row. It
/// is cleared by the next message, answer or stop that reaches Claude.
struct Notice {
    title: String,
    detail: String,
}

impl Notice {
    fn error(title: &str, detail: String) -> Notice {
        Notice {
            title: title.to_string(),
            detail,
        }
    }
}

/// One thread and what the window keeps for it.
struct Slot {
    thread: Entity<Thread>,
    /// Its composer, made the first time the thread is shown. Each thread has its own, so
    /// half-typed text (and its undo history) stays with its thread.
    field: Option<Entity<TextareaState>>,
    replies: reply::Replies,
    /// The placeholder currently set on that field, to avoid resetting it every event.
    placeholder: SharedString,
    notice: Option<Notice>,
    /// Feeds the thread its events. Dropping the slot (archive) stops it.
    _pump: Task<()>,
    /// Redraws the window whenever the thread changes.
    _changes: Subscription,
}

/// What `ChatView::new` starts from (see `main.rs`).
pub struct Startup {
    pub store: Store,
    pub project: Project,
    /// How to start `claude` (program, environment); each thread adds its own conversation.
    pub base_config: SessionConfig,
    /// The project's saved threads, newest first, already settled after the last run.
    pub threads: Vec<Thread>,
    /// A problem reading the saved threads, shown once like a failed save.
    pub problem: Option<String>,
    pub home: Option<PathBuf>,
    pub tools: Tools,
}

pub struct ChatView {
    store: Store,
    project: Project,
    base_config: SessionConfig,
    /// The project folder's name and its path for display, e.g. `Newjen` and `~/Newjen`.
    project_name: SharedString,
    project_label: SharedString,
    home: Option<PathBuf>,
    tools: Tools,
    /// Every thread, in list order (newest first). Never empty: with no saved thread, a draft.
    slots: Vec<Slot>,
    /// The thread on screen, as an index into `slots`.
    active: usize,
    /// The user hid the docked list with ⌘B (wide windows).
    sidebar_hidden: bool,
    /// The list floats over the chat (narrow windows, after ⌘B).
    drawer_open: bool,
    drawer_focus: FocusHandle,
    list_scroll: UniformListScrollHandle,
    /// A problem reading the saved threads at launch, or the first failed save, shown as a
    /// quiet line until dismissed (see `after_write_error`).
    save_problem: Option<String>,
    /// A failed save has been shown. Later ones are not: the next save writes their rows
    /// again anyway.
    save_problem_seen: bool,
    /// Focus target while an approval is pending (the dock replaces the composer, so the field
    /// is not on screen), so ⌘↵ and Esc reach us.
    root: FocusHandle,
    /// The "N allowed" popover is open, and the focus it takes while it is.
    grants_open: bool,
    grants_focus: FocusHandle,
    /// The approval the dock is showing, to notice when a new one takes its place.
    dock_request: Option<String>,
    /// That approval's path outside the thread's folder, if any (`approval::outside_path`).
    /// Worked out once when the request reaches the dock, because it reads the disk and
    /// `render` runs on every streamed frame.
    dock_outside: Option<String>,
    /// Keyboard answers (⌘↵, Esc) are ignored until this moment; see `KEY_ANSWER_DELAY`.
    keys_ready_at: Option<Instant>,
    /// The clock and the save-problem listener.
    _tasks: Vec<Task<()>>,
    /// Taking focus back when its element leaves the window, and closing the grants popover
    /// when focus leaves it.
    _focus: [Subscription; 2],
}

impl ChatView {
    pub fn new(startup: Startup, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Startup {
            store,
            project,
            base_config,
            threads,
            problem,
            home,
            tools,
        } = startup;
        let project_name = project
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| project.path.display().to_string());
        let project_label = tilde(&project.path, home.as_deref());
        let tasks = vec![clock(cx), save_problems(&store, cx)];
        let grants_focus = cx.focus_handle();
        let focus = [
            cx.on_focus_lost(window, |view, window, cx| view.reclaim_focus(window, cx)),
            cx.on_focus_out(&grants_focus, window, |view, _, _, cx| {
                view.grants_open = false;
                cx.notify();
            }),
        ];
        let mut view = ChatView {
            store,
            project,
            base_config,
            project_name: project_name.into(),
            project_label: project_label.into(),
            home,
            tools,
            slots: Vec::new(),
            active: 0,
            sidebar_hidden: false,
            drawer_open: false,
            drawer_focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            // A load problem is not a save problem: the first failed save still gets shown.
            save_problem_seen: false,
            save_problem: problem,
            root: cx.focus_handle(),
            grants_open: false,
            grants_focus,
            dock_request: None,
            dock_outside: None,
            keys_ready_at: None,
            _tasks: tasks,
            _focus: focus,
        };
        for thread in threads {
            let at = view.slots.len();
            view.add_slot(thread, at, window, cx);
        }
        if view.slots.is_empty() {
            view.add_draft(window, cx);
        }
        // The newest thread is on screen at launch. Nothing is started: a thread's `claude`
        // starts with the first message sent to it.
        view.enter(window, cx);
        match thread::Removal::pending(&view.store) {
            Ok(pending) => view.remove_copies(pending, cx),
            Err(error) => {
                view.save_problem.get_or_insert(format!(
                    "Couldn't list the Rift copies to remove: {error:#}"
                ));
            }
        }
        view
    }

    /// When the focused element leaves the window (an overlay closed without handing focus
    /// back, say): the composer takes it, or the root while the dock replaces the composer, so
    /// the shortcuts keep working.
    fn reclaim_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Deferred: GPUI draws no new frame for focus that a focus-lost listener moves, so the
        // composer would not show its focus until the next redraw.
        cx.defer_in(window, |view, window, cx| match view.field_focus(cx) {
            Some(field) if view.dock_request.is_none() => window.focus(&field, cx),
            _ => window.focus(&view.root, cx),
        });
    }

    /// The composer's focus handle of the thread on screen, so `main` can focus it when the
    /// window opens.
    pub fn field_focus(&self, cx: &App) -> Option<FocusHandle> {
        let field = self.slots.get(self.active)?.field.as_ref()?;
        Some(field.read(cx).focus_handle(cx))
    }

    /// At quit (⌘Q, or the window closed): settles and saves every thread and stops every
    /// `claude`, all at once (see `newjen_core::thread::finish_quit`). GPUI gives the whole app
    /// about 200 ms; this takes about as long as the slowest `claude` takes to stop.
    pub fn quit(&mut self, cx: &mut Context<Self>) {
        let quitting: Vec<Quitting> = self
            .slots
            .iter()
            .map(|slot| slot.thread.update(cx, |thread, _| thread.begin_quit()))
            .collect();
        thread::finish_quit(&self.store, quitting);
    }

    // -----------------------------------------------------------------------------------------
    // Threads
    // -----------------------------------------------------------------------------------------

    /// Puts `thread` in the list at row `at`, in its own entity, with the task that feeds it its
    /// events.
    fn add_slot(&mut self, thread: Thread, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let events = thread.events();
        let thread = cx.new(|_| thread);
        let weak = thread.downgrade();
        // Runs on the UI thread until the slot is dropped. `apply` can take ~150 ms when core
        // restarts claude to repair a conversation.
        let pump = cx.spawn(async move |_, cx| {
            while let Ok(event) = events.recv().await {
                let applied = weak.update(cx, |thread, cx| {
                    thread.apply(event);
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        let changes = cx.observe_in(&thread, window, |view, thread, window, cx| {
            view.thread_changed(&thread, window, cx)
        });
        let slot = Slot {
            thread,
            field: None,
            replies: Default::default(),
            placeholder: SharedString::default(),
            notice: None,
            _pump: pump,
            _changes: changes,
        };
        let at = at.min(self.slots.len());
        self.slots.insert(at, slot);
        self.sync_replies(at, cx);
    }

    /// A new, unsaved thread at the top of the list. Not shown yet; see `new_thread`.
    fn add_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = Thread::new_draft(self.store.clone(), &self.project, self.base_config.clone());
        self.add_slot(draft, 0, window, cx);
    }

    /// A thread changed (an event arrived, or the app acted on it): redraw, and if it is the
    /// thread on screen, keep the composer and the dock in step with it.
    fn thread_changed(
        &mut self,
        thread: &Entity<Thread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(ix) = self.slots.iter().position(|slot| slot.thread == *thread) {
            self.sync_replies(ix, cx);
        }
        if self
            .slots
            .get(self.active)
            .is_some_and(|slot| slot.thread == *thread)
        {
            self.sync_field(window, cx);
        }
        cx.notify();
    }

    fn sync_replies(&mut self, ix: usize, cx: &mut Context<Self>) {
        let slot = &mut self.slots[ix];
        slot.thread.clone().update(cx, |thread, cx| {
            reply::sync(&mut slot.replies, thread.transcript().items(), cx)
        })
    }

    /// The thread on screen.
    fn thread(&self) -> &Entity<Thread> {
        &self.slots[self.active].thread
    }

    fn index_of(&self, thread_id: &str, cx: &App) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.thread.read(cx).id() == thread_id)
    }

    /// ⌘N or the "New thread" row: a new draft on screen with the cursor in its composer. On
    /// a draft that has nothing typed yet, that draft is the new thread, so none pile up.
    fn new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.drawer_open = false;
        let composer = self.composer_text(self.active, cx);
        if is_empty_draft(self.thread().read(cx).is_draft(), &composer) {
            self.enter(window, cx);
            return;
        }
        self.add_draft(window, cx);
        // The thread on screen moved down a row.
        self.active += 1;
        self.show(0, window, cx);
    }

    /// Shows the thread at row `target`. The thread being left stops being "on screen" (so it
    /// can become unread again), and a draft left with an empty composer is dropped: it was
    /// never saved, and empty threads should not pile up.
    fn show(&mut self, target: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.drawer_open = false;
        if target >= self.slots.len() {
            return;
        }
        if target != self.active {
            let leaving = self.active;
            let composer = self.composer_text(leaving, cx);
            let thread = self.slots[leaving].thread.clone();
            let draft = thread.read(cx).is_draft();
            thread.update(cx, |thread, cx| {
                thread.set_active(false);
                cx.notify();
            });
            let mut target = target;
            if is_empty_draft(draft, &composer) {
                // A draft can have a `claude` already (Try again after a failed first send).
                // `archive` stops it on a background thread; dropped with the thread instead,
                // it would stop on this one and freeze the window for a moment. For a draft it
                // writes nothing: it never touched the store.
                let removal = thread.update(cx, |thread, _| thread.archive());
                self.remove_copies(removal, cx);
                // Dropping the slot stops its task.
                self.slots.remove(leaving);
                target = sidebar::shifted(target, leaving);
            }
            self.active = target;
        }
        self.enter(window, cx);
    }

    fn show_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(thread_id, cx) {
            self.show(ix, window, cx);
        }
    }

    /// Makes the thread at `active` the one on screen: clears its unread flag, gives its
    /// composer (or its approval dock) the keyboard, and scrolls the list to it.
    fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_replies(self.active, cx);
        // No `notify` on the thread: this view redraws itself below. (It also runs while the
        // view is being built, when nothing can observe the thread yet.)
        self.thread()
            .clone()
            .update(cx, |thread, _| thread.set_active(true));
        // As if no dock was up: a waiting approval on this thread then takes the keyboard (and
        // gets the short key delay) exactly as a new one would.
        self.dock_request = None;
        self.sync_field(window, cx);
        if self.dock_request.is_none() {
            let field = self.ensure_field(window, cx);
            let handle = field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        self.list_scroll
            .scroll_to_item(self.active, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Right click → "Archive thread": hides the thread from the list for good (its rows stay
    /// in the store) and stops its `claude`. If it was on screen, the row that moves into its
    /// place is shown; if it was the last thread, a new draft.
    fn archive(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(thread_id, cx) else {
            return;
        };
        let len = self.slots.len();
        let slot = self.slots.remove(ix);
        let removal = slot.thread.update(cx, |thread, cx| {
            cx.notify();
            thread.archive()
        });
        drop(slot);
        self.remove_copies(removal, cx);
        if ix != self.active {
            self.active = sidebar::shifted(self.active, ix);
            cx.notify();
            return;
        }
        match sidebar::after_removal(ix, len) {
            Some(next) => self.active = next,
            None => {
                self.add_draft(window, cx);
                self.active = 0;
            }
        }
        self.enter(window, cx);
    }

    fn render_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let list = self.render_list(
            f32::from(SIDEBAR) - DRAWER_PADDING - PROJECT_ROW_CHROME,
            window,
            cx,
        );
        v_flex()
            .size_full()
            .key_context(keys::CONTEXT)
            .track_focus(&self.drawer_focus)
            .capture_action(
                cx.listener(|view, _: &gpui_kit::base::actions::Cancel, _, cx| {
                    view.drawer_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .capture_key_down(cx.listener(Self::on_key_capture))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::on_stop))
            .on_action(cx.listener(Self::on_new_thread))
            .on_action(cx.listener(Self::on_previous))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_jump))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .children(list)
            .into_any_element()
    }

    fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let width = f32::from(window.viewport_size().width);
        (self.sidebar_hidden, self.drawer_open) =
            sidebar::toggled(width, self.sidebar_hidden, self.drawer_open);
        cx.notify();
    }

    /// Runs `idle_check` on every thread once a minute, and redraws while something is
    /// counting ("Working · 1m 12s") or once a minute ("4m ago"). Also follows macOS's Reduce
    /// Motion setting, which has no change notice of its own here.
    fn tick(&mut self, ticks: u64, cx: &mut Context<Self>) {
        gpui_kit::base::apply_system_reduce_motion(cx);
        let minute = ticks.is_multiple_of(MINUTE_TICKS);
        if minute {
            let now = Instant::now();
            for slot in &self.slots {
                slot.thread.update(cx, |thread, cx| {
                    if thread.idle_check(now) {
                        cx.notify();
                    }
                });
            }
        }
        let busy = self
            .slots
            .iter()
            .any(|slot| slot.thread.read(cx).status().is_busy());
        if minute || busy {
            cx.notify();
        }
    }

    /// A failed removal is tried again at the next launch (`Removal::pending`).
    fn remove_copies(
        &mut self,
        removals: impl IntoIterator<Item = thread::Removal>,
        cx: &mut Context<Self>,
    ) {
        for removal in removals {
            let path = tilde(&removal.copy().path, self.home.as_deref());
            let tools = self.tools.clone();
            let removed = cx.background_spawn(async move { removal.run(&tools) });
            cx.spawn(async move |view, cx| {
                if let Err(error) = removed.await {
                    let _ = view.update(cx, |view, cx| {
                        view.save_problem = Some(removal_problem(&path, &error));
                        cx.notify();
                    });
                }
            })
            .detach();
        }
    }

    /// A write to the store failed. The first one is shown once, quietly; the thread it
    /// belonged to writes it again at its next save.
    fn save_failed(&mut self, problem: String, cx: &mut Context<Self>) {
        let first = !self.save_problem_seen;
        (self.save_problem, self.save_problem_seen) =
            after_write_error(self.save_problem.take(), self.save_problem_seen, problem);
        if first {
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------------------------
    // The thread on screen
    // -----------------------------------------------------------------------------------------

    /// The composer of the thread on screen, made the first time it is needed.
    fn ensure_field(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        let slot = &mut self.slots[self.active];
        if let Some(field) = &slot.field {
            return field.clone();
        }
        let field = cx.new(|cx| ui::composer_field(window, cx));
        slot.field = Some(field.clone());
        // A new field has no placeholder yet; `sync_field` sets it.
        slot.placeholder = SharedString::default();
        field
    }

    /// The text in row `ix`'s composer ("" if it never had one).
    fn composer_text(&self, ix: usize, cx: &App) -> String {
        self.slots
            .get(ix)
            .and_then(|slot| slot.field.as_ref())
            .map(|field| field.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Keeps focus and the text field in step with the thread on screen: focus on the root
    /// while an approval is pending (so ⌘↵ and Esc reach `on_key_down`), back in the field
    /// afterwards, and a placeholder that fits the moment.
    fn sync_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = self.ensure_field(window, cx);
        let (front, outside, status, empty) = {
            let thread = self.thread().read(cx);
            let request = thread.transcript().pending_approval();
            let front = request.map(|request| request.request_id.clone());
            // Only for a new request: it reads the disk.
            let outside = if front != self.dock_request {
                request.and_then(|request| approval::outside_path(request, thread.cwd()))
            } else {
                self.dock_outside.clone()
            };
            let empty = thread.transcript().items().is_empty();
            (front, outside, thread.status(), empty)
        };
        if front != self.dock_request {
            // A new request is on the dock: give the user a moment to see it.
            self.keys_ready_at = front
                .as_ref()
                .map(|_| cx.background_executor().now() + KEY_ANSWER_DELAY);
            match (&self.dock_request, &front) {
                // The dock appears in place of the composer.
                (None, Some(_)) => window.focus(&self.root, cx),
                // The last approval was answered: the composer is back.
                (Some(_), None) => {
                    let handle = field.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
                // One request replaced by the next, or nothing to nothing: focus stays.
                _ => {}
            }
            self.dock_request = front;
        }
        self.dock_outside = outside;

        let placeholder = SharedString::from(match status {
            Status::Working | Status::Waiting => "Claude is working…".to_string(),
            _ if empty => format!("Ask Claude to work on {}…", self.project_label),
            _ => "Reply to Claude…".to_string(),
        });
        let slot = &mut self.slots[self.active];
        if placeholder != slot.placeholder {
            slot.placeholder = placeholder.clone();
            field.update(cx, |field, cx| {
                field.set_accessibility(
                    InputAccessibility::new(Some(gpui::Role::TextInput))
                        .with_label(Some(placeholder.clone())),
                );
                field.set_placeholder(placeholder.clone(), window, cx);
            });
        }
    }

    fn is_busy(&self, cx: &App) -> bool {
        is_busy(self.thread().read(cx).status())
    }

    /// Sends the composer's text as the next message (Enter or the send button).
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = self.ensure_field(window, cx);
        let text = field.read(cx).value().trim().to_string();
        if text.is_empty() || self.is_busy(cx) {
            return;
        }
        if self.send_text(&text, window, cx) {
            field.update(cx, |field, cx| field.clean(window, cx));
        }
    }

    /// Continue on the "Stopped when Newjen quit" notice: a visible message asking Claude to
    /// go on, or the user's last message again if Claude never started its turn
    /// (`Thread::continue_text`).
    /// Pressing it is the only way a stopped turn goes on (nothing restarts by itself).
    fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_busy(cx) {
            let text = self.thread().read(cx).continue_text();
            self.send_text(&text, window, cx);
        }
    }

    /// Sends `text` to the thread on screen, starting its `claude` if none runs. Returns
    /// whether it went out. If not, nothing was sent, the conversation is as it was (so the
    /// composer stays usable and keeps its text) and a notice says why.
    fn send_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let thread = self.thread().clone();
        let (had_claude, program) = {
            let thread = thread.read(cx);
            (thread.has_session(), thread.session_config().program)
        };
        let sent = thread.update(cx, |thread, cx| {
            let sent = thread.send(text);
            cx.notify();
            sent
        });
        let went_out = sent.is_ok();
        self.slots[self.active].notice = match sent {
            // Writing worked, so an older "could not reach Claude" notice is out of date.
            Ok(_) => None,
            Err(error) if had_claude => notice_after(Err(error), "Could not reach Claude"),
            // No claude was running, so the error is from starting one.
            Err(error) => Some(spawn_notice(&error, &program)),
        };
        self.sync_field(window, cx);
        cx.notify();
        went_out
    }

    /// Answers the pending approval. `request_id` is the request the button belonged to; a
    /// stale click (that request was already answered) does nothing. `rule_cut` says the dock
    /// could not show the remembered rule in full (`ScopeRow::once`), so "Always allow" is sent
    /// as allow once (see `sent_answer`).
    fn answer(
        &mut self,
        request_id: Option<&str>,
        choice: PermissionAnswer,
        rule_cut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = sent_answer(choice, rule_cut);
        let thread = self.thread().clone();
        let sent = thread.update(cx, |thread, cx| {
            let sent = thread.answer(request_id, answer);
            cx.notify();
            sent
        });
        // No note after the click when "Always allow" acts once: the scope row under the dock
        // already said so before the click.
        self.slots[self.active].notice = notice_after(sent, "Could not send your answer to Claude");
        self.sync_field(window, cx);
        cx.notify();
    }

    /// ⌘. or the stop button: deny-and-stop a pending approval, otherwise stop the turn.
    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread().clone();
        let (pending, busy) = {
            let thread = thread.read(cx);
            let pending = thread.transcript().pending_approval().is_some();
            (pending, thread.status().is_busy())
        };
        if !pending && !busy {
            return;
        }
        let sent = thread.update(cx, |thread, cx| {
            let sent = thread.stop();
            cx.notify();
            sent
        });
        let failed = if pending {
            "Could not send your answer to Claude"
        } else {
            "Could not stop Claude"
        };
        self.slots[self.active].notice = notice_after(sent, failed);
        self.sync_field(window, cx);
        cx.notify();
    }

    /// "Try again" on an error: a fresh `claude` for the same conversation (core works out
    /// whether it resumes or starts over).
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread().clone();
        let program = thread.read(cx).session_config().program;
        let started = thread.update(cx, |thread, cx| {
            let started = thread.restart();
            cx.notify();
            started
        });
        self.slots[self.active].notice = started.err().map(|error| spawn_notice(&error, &program));
        self.sync_field(window, cx);
        cx.notify();
    }

    /// Whether a keyboard answer may go through now. A press that comes too soon is ignored
    /// and pushes the moment back, so tapping quickly never answers a request you have not
    /// seen yet.
    fn keys_ready(&mut self, cx: &App) -> bool {
        let now = cx.background_executor().now();
        match self.keys_ready_at {
            Some(ready_at) if now < ready_at => {
                self.keys_ready_at = Some(now + KEY_ANSWER_DELAY);
                false
            }
            _ => true,
        }
    }

    /// Where a key press goes (see `keys::key_route`).
    fn key_route(&self, event: &KeyDownEvent, window: &Window, cx: &App) -> KeyRoute {
        let pending = self
            .thread()
            .read(cx)
            .transcript()
            .pending_approval()
            .is_some();
        keys::key_route(event, self.drawer_open, overlay_focused(window), pending)
    }

    /// Esc closes the thread drawer. GPUI calls this for every key press inside the view
    /// *before* the focused element sees it (the "capture" phase), so the drawer closes even
    /// while the text field has the keyboard. Every other key is left alone here.
    fn on_key_capture(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.key_route(event, window, cx) == KeyRoute::CloseDrawer {
            self.drawer_open = false;
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// ⌘↵ (allow once) and Esc (deny) answer a pending approval. GPUI calls this *after* the
    /// focused element had its turn (the "bubble" phase), so a popover or menu that is open
    /// over the dock closes on Esc first, and the Esc never reaches the request.
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match self.key_route(event, window, cx) {
            KeyRoute::Answer(answer) => {
                // An approval key while the dock is up is ours, even when it is ignored below.
                cx.stop_propagation();
                if self.keys_ready(cx) {
                    self.answer(None, answer, false, window, cx);
                }
            }
            // Held down: macOS is repeating the key. Only a fresh press answers.
            KeyRoute::Ignore => cx.stop_propagation(),
            // The drawer's Esc was taken in the capture phase.
            KeyRoute::CloseDrawer | KeyRoute::Pass => {}
        }
    }

    fn on_stop(&mut self, _: &Stop, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(window, cx);
    }

    fn on_new_thread(&mut self, _: &NewThread, window: &mut Window, cx: &mut Context<Self>) {
        self.new_thread(window, cx);
    }

    fn on_previous(&mut self, _: &PreviousThread, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = sidebar::step(self.active, self.slots.len(), false) {
            self.show(target, window, cx);
        }
    }

    fn on_next(&mut self, _: &NextThread, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = sidebar::step(self.active, self.slots.len(), true) {
            self.show(target, window, cx);
        }
    }

    fn on_jump(&mut self, jump: &JumpToThread, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = sidebar::jump(jump.0, self.slots.len()) {
            self.show(target, window, cx);
        }
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &ToggleSidebar,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(window, cx);
    }

    /// A click handler for buttons and rows, which receive `&mut App` rather than our
    /// `Context`, so it goes back through the entity.
    fn press(
        &self,
        cx: &Context<Self>,
        act: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> OnPress {
        let view = cx.weak_entity();
        Rc::new(move |window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |view, cx| act(view, window, cx));
        })
    }

    // -----------------------------------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------------------------------

    /// Row `ix` of the message list: a transcript item, or the app notice after them.
    fn render_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on_restart: OnRestart = self.press(cx, |view, window, cx| view.restart(window, cx));
        let on_continue: OnRestart = self.press(cx, |view, window, cx| view.resume(window, cx));
        let unbuilt = {
            let slot = &self.slots[self.active];
            match slot.thread.read(cx).transcript().items().get(ix) {
                Some(Item::Assistant { id, text, .. }) if !slot.replies.contains(id) => {
                    Some((*id, text.clone()))
                }
                _ => None,
            }
        };
        if let Some((id, text)) = unbuilt {
            self.slots[self.active].replies.ensure(id, &text, cx);
        }
        let slot = &self.slots[self.active];
        let cx: &App = cx;
        let thread = slot.thread.read(cx);
        let busy = is_busy(thread.status());
        let items = thread.transcript().items();
        let notice = slot.notice.as_ref();
        let retry = offers_retry(ix, items.len(), notice, busy).then_some(&on_restart);
        // Only the notice that is the last row gets Continue; an older one (Newjen quit twice)
        // is history.
        let resume = (ix + 1 == items.len() && !busy && rows::shows_continue(items)).then_some((
            &on_continue,
            rows::continue_label(thread.continue_resends()),
        ));
        let row = if let Some(item) = items.get(ix) {
            rows::item(
                item,
                slot.replies.get(&item.id()).map(|reply| reply.view(cx)),
                retry,
                resume,
                rows::stopped_resends(items, ix),
                cx,
            )
        } else if let Some(notice) = notice {
            rows::error("notice".into(), &notice.title, &notice.detail, retry, cx)
        } else {
            div().into_any_element()
        };
        let first = ix == 0;
        // Center each row in the column, like the composer below it.
        div()
            .w_full()
            .flex()
            .justify_center()
            .px_4()
            .when(first, |row| row.pt_4())
            .child(div().w_full().max_w(COLUMN_WIDTH).py_2().child(row))
            .into_any_element()
    }

    /// The bar over the chat. While the list is out of sight it also has the button that
    /// brings it back, with an amber dot if another thread waits for an approval.
    fn render_title_bar(
        &self,
        placement: Placement,
        main_width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let show_list = (placement != Placement::Docked).then(|| {
            let statuses: Vec<Status> = self
                .slots
                .iter()
                .map(|slot| slot.thread.read(cx).status())
                .collect();
            let attention = sidebar::hidden_needs_you(&statuses, self.active);
            let toggle = self.press(cx, |view, window, cx| view.toggle_sidebar(window, cx));
            (toggle, attention)
        });
        let entity = self.thread().clone();
        let bar = {
            let thread = entity.read(cx);
            let path = tilde(thread.cwd(), self.home.as_deref());
            Bar {
                title: thread.title().to_string().into(),
                path: short_path(&path, title_bar::path_budget(main_width)),
                show_list,
                grants: thread.grants().to_vec(),
                status: thread.status(),
                background_tasks: thread.background_tasks().len(),
            }
        };
        // claude cannot forget a rule, so core stops it (now, or when the turn ends) and the
        // next message starts one without the rule.
        let on_revoke: OnRevoke = Rc::new(move |rule: &str, _window: &mut Window, cx: &mut App| {
            entity.update(cx, |thread, cx| {
                thread.revoke(rule);
                cx.notify();
            });
        });
        let view = cx.weak_entity();
        let on_open: OnGrantsOpen = Rc::new(move |open, _, cx| {
            let _ = view.update(cx, |view, cx| {
                view.grants_open = open;
                cx.notify();
            });
        });
        let grants = Grants {
            open: self.grants_open,
            focus: self.grants_focus.clone(),
            on_open,
            on_revoke,
        };
        title_bar::render(bar, grants, window, cx)
    }

    fn render_empty_state(&self, cx: &App) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .child(ui::title("What should Claude work on?", cx))
            .child(
                // Centered and kept to the column, so a long path wraps under the title.
                div().max_w(COLUMN_WIDTH).text_center().child(
                    ui::caption(
                        format!(
                            "Claude Code runs in {}. You approve each tool call before it runs.",
                            self.project_label
                        ),
                        cx,
                    )
                    .text_base(),
                ),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let slot = &self.slots[self.active];
        let count =
            slot.thread.read(cx).transcript().items().len() + usize::from(slot.notice.is_some());
        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(MessageList::new(
                "conversation",
                count,
                cx.processor(|view, ix: usize, window, cx| view.render_row(ix, window, cx)),
            ))
    }

    /// How much of an approval fits in the dock at the current window size. Returns the title
    /// cut to the dock metrics’ title row limit and the room left for the detail: columns from the dock's width (in
    /// a chat `main_width` wide) and the mono font's character width, rows from the window
    /// height minus the title bar, the dock's padding, buttons and title, the scope row under
    /// it, the save-problem line if shown, and a strip of conversation.
    fn dock_room(&self, title: &str, main_width: f32, window: &Window, cx: &App) -> (String, Room) {
        let m = &DOCK_METRICS;
        let font_size = gpui::rems(0.875).to_pixels(window.rem_size());
        let text_system = window.text_system();
        let mono = text_system.resolve_font(&font(cx.theme().mono_font_family.clone()));
        let char_width = text_system
            .advance(mono, font_size, '0')
            .map(|size| f32::from(size.width))
            .unwrap_or(f32::from(font_size) * 0.6)
            .max(1.0);
        let viewport = window.viewport_size();
        let dock_width = (main_width - m.page_padding * 2.).min(f32::from(COLUMN_WIDTH));
        // One column spare: real wrapping is a little less tidy than the estimate.
        let cols = ((dock_width - m.detail_inset()) / char_width).max(0.0) as usize;
        let cols = cols.saturating_sub(1);

        let title = fit_rows(title, cols, m.title_rows);
        let line_height = f32::from(gpui::phi().to_pixels(font_size.into(), window.rem_size()));
        let problem = if self.save_problem.is_some() {
            m.scope_gap + line_height
        } else {
            0.0
        };
        let used = f32::from(TITLE_BAR_HEIGHT)
            + m.top_padding
            + m.bottom_padding
            + m.chrome_height(f32::from(window.rem_size()) * 2.)
            + text_rows(&title, cols) as f32 * line_height
            + m.scope_gap
            + line_height
            + problem
            + m.conversation_reserve;
        let free = (f32::from(viewport.height) - used).max(0.0);
        let rows = ((free / line_height) as usize).clamp(m.detail_rows_min, m.detail_rows_max);
        (title, Room { cols, rows })
    }

    /// The approval dock, shown instead of the composer while Claude waits for an answer.
    fn render_dock(
        &self,
        main_width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (request, waiting, cwd) = {
            let thread = self.thread().read(cx);
            let request = thread.transcript().pending_approval()?.clone();
            // Say when more approvals are queued behind this one (each tool waiting for an
            // answer shows as Waiting), so a second request is never a surprise.
            let waiting = thread
                .transcript()
                .items()
                .iter()
                .filter(|item| {
                    matches!(
                        item,
                        Item::Tool {
                            state: ToolState::Waiting,
                            ..
                        }
                    )
                })
                .count();
            (request, waiting, thread.cwd().to_path_buf())
        };
        let mut title = approval::title(&request, Some(&cwd));
        if waiting > 1 {
            title.push_str(&format!(" (1 of {waiting})"));
        }
        let (title, room) = self.dock_room(&title, main_width, window, cx);
        // `sync_field` keeps `dock_outside` in step with the dock's request.
        let detail = approval::detail(&request, Some(&cwd), self.dock_outside.as_deref(), room);
        let scope = approval::scope_row(
            &session_scope(&request),
            &request,
            detail.shortened,
            room.cols,
        );
        let rule_cut = scope.once;
        let request_id = SharedString::from(request.request_id.clone());
        let view = cx.weak_entity();
        let answered_id = request_id.clone();
        let card = ui::ApprovalCard::new(
            (ElementId::from("approval"), request_id.clone()),
            title,
            detail.text,
            move |answer, window, cx| {
                let _ = view.update(cx, |view, cx| {
                    view.answer(Some(answered_id.as_ref()), answer, rule_cut, window, cx)
                });
            },
        );
        Some(ui::approval_dock(card, rows::scope(&scope, cx)))
    }

    fn render_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let field = self.ensure_field(window, cx);
        let send = self.press(cx, |view, window, cx| view.send(window, cx));
        let stop = self.press(cx, |view, window, cx| view.stop(window, cx));
        ui::Composer::new(field, self.is_busy(cx), send, stop).into_any_element()
    }

    /// "Couldn't save threads: …", once, quietly, above the composer, until dismissed.
    fn render_save_problem(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let problem = self.save_problem.clone()?;
        let dismiss = cx.listener(|view: &mut ChatView, _, _, cx| {
            view.save_problem = None;
            cx.notify();
        });
        let theme = cx.theme();
        Some(
            h_flex()
                .gap_1p5()
                .px_1()
                .min_w_0()
                .child(ui::icon(
                    ui::IconName::TriangleAlert,
                    gpui_kit::component::Size::XSmall,
                    theme.colors.warning,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(theme.colors.muted_foreground)
                        .child(ui::ellipsis(problem)),
                )
                .child(
                    ui::icon_button("dismiss-save-problem", ui::IconName::Close, "Dismiss", cx)
                        .on_click(dismiss),
                )
                .into_any_element(),
        )
    }

    /// The list's content: the project, "New thread", then the threads (or, with none yet, a
    /// line saying so). `room` is the width the project row's text has.
    fn render_list(&self, room: f32, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let drafts: Vec<bool> = self
            .slots
            .iter()
            .map(|slot| slot.thread.read(cx).is_draft())
            .collect();
        let empty = sidebar::is_empty_list(&drafts);
        let new_thread = self.press(cx, |view, window, cx| view.new_thread(window, cx));
        let threads = if empty {
            sidebar::empty_caption(cx)
        } else {
            uniform_list(
                "threads",
                self.slots.len(),
                cx.processor(|view, range: Range<usize>, _window, cx| {
                    view.render_thread_rows(range, cx)
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        vec![
            sidebar::project_row(&self.project_name, &self.project_label, room, window, cx),
            sidebar::new_thread_row(empty, new_thread, cx),
            // The gap under "New thread", as in the mock.
            div().flex_none().h(px(6.0)).into_any_element(),
            threads,
        ]
    }

    /// The thread rows in `range`, for the list (`uniform_list` asks only for the rows on
    /// screen, so hundreds of threads cost no more than a screenful).
    fn render_thread_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let now = Instant::now();
        let now_ms = now_ms();
        range
            .filter_map(|ix| {
                let slot = self.slots.get(ix)?;
                let (id, row) = {
                    let thread = slot.thread.read(cx);
                    let id = SharedString::from(thread.id().to_string());
                    let row = sidebar::Row {
                        id: id.clone(),
                        title: thread.title().to_string().into(),
                        look: sidebar::row_look(&row_facts(thread, now), now_ms),
                        selected: ix == self.active,
                    };
                    (id, row)
                };
                let shown = id.clone();
                let on_press = self.press(cx, move |view, window, cx| {
                    view.show_thread(&shown, window, cx)
                });
                let on_archive =
                    self.press(cx, move |view, window, cx| view.archive(&id, window, cx));
                Some(sidebar::thread_row(row, on_press, on_archive, cx))
            })
            .collect()
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width = f32::from(window.viewport_size().width);
        // The drawer only exists in narrow windows; widening the window docks the list.
        if width >= sidebar::NARROW_WINDOW {
            self.drawer_open = false;
        }
        // Revoking the last rule takes the chip, and its popover, away.
        if self.thread().read(cx).grants().is_empty() {
            self.grants_open = false;
        }
        let placement = sidebar::placement(width, self.sidebar_hidden, self.drawer_open);
        let sidebar_px = f32::from(SIDEBAR);
        // The drawer floats over the chat, so only the docked list takes width from it.
        let main_width = match placement {
            Placement::Docked => width - sidebar_px,
            Placement::Drawer | Placement::Hidden => width,
        };

        let (thread_id, empty) = {
            let slot = &self.slots[self.active];
            let thread = slot.thread.read(cx);
            (
                SharedString::from(thread.id().to_string()),
                thread.transcript().items().is_empty() && slot.notice.is_none(),
            )
        };
        let title_bar = self.render_title_bar(placement, main_width, window, cx);
        let body = if empty {
            self.render_empty_state(cx).into_any_element()
        } else {
            self.render_messages(cx).into_any_element()
        };
        let bottom = match self.render_dock(main_width, window, cx) {
            Some(dock) => dock,
            None => self.render_composer(window, cx),
        };
        let problem = self.render_save_problem(cx);

        let docked = (placement == Placement::Docked).then(|| {
            let toggle = self.press(cx, |view, window, cx| view.toggle_sidebar(window, cx));
            let header = sidebar::header(toggle, window, cx);
            let room = sidebar_px - LIST_PADDING - PROJECT_ROW_CHROME;
            let list = self.render_list(room, window, cx);
            sidebar::column(header, list, cx)
        });
        use gpui_kit::component::{Placement as SheetPlacement, WindowExt as _};
        if placement == Placement::Drawer && !window.has_active_sheet(cx) {
            let view = cx.entity().downgrade();
            let closed = view.clone();
            window.open_sheet_at(SheetPlacement::Left, cx, move |sheet, window, cx| {
                let content = view
                    .update(cx, |view, cx| view.render_drawer(window, cx))
                    .ok();
                let closed = closed.clone();
                sheet
                    .size(SIDEBAR)
                    .resizable(false)
                    .children(content)
                    .on_close(move |_, _, cx| {
                        let _ = closed.update(cx, |view, cx| {
                            view.drawer_open = false;
                            cx.notify();
                        });
                    })
            });
            let focus = self.drawer_focus.clone();
            cx.defer_in(window, move |_, window, cx| focus.focus(window, cx));
        } else if placement != Placement::Drawer && window.has_active_sheet(cx) {
            window.close_sheet(cx);
            match self.field_focus(cx) {
                Some(field) if self.dock_request.is_none() => field.focus(window, cx),
                _ => self.root.focus(window, cx),
            }
        }

        let theme = cx.theme();
        let (bg, fg, font) = (
            theme.background,
            theme.foreground,
            theme.font_family.clone(),
        );
        // Everything that belongs to one thread sits under an id made from the thread's id,
        // so per-element state (a tool card left open, the scroll position, a streaming reply)
        // can never carry over to another thread whose rows have the same numbers.
        let main = v_flex()
            .id((ElementId::from("thread"), thread_id))
            .flex_1()
            .min_w_0()
            .h_full()
            .child(title_bar)
            .child(body)
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .flex()
                    .justify_center()
                    .px(px(DOCK_METRICS.page_padding))
                    .pt(px(DOCK_METRICS.top_padding))
                    .pb(px(DOCK_METRICS.bottom_padding))
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(COLUMN_WIDTH)
                            .gap(px(DOCK_METRICS.scope_gap))
                            .children(problem)
                            .child(bottom),
                    ),
            );

        // Our key context and shortcut handlers. Tab is the window root's (Kit); focus that
        // falls out of the window comes back through `reclaim_focus`.
        div()
            .key_context(keys::CONTEXT)
            .track_focus(&self.root)
            .size_full()
            .capture_action(
                cx.listener(|view, _: &gpui_kit::base::input::Escape, _, cx| {
                    if view.drawer_open {
                        view.drawer_open = false;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }),
            )
            .capture_key_down(cx.listener(Self::on_key_capture))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::on_stop))
            .on_action(cx.listener(Self::on_new_thread))
            .on_action(cx.listener(Self::on_previous))
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_jump))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .flex()
            .bg(bg)
            .text_color(fg)
            .font_family(font)
            .children(docked)
            .child(main)
    }
}

/// Whether a Kit menu or popover holds the keyboard (a row's "Archive thread", the "N allowed"
/// list). Then Esc closes it, and ⌘↵ must not allow a request the user is not looking at.
fn overlay_focused(window: &Window) -> bool {
    window
        .context_stack()
        .iter()
        .any(|context| context.contains("PopupMenu") || context.contains("PopoverContent"))
}

/// Ticks every `TICK` for as long as the view lives (see `ChatView::tick`).
fn clock(cx: &mut Context<ChatView>) -> Task<()> {
    cx.spawn(async move |view, cx| {
        let mut ticks: u64 = 0;
        loop {
            cx.background_executor().timer(TICK).await;
            ticks += 1;
            if view.update(cx, |view, cx| view.tick(ticks, cx)).is_err() {
                break;
            }
        }
    })
}

/// Listens for failed writes to the store (it has one listener: each error is delivered once).
fn save_problems(store: &Store, cx: &mut Context<ChatView>) -> Task<()> {
    let errors = store.write_errors();
    cx.spawn(async move |view, cx| {
        while let Ok(error) = errors.recv().await {
            let shown = view.update(cx, |view, cx| view.save_failed(error.to_string(), cx));
            if shown.is_err() {
                break;
            }
        }
    })
}

/// What a thread's row in the list needs to know about it at `now`.
fn row_facts(thread: &Thread, now: Instant) -> RowFacts {
    let transcript = thread.transcript();
    let status = thread.status();
    // Only a failed thread needs its error, so the others never search their transcript.
    let failure = (status == Status::Failed)
        .then(|| {
            transcript.items().iter().rev().find_map(|item| match item {
                Item::Error { title, detail, .. } => Some(sidebar::failure_reason(title, detail)),
                _ => None,
            })
        })
        .flatten();
    RowFacts {
        status,
        draft: thread.is_draft(),
        unread: thread.is_unread(),
        waiting_tool: transcript
            .pending_approval()
            .map(|request| display_name(&request.tool_name)),
        working_for: thread
            .turn_started_at()
            .map(|started| now.saturating_duration_since(started)),
        failure,
        stopped_on_quit: matches!(transcript.items().last(), Some(Item::Stopped { .. })),
        background_tasks: thread.background_tasks().len(),
        updated_at: thread.updated_at(),
    }
}

/// The quiet problem line after a failed write, given the `line` now shown (if any) and whether
/// a failed write was already shown (`seen`). The first failed write takes the line, even over
/// a "Couldn't load threads" line, which says nothing about saving. Later ones change nothing:
/// the next save writes their rows again anyway. Returns the new line and `seen`.
fn after_write_error(line: Option<String>, seen: bool, error: String) -> (Option<String>, bool) {
    if seen {
        (line, true)
    } else {
        (Some(error), true)
    }
}

fn removal_problem(path: &str, error: &anyhow::Error) -> String {
    format!("Couldn't remove the Rift copy at {path}: {error:#}")
}

/// A draft with nothing typed in its composer: switching away drops it, and ⌘N on it just
/// focuses it rather than making another (see `newjen_core::thread::keeps_draft`).
fn is_empty_draft(draft: bool, composer_text: &str) -> bool {
    draft && !keeps_draft(composer_text)
}

/// The error row shown when `claude` could not be started, worded by what went wrong.
fn spawn_notice(error: &anyhow::Error, program: &Path) -> Notice {
    let program = program.display();
    let kind = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<io::Error>())
        .map(io::Error::kind);
    let detail = match kind {
        // The project folder is checked at startup, so this is the program (unless the folder
        // was deleted since).
        Some(io::ErrorKind::NotFound) => format!(
            "Newjen could not find `{program}`. Install Claude Code and run `claude` once in a \
             terminal to log in, then try again. Set NEWJEN_CLAUDE to use another path."
        ),
        Some(io::ErrorKind::PermissionDenied) => format!(
            "`{program}` is not allowed to run ({error:#}). Check that it is an executable \
             file, then try again."
        ),
        _ => format!("{error:#}.\n\nCheck that `{program}` runs in a terminal, then try again."),
    };
    Notice::error("Could not start Claude Code", detail)
}

/// While Claude is working or waiting on an approval, the composer shows the stop button and
/// sending is blocked. Any other status (including Failed) leaves the composer usable.
fn is_busy(status: Status) -> bool {
    matches!(status, Status::Working | Status::Waiting)
}

/// The notice after writing a prompt, answer or stop to Claude. If it was sent, none: an older
/// notice (say, an earlier failed write) is now out of date. If not, an error titled `failed`.
fn notice_after(sent: anyhow::Result<()>, failed: &str) -> Option<Notice> {
    let error = sent.err()?;
    Some(Notice::error(
        failed,
        format!("{error:#}. Try again to restart the session."),
    ))
}

/// What to send for the user's `choice`. When the dock could not show the remembered rule in
/// full (`rule_cut`), "Always allow" is sent as allow once: nothing is remembered that the user
/// could not read.
fn sent_answer(choice: PermissionAnswer, rule_cut: bool) -> PermissionAnswer {
    match choice {
        PermissionAnswer::AllowForSession if rule_cut => PermissionAnswer::AllowOnce,
        other => other,
    }
}

/// Whether row `ix` gets a "Try again" button. Rows are the transcript's `item_count` items,
/// then the notice if there is one.
fn offers_retry(ix: usize, item_count: usize, notice: Option<&Notice>, busy: bool) -> bool {
    let row_count = item_count + usize::from(notice.is_some());
    ix < row_count && rows::can_retry(ix + 1 == row_count, ix >= item_count, busy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use newjen_core::{SessionEvent, Transcript};

    #[test]
    fn always_allow_on_a_cut_rule_is_sent_as_allow_once() {
        let always = PermissionAnswer::AllowForSession;
        assert_eq!(sent_answer(always, true), PermissionAnswer::AllowOnce);
        // Shown in full: remembered as asked.
        assert_eq!(sent_answer(always, false), always);
        // Other answers are never changed.
        assert_eq!(
            sent_answer(PermissionAnswer::Deny, true),
            PermissionAnswer::Deny
        );
    }

    #[test]
    fn a_load_problem_does_not_hide_the_first_failed_save() {
        let load = Some("Couldn't load threads: bad row".to_string());
        let full = "Couldn't save threads: disk full".to_string();
        // Launched with a load problem on screen; the first failed save still shows.
        let (line, seen) = after_write_error(load, false, full.clone());
        assert_eq!(line.as_deref(), Some(full.as_str()));
        assert!(seen);
        // Later failures change nothing, whether the line is still up or was dismissed.
        let later = "Couldn't save threads: again".to_string();
        assert_eq!(
            after_write_error(line, seen, later.clone()),
            (Some(full), true)
        );
        assert_eq!(after_write_error(None, true, later), (None, true));
        // With nothing on screen, the first failure shows.
        let (line, _) = after_write_error(None, false, "Couldn't save threads: x".to_string());
        assert_eq!(line.as_deref(), Some("Couldn't save threads: x"));
    }

    #[test]
    fn a_failed_removal_names_the_copy_and_why() {
        let error = anyhow::anyhow!("`rift remove` failed: permission denied");
        assert_eq!(
            removal_problem("~/.rifts/app/newjen-1a2b3c4d", &error),
            "Couldn't remove the Rift copy at ~/.rifts/app/newjen-1a2b3c4d: `rift remove` failed: \
             permission denied"
        );
    }

    #[test]
    fn only_an_empty_draft_is_dropped_or_reused() {
        assert!(is_empty_draft(true, ""));
        assert!(is_empty_draft(true, "  \n "));
        // Something typed: the draft stays in the list with its text.
        assert!(!is_empty_draft(true, "half a thought"));
        // A saved thread is never dropped, whatever its composer holds.
        assert!(!is_empty_draft(false, ""));
    }

    #[test]
    fn an_interrupt_timeout_leaves_the_composer_usable_with_try_again() {
        let mut transcript = Transcript::new();
        transcript.push_user("hello");
        transcript.apply(&SessionEvent::InterruptTimedOut);
        // The session shuts claude down right after; that must not change the outcome.
        transcript.apply(&SessionEvent::Exited {
            code: None,
            requested: true,
        });
        let busy = is_busy(transcript.status());
        assert!(!busy, "the composer must not stay on \"Working\"");
        let items = transcript.items();
        let last = items.len() - 1;
        assert!(matches!(
            &items[last],
            Item::Error { title, .. } if title == "Claude didn't stop, so Newjen ended the session"
        ));
        assert!(offers_retry(last, items.len(), None, busy));
    }

    #[test]
    fn an_error_notice_is_the_last_row_and_keeps_its_button() {
        // Two transcript items, then the notice; even mid-turn it keeps "Try again".
        let error = Notice::error("Could not reach Claude", String::new());
        assert!(offers_retry(2, 2, Some(&error), true));
        assert!(!offers_retry(1, 2, Some(&error), false));
    }

    #[test]
    fn a_sent_answer_or_stop_clears_the_notice() {
        assert!(notice_after(Ok(()), "Could not stop Claude").is_none());
        let failed = notice_after(
            Err(anyhow::anyhow!("claude has exited")),
            "Could not stop Claude",
        )
        .expect("a failed write leaves a notice");
        assert_eq!(failed.title, "Could not stop Claude");
        assert!(failed.detail.starts_with("claude has exited. Try again"));
    }

    #[test]
    fn a_failed_write_leaves_the_composer_usable_with_try_again() {
        let mut transcript = Transcript::new();
        transcript.push_user("hello");
        assert!(is_busy(transcript.status()));
        transcript.apply(&SessionEvent::WriteFailed(
            "Broken pipe (os error 32)".to_string(),
        ));
        let busy = is_busy(transcript.status());
        assert!(!busy, "the composer must not stay on \"Working\"");
        let items = transcript.items();
        let last = items.len() - 1;
        assert!(matches!(items[last], Item::Error { .. }));
        assert!(offers_retry(last, items.len(), None, busy));
    }

    fn failed(kind: io::ErrorKind) -> anyhow::Error {
        // The same shape `Session::spawn` returns: the OS error with context around it.
        anyhow::Error::new(io::Error::from(kind)).context("could not start `claude` in /work")
    }

    #[test]
    fn spawn_notice_names_the_actual_problem() {
        let program = Path::new("/opt/bin/claude");
        let missing = spawn_notice(&failed(io::ErrorKind::NotFound), program);
        assert!(missing.detail.contains("could not find `/opt/bin/claude`"));
        assert!(missing.detail.contains("Install Claude Code"));

        let denied = spawn_notice(&failed(io::ErrorKind::PermissionDenied), program);
        assert!(denied.detail.contains("not allowed to run"));
        assert!(!denied.detail.contains("Install Claude Code"));

        let other = spawn_notice(&failed(io::ErrorKind::OutOfMemory), program);
        assert!(
            other
                .detail
                .starts_with("could not start `claude` in /work")
        );
        assert!(!other.detail.contains("Install Claude Code"));
    }
}

#[cfg(test)]
mod input_tests;
