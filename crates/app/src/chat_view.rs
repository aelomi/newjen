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

mod limits;
mod picker;
mod review;

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
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Side, WindowExt as _};
use newjen_core::agent::NoteDelivery;
use newjen_core::rift::{self, CopyError, Tools};
use newjen_core::store::{PendingCopy, PendingIntent, PendingState, now_ms};
use newjen_core::thread::{self, CopyJob, NotSent, Quitting, Sent, WorksIn, keeps_draft};
use newjen_core::{ApprovalIntent, Item, Project, SessionConfig, Status, Store, Thread, ToolState};

use crate::approval::{self, Room};
use crate::format::{elapsed, fit_rows, short_path, text_rows, tilde};
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
use gpui_kit::component::message_scroller::MessageScrollerState;

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

/// While a Rift copy is made, a hook line redraws the window at most this often. The clock
/// redraws it every second anyway.
const LINE_REDRAW: Duration = Duration::from_millis(100);
const COPY_CANCELLING: &str = "Cancelling… Rift is being stopped";
const COPY_OUTPUT_LINES: usize = 8;
const COPY_FAILED: &str = "Couldn't create the Rift copy";

/// A message from the app itself (not reported by Claude), shown as the thread's last row. It
/// is cleared by the next message, answer or stop that reaches Claude.
struct Notice {
    kind: NoticeKind,
    title: String,
    detail: String,
}

enum NoticeKind {
    /// Try again starts a fresh `claude`.
    Error,
    /// Try again sends the composer's text again: the message went back there.
    CopyFailed {
        output: Vec<String>,
    },
    /// Nothing failed and nothing was sent, so no Try again.
    Held,
    Snapshot,
}

impl Notice {
    fn error(title: &str, detail: String) -> Notice {
        Notice {
            kind: NoticeKind::Error,
            title: title.to_string(),
            detail,
        }
    }

    fn held(title: &str, detail: String) -> Notice {
        Notice {
            kind: NoticeKind::Held,
            title: title.to_string(),
            detail,
        }
    }
}

/// What one row of the message list shows: the transcript's items, then, while the thread's
/// Rift copy is made, the message waiting for it and the row that says so, then the notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry {
    Item(usize),
    HeldMessage,
    MakingCopy,
    Notice,
}

impl Entry {
    fn at(ix: usize, items: usize, making_copy: bool, notice: bool) -> Option<Entry> {
        let held = if making_copy { 2 } else { 0 };
        match ix.checked_sub(items) {
            None => Some(Entry::Item(ix)),
            Some(0) if making_copy => Some(Entry::HeldMessage),
            Some(1) if making_copy => Some(Entry::MakingCopy),
            Some(after) if after == held && notice => Some(Entry::Notice),
            Some(_) => None,
        }
    }

    fn count(items: usize, making_copy: bool, notice: bool) -> usize {
        items + if making_copy { 2 } else { 0 } + usize::from(notice)
    }
}

enum CopyProgress {
    Progress(rift::Progress),
    Made(Result<rift::Copy, CopyError>),
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
    messages: Entity<MessageScrollerState>,
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
    project_chooser: Option<OnPress>,
    visible: bool,
    copy_peers: Vec<gpui::WeakEntity<ChatView>>,
    store: Store,
    project: Project,
    base_config: SessionConfig,
    picker: picker::Picker,
    picker_focus: FocusHandle,
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
    save_problem: Option<String>,
    /// Focus target while an approval is pending (the dock replaces the composer, so the field
    /// is not on screen), so ⌘↵ and Esc reach us.
    root: FocusHandle,
    /// The "N allowed" popover is open, and the focus it takes while it is.
    grants_open: bool,
    grants_focus: FocusHandle,
    /// The approval the dock is showing, to notice when a new one takes its place.
    dock_request: Option<newjen_core::ApprovalKey>,
    /// The optional reason for a denial, typed under the approval card (see
    /// `review::prepare_approval`).
    denial_note: Option<Entity<TextareaState>>,
    edit_preview: Option<review::ApprovalPreview>,
    review_panel: Option<review::ReviewPanel>,
    review_loading: bool,
    /// The root has the keyboard while the thread on screen waits for its Rift copy: the
    /// composer is disabled until then.
    making_copy_shown: bool,
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
        let tasks = vec![clock(cx)];
        let grants_focus = cx.focus_handle();
        let focus = [
            cx.on_focus_lost(window, |view, window, cx| view.reclaim_focus(window, cx)),
            cx.on_focus_out(&grants_focus, window, |view, _, _, cx| {
                view.grants_open = false;
                cx.notify();
            }),
        ];
        let mut view = ChatView {
            project_chooser: None,
            visible: true,
            copy_peers: vec![],
            store,
            project,
            base_config,
            picker: picker::Picker::default(),
            picker_focus: cx.focus_handle(),
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
            save_problem: problem,
            root: cx.focus_handle(),
            grants_open: false,
            grants_focus,
            dock_request: None,
            denial_note: None,
            edit_preview: None,
            review_panel: None,
            review_loading: false,
            making_copy_shown: false,
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
        view.restore_pending_copies(window, cx);
        #[cfg(not(test))]
        view.refresh_providers(window, cx);
        view
    }

    fn restore_pending_copies(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = match self.store.pending_copies(self.project.id) {
            Ok(pending) => pending,
            Err(error) => {
                self.save_problem = Some(format!("Couldn't recover Rift copies: {error:#}"));
                return;
            }
        };
        let store = self.store.clone();
        let project = self.project.clone();
        let tools = self.tools.clone();
        let cleanup = cx.background_spawn(async move {
            let cleanup = Thread::recover_copy_cleanup(&store, &project, &tools);
            let drafts = pending
                .into_iter()
                .map(|pending| {
                    let result = Thread::recover_pending_copy(&store, &project, &pending, &tools);
                    (pending, result)
                })
                .collect::<Vec<_>>();
            (cleanup, drafts)
        });
        cx.spawn_in(window, async move |view, cx| {
            let (cleanup, recovered) = cleanup.await;
            let _ = view.update_in(cx, |view, window, cx| {
                if let Err(error) = cleanup {
                    view.save_problem =
                        Some(format!("Couldn't remove unfinished Rift copies: {error:#}"));
                }
                for (pending, result) in recovered {
                    match result {
                        Ok(thread::CopyRecovery::Kept(path)) => {
                            view.save_problem = Some(format!(
                                "Newjen kept the Rift copy at {}: its thread couldn't be saved.",
                                path.display()
                            ));
                        }
                        Err(error) => {
                            view.save_problem = Some(format!(
                                "Couldn't remove the unfinished Rift copy: {error:#}"
                            ))
                        }
                        _ => {}
                    }
                    if pending.state != PendingState::Sent {
                        view.restore_pending_draft(pending, window, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn restore_pending_draft(
        &mut self,
        pending: PendingCopy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let PendingIntent::Restore(message) = &pending.intent else {
            return;
        };
        let draft = Thread::recovered_draft(
            self.store.clone(),
            &self.project,
            self.base_config.clone(),
            &pending,
        );
        self.add_slot(draft, 0, window, cx);
        self.active += 1;
        self.show(0, window, cx);
        let field = self.ensure_field(window, cx);
        field.update(cx, |field, cx| field.set_value(message.clone(), window, cx));
        if pending.state == PendingState::MakingCopy {
            self.slots[0].notice = Some(Notice::held(
                "Newjen quit while making a Rift copy. Your message is back.",
                String::new(),
            ));
        }
        cx.notify();
    }

    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible {
            self.enter(window, cx);
        } else {
            self.thread()
                .clone()
                .update(cx, |thread, _| thread.set_active(false));
            if self.drawer_open {
                window.close_sheet(cx);
            }
            self.drawer_open = false;
            self.grants_open = false;
            window.close_all_dialogs(cx);
        }
        cx.notify();
    }

    pub fn set_copy_peers(&mut self, peers: Vec<gpui::WeakEntity<ChatView>>) {
        self.copy_peers = peers;
    }

    pub fn set_project_chooser(&mut self, chooser: OnPress) {
        self.project_chooser = Some(chooser);
    }

    /// When the focused element leaves the window (an overlay closed without handing focus
    /// back, say): the composer takes it, or the root while the dock replaces the composer, so
    /// the shortcuts keep working.
    fn reclaim_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        // Deferred: GPUI draws no new frame for focus that a focus-lost listener moves, so the
        // composer would not show its focus until the next redraw.
        cx.defer_in(window, |view, window, cx| {
            if !view.visible {
                return;
            }
            match view.field_focus(cx) {
                Some(field) if view.composer_has_keys() => window.focus(&field, cx),
                _ => window.focus(&view.root, cx),
            }
        });
    }

    fn composer_has_keys(&self) -> bool {
        self.dock_request.is_none() && !self.making_copy_shown
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
    pub fn begin_quit(&mut self, cx: &mut Context<Self>) -> Vec<Quitting> {
        for (ix, slot) in self.slots.iter().enumerate() {
            if let Err(error) = slot.thread.read(cx).save_draft(&self.composer_text(ix, cx)) {
                self.save_problem = Some(format!("Couldn't save the draft: {error:#}"));
            }
        }
        self.slots
            .iter()
            .map(|slot| slot.thread.update(cx, |thread, _| thread.begin_quit()))
            .collect()
    }

    #[cfg(test)]
    pub fn quit(&mut self, cx: &mut Context<Self>) {
        let quitting = self.begin_quit(cx);
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
            messages: cx.new(|cx| MessageScrollerState::new(0, cx)),
            _pump: pump,
            _changes: changes,
        };
        let at = at.min(self.slots.len());
        self.slots.insert(at, slot);
        self.sync_replies(at, cx);
        let thread = self.slots[at].thread.clone();
        self.run_snapshot(&thread, window, cx);
    }

    /// A new, unsaved thread at the top of the list. Not shown yet; see `new_thread`.
    fn add_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut draft =
            Thread::new_draft(self.store.clone(), &self.project, self.base_config.clone());
        self.restore_draft_agent(&mut draft);
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
        if !self
            .slots
            .iter()
            .any(|slot| slot.thread.read(cx).blocks_project_send())
        {
            for slot in &mut self.slots {
                if matches!(
                    slot.notice.as_ref().map(|notice| &notice.kind),
                    Some(NoticeKind::Snapshot)
                ) {
                    slot.notice = None;
                    let thread = slot.thread.read(cx);
                    let count = Entry::count(
                        thread.transcript().items().len(),
                        thread.preparing().is_some(),
                        false,
                    );
                    slot.messages
                        .update(cx, |messages, cx| messages.reset(count, cx));
                }
            }
        }
        if let Some(ix) = self.slots.iter().position(|slot| slot.thread == *thread) {
            self.sync_replies(ix, cx);
            self.run_snapshot(thread, window, cx);
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
        if self.review_loading || self.review_panel.as_ref().is_some_and(|panel| panel.busy) {
            return;
        }
        self.review_panel = None;
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
        if !self.visible {
            return;
        }
        self.sync_replies(self.active, cx);
        // No `notify` on the thread: this view redraws itself below. (It also runs while the
        // view is being built, when nothing can observe the thread yet.)
        self.thread()
            .clone()
            .update(cx, |thread, _| thread.set_active(true));
        // As if no dock was up: a waiting approval on this thread then takes the keyboard (and
        // gets the short key delay) exactly as a new one would.
        self.dock_request = None;
        self.making_copy_shown = false;
        self.sync_field(window, cx);
        if self.composer_has_keys() {
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
        // Archiving now would leave the copy being made behind with no thread.
        if self.slots[ix].thread.read(cx).preparing().is_some() {
            return;
        }
        if self.hold_archive(ix, cx) {
            return;
        }
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

    /// A thread in a Rift copy asks first: archiving it deletes the copy with Claude's work.
    fn ask_to_archive(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(thread_id, cx) else {
            return;
        };
        if self.hold_archive(ix, cx) {
            return;
        }
        if self.slots[ix].thread.read(cx).review_base().is_some()
            && !self.slots[ix].thread.read(cx).landing_finished()
        {
            self.show_thread(thread_id, window, cx);
            self.open_review(window, cx);
            return;
        }
        let question = {
            let thread = self.slots[ix].thread.read(cx);
            thread.copy().map(|copy| {
                archive_question(
                    thread.title(),
                    &tilde(&copy.path, self.home.as_deref()),
                    thread.presentation(),
                )
            })
        };
        let Some((title, body)) = question else {
            self.archive(thread_id, window, cx);
            return;
        };
        let view = cx.weak_entity();
        let id = thread_id.to_string();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            let id = id.clone();
            // Return closes the dialog and keeps the copy: deleting a copy takes a click.
            alert
                .title(SharedString::from(title.clone()))
                .description(SharedString::from(body.clone()))
                .on_ok(|_, _, _| true)
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("keep-copy")
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            div().debug_selector(|| "delete-copy".into()).child(
                                Button::new("delete-copy")
                                    .label("Archive and delete")
                                    .danger()
                                    .on_click(move |event, window, cx| {
                                        if matches!(event, gpui::ClickEvent::Mouse(_)) {
                                            let _ = view.update(cx, |view, cx| {
                                                view.archive(&id, window, cx)
                                            });
                                            window.close_dialog(cx);
                                        }
                                    }),
                            ),
                        ),
                )
        });
    }

    fn hold_archive(&mut self, ix: usize, cx: &mut Context<Self>) -> bool {
        let Some(problem) = self.archive_blocker(ix, cx) else {
            return false;
        };
        self.save_problem = Some(problem);
        cx.notify();
        true
    }

    fn archive_blocker(&self, ix: usize, cx: &App) -> Option<String> {
        let target = &self.slots[ix].thread;
        let folder = &target.read(cx).copy()?.path;
        let thread_using_folder = |view: &ChatView| {
            view.slots.iter().find_map(|slot| {
                if slot.thread == *target {
                    return None;
                }
                let thread = slot.thread.read(cx);
                thread.cwd().starts_with(folder).then(|| {
                    format!(
                        "Couldn't archive this thread. Its Rift copy is used by thread “{}”. \
                         Archive that thread before deleting this copy.",
                        thread.title()
                    )
                })
            })
        };
        if let Some(problem) = thread_using_folder(self) {
            return Some(problem);
        }
        for peer in &self.copy_peers {
            let Some(peer) = peer.upgrade() else { continue };
            let peer = peer.read(cx);
            if peer.project.path.starts_with(folder) {
                return Some(format!(
                    "Couldn't archive this thread. Its Rift copy is open as project {}. \
                     Quit Newjen and reopen only the original project before archiving this copy.",
                    tilde(&peer.project.path, self.home.as_deref())
                ));
            }
            if let Some(problem) = thread_using_folder(peer) {
                return Some(problem);
            }
        }
        None
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
        let busy = self.slots.iter().any(|slot| {
            let thread = slot.thread.read(cx);
            thread.status().is_busy() || thread.preparing().is_some()
        });
        let limited = self.thread().read(cx).limit_event().is_some();
        if minute || busy || limited {
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
        if !self.visible {
            return;
        }
        let field = self.ensure_field(window, cx);
        let (front, outside, status, empty, making_copy) = {
            let thread = self.thread().read(cx);
            let request = thread.transcript().pending_approval();
            let front = request.map(|request| request.key());
            // Only for a new request: it reads the disk.
            let outside = if front != self.dock_request {
                request.and_then(|request| approval::outside_path(request.view(), thread.cwd()))
            } else {
                self.dock_outside.clone()
            };
            let empty = thread.transcript().items().is_empty();
            let making_copy = thread.preparing().is_some();
            (front, outside, thread.status(), empty, making_copy)
        };
        if front != self.dock_request {
            self.prepare_approval(window, cx);
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
        if making_copy != self.making_copy_shown {
            self.making_copy_shown = making_copy;
            if self.dock_request.is_none() {
                if making_copy {
                    window.focus(&self.root, cx);
                } else {
                    let handle = field.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
            }
        }

        let no_agent = self.picker.checked.is_some()
            && !self
                .picker
                .statuses
                .iter()
                .any(|s| !newjen_core::picker::model_rows(s).is_empty());
        let paused = self.thread().read(cx).paused_until(now_ms() / 1000);
        let placeholder = SharedString::from(match status {
            _ if paused.is_some() => format!(
                "Paused until {}",
                newjen_core::picker::reset_time(paused.expect("checked"), now_ms() / 1000)
            ),
            _ if no_agent && self.thread().read(cx).is_draft() => "No agent to send to yet".into(),
            _ if making_copy => self
                .thread()
                .read(cx)
                .presentation()
                .reply_placeholder
                .to_string(),
            Status::Working | Status::Waiting => self
                .thread()
                .read(cx)
                .presentation()
                .working_placeholder
                .to_string(),
            _ if empty => self
                .thread()
                .read(cx)
                .presentation()
                .new_thread_placeholder(&self.project_label),
            _ => self
                .thread()
                .read(cx)
                .presentation()
                .reply_placeholder
                .to_string(),
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
        let thread = self.thread().read(cx);
        is_busy(thread.status())
            || thread.preparing().is_some()
            || thread.work_pending()
            || self.review_loading
            || self.review_panel.is_some()
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
        if self.review_loading || self.review_panel.is_some() {
            return false;
        }
        if thread.read(cx).works_in() == WorksIn::ProjectFolder
            && self
                .slots
                .iter()
                .any(|slot| slot.thread != thread && slot.thread.read(cx).owns_project_work())
        {
            self.slots[self.active].notice = Some(Notice::held("Review the project thread first", "Land or discard the existing project-folder work before starting another thread here.".into()));
            cx.notify();
            return false;
        }
        if let Some(held) = self.copy_hold(&thread, cx) {
            self.slots[self.active].notice = Some(held);
            cx.notify();
            return false;
        }
        let (had_claude, program, gone) = {
            let thread = thread.read(cx);
            let gone = self.gone_copy(thread);
            (thread.has_session(), thread.session_config().program, gone)
        };
        let sent = thread.update(cx, |thread, cx| {
            let sent = thread.send(text);
            cx.notify();
            sent
        });
        let went_out = sent.is_ok();
        let notice = match sent {
            // Writing worked, so an older "could not reach Claude" notice is out of date.
            Ok(Sent::Started) => None,
            Ok(Sent::PreparingProject(job)) => {
                self.prepare_project(&thread, job, window, cx);
                None
            }
            Ok(Sent::MakingCopy(job)) => {
                self.make_copy(&thread, job, window, cx);
                None
            }
            Err(error) if error.downcast_ref::<CopyError>().is_some() => Some(copy_failed(
                error.downcast_ref::<CopyError>().expect("checked"),
                self.home.as_deref(),
            )),
            Err(error) if had_claude => {
                notice_after(Err(error), thread.read(cx).presentation().send_failed)
            }
            // No claude was running, so the error is from starting one.
            Err(error) => Some(spawn_notice(
                &error,
                &program,
                thread.read(cx).presentation(),
                gone.as_deref(),
            )),
        };
        self.slots[self.active].notice = notice;
        self.sync_field(window, cx);
        cx.notify();
        went_out
    }

    /// Checked before `Thread::send`, which would hold the message and rename the thread.
    /// Why `thread` cannot send yet, if it can't: a draft that wants a Rift copy waits for
    /// threads busy in the project folder (and for another copy being made), and a
    /// project-folder send waits while a copy's jj snapshot of the project runs. Threads of
    /// other open projects count when they share the repository.
    fn copy_hold(&self, thread: &Entity<Thread>, cx: &App) -> Option<Notice> {
        let (project_folder, wants_copy) = {
            let thread = thread.read(cx);
            (
                thread.works_in() == WorksIn::ProjectFolder,
                thread.is_draft()
                    && thread.works_in() == WorksIn::RiftCopy
                    && thread.copy().is_none(),
            )
        };
        if !project_folder && !wants_copy {
            return None;
        }
        let source = rift::project_root(thread.read(cx).cwd()).0;
        let jj_repo = in_jj_repo(&source);
        if wants_copy && let Some(missing) = missing_tool(&self.tools, jj_repo) {
            return Some(missing);
        }
        let blocks_send = |other: &Thread| {
            let busy = if project_folder {
                other.blocks_project_send()
            } else {
                other.preparing().is_some()
                    || other.status().is_busy()
                    || !other.background_tasks().is_empty()
            };
            if !busy {
                return false;
            }
            let other_source = rift::project_root(other.cwd()).0;
            source.starts_with(&other_source) || other_source.starts_with(&source)
        };
        let hold = |blocker: &Thread| {
            if project_folder {
                Notice {
                    kind: NoticeKind::Snapshot,
                    title: "Wait a moment".to_string(),
                    detail: format!(
                        "Newjen is taking a jj snapshot of the project to make the Rift copy for “{}”.",
                        blocker.title()
                    ),
                }
            } else {
                refusal(blocker.title(), blocker.preparing().is_some(), jj_repo)
            }
        };
        if let Some(blocker) = self
            .slots
            .iter()
            .filter(|slot| slot.thread != *thread)
            .map(|slot| slot.thread.read(cx))
            .find(|other| blocks_send(other))
        {
            return Some(hold(blocker));
        }
        for peer in &self.copy_peers {
            let Some(peer) = peer.upgrade() else { continue };
            for slot in &peer.read(cx).slots {
                let other = slot.thread.read(cx);
                if blocks_send(other) {
                    return Some(hold(other));
                }
            }
        }
        None
    }

    fn gone_copy(&self, thread: &Thread) -> Option<String> {
        thread
            .copy()
            .filter(|copy| !copy.workdir.exists())
            .map(|copy| tilde(&copy.path, self.home.as_deref()))
    }

    fn make_copy(
        &mut self,
        thread: &Entity<Thread>,
        job: CopyJob,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tools = self.tools.clone();
        let (progress, updates) = async_channel::unbounded();
        let lines = progress.clone();
        cx.background_spawn(async move {
            let made = job.run(&tools, |progress| {
                let _ = lines.send_blocking(CopyProgress::Progress(progress));
            });
            let _ = progress.send_blocking(CopyProgress::Made(made));
        })
        .detach();
        let thread = thread.clone();
        cx.spawn_in(window, async move |view, cx| {
            let mut redrawn: Option<Instant> = None;
            let made = loop {
                match updates.recv().await {
                    Ok(CopyProgress::Progress(progress)) => {
                        let now = Instant::now();
                        let redraw = redrawn.is_none_or(|at| now - at >= LINE_REDRAW);
                        if redraw {
                            redrawn = Some(now);
                        }
                        thread.update(cx, |thread, cx| {
                            thread.note_copy_progress(progress);
                            if redraw {
                                cx.notify();
                            }
                        });
                    }
                    Ok(CopyProgress::Made(made)) => break made,
                    Err(_) => {
                        break Err(CopyError::Failed {
                            command: "rift create".to_string(),
                            detail: "Newjen stopped before the copy was made".to_string(),
                            output: Vec::new(),
                        });
                    }
                }
            };
            let _ = view.update_in(cx, |view, window, cx| {
                view.copy_made(&thread, made, window, cx)
            });
        })
        .detach();
    }

    fn copy_made(
        &mut self,
        thread: &Entity<Thread>,
        made: Result<rift::Copy, CopyError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let program = thread.read(cx).session_config().program;
        let finished = thread.update(cx, |thread, cx| {
            let finished = thread.finish_copy(made);
            cx.notify();
            finished
        });
        if let Err(NotSent {
            copy_to_remove: Some(copy),
            ..
        }) = &finished
        {
            let copy = copy.clone();
            let tools = self.tools.clone();
            let path = tilde(&copy.path, self.home.as_deref());
            let removed = cx.background_spawn(async move {
                rift::remove(&tools, &copy.source, &copy.path, &copy.rift_id)
            });
            cx.spawn(async move |view, cx| {
                if let Err(error) = removed.await {
                    let _ = view.update(cx, |view, cx| {
                        view.save_problem = Some(removal_problem(&path, &error.into()));
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        let Some(ix) = self.slots.iter().position(|slot| slot.thread == *thread) else {
            return;
        };
        let notice = match finished {
            Ok(()) => None,
            Err(NotSent { message, error, .. }) => {
                if let Some(field) = self.slots[ix].field.clone().filter(|_| !message.is_empty()) {
                    field.update(cx, |field, cx| field.set_value(message, window, cx));
                }
                match error.downcast_ref::<CopyError>() {
                    Some(CopyError::Cancelled) => None,
                    Some(failed) => Some(copy_failed(failed, self.home.as_deref())),
                    None => {
                        let gone = self.gone_copy(thread.read(cx));
                        Some(spawn_notice(
                            &error,
                            &program,
                            thread.read(cx).presentation(),
                            gone.as_deref(),
                        ))
                    }
                }
            }
        };
        self.slots[ix].notice = notice;
        if ix == self.active {
            self.sync_field(window, cx);
        }
        cx.notify();
    }

    fn choose_works_in(&mut self, works_in: WorksIn, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread().clone();
        if thread
            .update(cx, |thread, _| thread.set_works_in(works_in))
            .is_err()
        {
            return;
        }
        let missing = match works_in {
            WorksIn::RiftCopy => missing_tool(&self.tools, in_jj_repo(&self.project.path)),
            WorksIn::ProjectFolder => None,
        };
        self.slots[self.active].notice = missing;
        if let Some(field) = self.field_focus(cx) {
            window.focus(&field, cx);
        }
        cx.notify();
    }

    /// Answers the pending approval. `request_id` is the request the button belonged to; a
    /// stale click (that request was already answered) does nothing. `rule_cut` says the dock
    /// could not show the remembered rule in full (`ScopeRow::once`), so "Always allow" is sent
    /// as allow once (see `sent_answer`).
    fn send_answer(
        &mut self,
        request_id: Option<newjen_core::ApprovalKey>,
        choice: ApprovalIntent,
        rule_cut: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = sent_answer(choice, rule_cut);
        let thread = self.thread().clone();
        let note = self
            .denial_note
            .as_ref()
            .map(|field| field.read(cx).value().to_string());
        let sent = thread.update(cx, |thread, cx| {
            let sent = thread.answer_with_note(request_id, answer, note.as_deref());
            cx.notify();
            sent
        });
        // No note after the click when "Always allow" acts once: the scope row under the dock
        // already said so before the click.
        self.slots[self.active].notice = match sent {
            Ok(NoteDelivery::Delivered) => None,
            // The provider's denial carries no text: the note waits in the composer instead.
            Ok(NoteDelivery::Kept(note)) => {
                let label = thread.read(cx).presentation().label;
                let field = self.ensure_field(window, cx);
                field.update(cx, |field, cx| field.set_value(note, window, cx));
                Some(Notice::held(
                    "The note was not sent",
                    format!("{label} can't take a note with a denial; send it as a message."),
                ))
            }
            Err(error) => notice_after(Err(error), thread.read(cx).presentation().answer_failed),
        };
        self.sync_field(window, cx);
        cx.notify();
    }

    /// ⌘. or the stop button: deny-and-stop a pending approval, otherwise stop the turn. While
    /// a Rift copy is made, it cancels that instead and the message comes back.
    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread().clone();
        let (pending, busy) = {
            let thread = thread.read(cx);
            let pending = thread.transcript().pending_approval().is_some();
            (
                pending,
                thread.status().is_busy() || thread.preparing().is_some(),
            )
        };
        if !pending && !busy {
            return;
        }
        if pending {
            self.answer(None, ApprovalIntent::DenyAndStop, false, window, cx);
            return;
        }
        let sent = thread.update(cx, |thread, cx| {
            let sent = thread.stop();
            cx.notify();
            sent
        });
        let failed = if pending {
            thread.read(cx).presentation().answer_failed
        } else {
            thread.read(cx).presentation().stop_failed
        };
        self.slots[self.active].notice = notice_after(sent, failed);
        self.sync_field(window, cx);
        cx.notify();
    }

    /// "Try again" on an error: a fresh `claude` for the same conversation (core works out
    /// whether it resumes or starts over).
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread().clone();
        let (program, gone) = {
            let thread = thread.read(cx);
            (thread.session_config().program, self.gone_copy(thread))
        };
        let started = thread.update(cx, |thread, cx| {
            let started = thread.restart();
            cx.notify();
            started
        });
        self.slots[self.active].notice = started.err().map(|error| {
            spawn_notice(
                &error,
                &program,
                thread.read(cx).presentation(),
                gone.as_deref(),
            )
        });
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
        let on_send_again: OnRestart = self.press(cx, |view, window, cx| view.send(window, cx));
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
        let preparing = thread.preparing();
        let busy = is_busy(thread.status()) || preparing.is_some();
        let items = thread.transcript().items();
        let notice = slot.notice.as_ref();
        let retry = offers_retry(ix, items.len(), notice, busy).then_some(&on_restart);
        // Only the notice that is the last row gets Continue; an older one (Newjen quit twice)
        // is history.
        let resume = (ix + 1 == items.len() && !busy && rows::shows_continue(items)).then_some((
            &on_continue,
            rows::continue_label(thread.continue_resends()),
        ));
        let row = match Entry::at(ix, items.len(), preparing.is_some(), notice.is_some()) {
            Some(Entry::Item(ix)) => {
                let item = &items[ix];
                if limits::notice_replaces_error(item, ix + 1 == items.len(), thread.limit_event())
                {
                    return div().into_any_element();
                }
                if limits::is_limit_error(item) {
                    rows::error(
                        ("limit-history", item.id()).into(),
                        &format!("{} usage limit reached", thread.presentation().label),
                        "The CLI refused this turn. Newjen did not retry or resume it.",
                        &[],
                        None,
                        cx,
                    )
                } else {
                    rows::item(
                        item,
                        slot.replies.get(&item.id()).map(|reply| reply.view(cx)),
                        retry,
                        resume,
                        rows::stopped_resends(items, ix),
                        cx,
                    )
                }
            }
            Some(Entry::HeldMessage) => preparing.map_or_else(
                || div().into_any_element(),
                |preparing| ui::user(preparing.message().to_string(), cx).into_any_element(),
            ),
            Some(Entry::MakingCopy) => preparing.map_or_else(
                || div().into_any_element(),
                |preparing| {
                    let took = Instant::now().saturating_duration_since(preparing.started_at());
                    rows::preparing_work(
                        if thread.works_in() == WorksIn::RiftCopy {
                            "Creating a Rift copy"
                        } else {
                            "Preparing the project"
                        },
                        &elapsed(took.as_secs()),
                        preparing.last_line(),
                        cx,
                    )
                },
            ),
            Some(Entry::Notice) => match notice {
                Some(Notice {
                    kind: NoticeKind::Error,
                    title,
                    detail,
                }) => rows::error("notice".into(), title, detail, &[], retry, cx),
                Some(Notice {
                    kind: NoticeKind::CopyFailed { output },
                    title,
                    detail,
                }) => rows::error(
                    "notice".into(),
                    title,
                    detail,
                    output,
                    Some(&on_send_again),
                    cx,
                ),
                Some(Notice {
                    kind: NoticeKind::Held | NoticeKind::Snapshot,
                    title,
                    detail,
                }) => rows::held("notice".into(), title, detail, cx),
                None => div().into_any_element(),
            },
            None => div().into_any_element(),
        };
        let notice_row = matches!(
            Entry::at(ix, items.len(), preparing.is_some(), notice.is_some()),
            Some(Entry::Notice)
        );
        let first = ix == 0;
        // Center each row in the column, like the composer below it.
        div()
            .w_full()
            .flex()
            .justify_center()
            .px_4()
            .when(first, |row| row.pt_4())
            .when(notice_row, |row| {
                row.debug_selector(|| "chat-notice".to_string())
            })
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
            let making_copy = thread.preparing().is_some();
            // While the copy is made its folder is not known yet, and the project folder would
            // read as the copy's.
            let path = if making_copy {
                String::new()
            } else {
                short_path(
                    &tilde(thread.cwd(), self.home.as_deref()),
                    title_bar::path_budget(main_width),
                )
            };
            Bar {
                title: thread.title().to_string().into(),
                path,
                copy: (making_copy && thread.works_in() == WorksIn::RiftCopy)
                    || thread.copy().is_some(),
                show_list,
                grants: thread.grant_views(),
                grants_detail: thread.presentation().grants_detail,
                pill: if making_copy {
                    Some(title_bar::STARTING)
                } else {
                    title_bar::pill(thread.status(), thread.background_tasks().len())
                },
                background_tasks: thread.background_tasks().len(),
            }
        };
        // claude cannot forget a rule, so core stops it (now, or when the turn ends) and the
        // next message starts one without the rule.
        let on_revoke: OnRevoke = Rc::new(
            move |rule: &newjen_core::GrantId, _window: &mut Window, cx: &mut App| {
                entity.update(cx, |thread, cx| {
                    thread.revoke(rule);
                    cx.notify();
                });
            },
        );
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
        let works_in = self.thread().read(cx).works_in();
        let no_agent = self.picker.checked.is_some()
            && !self
                .picker
                .statuses
                .iter()
                .any(|s| !newjen_core::picker::model_rows(s).is_empty());
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .child(ui::title(
                if no_agent { "Install an agent to start" } else { self.thread().read(cx).presentation().empty_title },
                cx,
            ))
            .child(
                // Centered and kept to the column, so a long path wraps under the title.
                div().max_w(COLUMN_WIDTH).text_center().child(
                    ui::caption(
                        if no_agent { "Newjen drives the Claude or Codex CLI installed and signed in on this Mac with your own account. Install one, then Refresh.".into() } else { empty_caption(
                            works_in,
                            &self.project_label,
                            self.thread().read(cx).presentation(),
                        ) },
                        cx,
                    )
                    .text_base(),
                ),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let slot = &self.slots[self.active];
        let thread = slot.thread.read(cx);
        let count = Entry::count(
            thread.transcript().items().len(),
            thread.preparing().is_some(),
            slot.notice.is_some(),
        );
        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(MessageList::new(
                "conversation",
                count,
                slot.messages.clone(),
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
            + m.conversation_reserve
            + if self.edit_preview.is_some() {
                160.
            } else {
                0.
            }
            + 50.;
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
        let (request, waiting) = {
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
            (request, waiting)
        };
        let mut title = approval::title(request.view());
        if waiting > 1 {
            title.push_str(&format!(" (1 of {waiting})"));
        }
        let (title, room) = self.dock_room(&title, main_width, window, cx);
        // `sync_field` keeps `dock_outside` in step with the dock's request.
        let detail = approval::detail(request.view(), self.dock_outside.as_deref(), room);
        let scope = approval::scope_row(&request.view().remember, detail.shortened, room.cols);
        let rule_cut = scope.once;
        let request_key = request.key();
        let request_id = SharedString::from(request_key.to_string());
        let view = cx.weak_entity();
        let answered_id = request_key;
        let card = ui::ApprovalCard::new(
            (ElementId::from("approval"), request_id.clone()),
            title,
            detail.text,
            move |answer, window, cx| {
                let _ = view.update(cx, |view, cx| {
                    view.answer(Some(answered_id), answer, rule_cut, window, cx)
                });
            },
        );
        let card = card.preview(
            self.render_edit_preview(f32::from(window.viewport_size().height), cx),
            !self
                .edit_preview
                .as_ref()
                .is_some_and(|preview| preview.loading()),
        );
        let card = if let Some(note) = &self.denial_note {
            card.note(note.clone())
        } else {
            card
        };
        Some(ui::approval_dock(card, rows::scope(&scope, cx)))
    }

    fn render_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.sync_field(window, cx);
        let field = self.ensure_field(window, cx);
        let (making_copy, cancelling, busy, choosing) = {
            let thread = self.thread().read(cx);
            let choosing =
                (thread.is_draft() && thread.copy().is_none()).then(|| thread.works_in());
            (
                thread.preparing().is_some(),
                thread
                    .preparing()
                    .is_some_and(|waiting| waiting.cancelled()),
                is_busy(thread.status()),
                choosing,
            )
        };
        let no_agent = self.picker.checked.is_some()
            && !self
                .picker
                .statuses
                .iter()
                .any(|s| !newjen_core::picker::model_rows(s).is_empty());
        let checking = self.picker.refreshing && self.picker.statuses.is_empty();
        let paused = self
            .thread()
            .read(cx)
            .paused_until(now_ms() / 1000)
            .is_some();
        let footer = if paused {
            ui::Footer::Disabled("Limit reached")
        } else if no_agent {
            ui::Footer::Disabled("Install an agent, then Refresh")
        } else if checking {
            ui::Footer::Disabled("Checking agents…")
        } else if self.thread().read(cx).work_pending() {
            ui::Footer::Wait("Saving a jj snapshot…")
        } else if self.thread().read(cx).landing_finished() {
            ui::Footer::Wait("Work resolved. Start a new thread to continue.")
        } else if cancelling {
            ui::Footer::Wait(COPY_CANCELLING)
        } else if making_copy && self.thread().read(cx).works_in() == WorksIn::ProjectFolder {
            ui::Footer::Wait("Preparing the project…")
        } else if making_copy {
            ui::Footer::Cancel(self.press(cx, |view, window, cx| view.stop(window, cx)))
        } else if busy {
            ui::Footer::Stop(self.press(cx, |view, window, cx| view.stop(window, cx)))
        } else {
            ui::Footer::Send(self.press(cx, |view, window, cx| view.send(window, cx)))
        };
        let focus = field.read(cx).focus_handle(cx);
        let picker = if let Some(works_in) = choosing {
            Some(
                h_flex()
                    .gap_1()
                    .min_w_0()
                    .child(self.render_works_in(
                        works_in,
                        focus,
                        window.viewport_size().width < px(760.),
                        cx,
                    ))
                    .child(self.render_agent_chips(window, cx))
                    .into_any_element(),
            )
        } else {
            let thread = self.thread().read(cx);
            let config = thread.agent_config();
            let status = config.and_then(|a| {
                self.picker
                    .statuses
                    .iter()
                    .find(|s| s.provider == a.provider)
            });
            Some(
                h_flex()
                    .min_w_0()
                    .gap_1()
                    .when(newjen_core::picker::is_fast(config), |r| {
                        r.child(ui::icon(
                            ui::IconName::Zap,
                            gpui_kit::component::Size::Small,
                            cx.theme().warning,
                        ))
                    })
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(newjen_core::picker::frozen_label(config, status)),
                    )
                    .into_any_element(),
            )
        };
        ui::Composer::new(field, footer)
            .picker(picker)
            .into_any_element()
    }

    /// Bottom-left: Kit's corner anchors clamp rather than flip, so a menu hung below the
    /// composer would cover its own button.
    fn render_works_in(
        &self,
        works_in: WorksIn,
        composer: FocusHandle,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (icon, label) = works_in_label(works_in);
        let view = cx.weak_entity();
        ui::button_with_icon(
            "works-in",
            if compact { "" } else { label },
            icon,
            ui::Look::Ghost,
            ui::Size::Sm,
            cx,
        )
        .tooltip(label)
        .dropdown_caret(true)
        .debug_selector(|| "works-in".to_string())
        .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, move |menu, _, _| {
            let item = |choice: WorksIn| {
                let (icon, label) = works_in_label(choice);
                let view = view.clone();
                PopupMenuItem::new(label)
                    .icon(icon)
                    .checked(choice == works_in)
                    .on_click(move |_, window, cx| {
                        let _ =
                            view.update(cx, |view, cx| view.choose_works_in(choice, window, cx));
                    })
            };
            menu.action_context(composer.clone())
                .check_side(Side::Right)
                .item(item(WorksIn::ProjectFolder))
                .item(item(WorksIn::RiftCopy))
        })
        .into_any_element()
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
            div()
                .id("switch-project")
                .debug_selector(|| "switch-project".to_string())
                .cursor_pointer()
                .when_some(self.project_chooser.clone(), |row, chooser| {
                    row.on_click(move |_, window, cx| chooser(window, cx))
                })
                .child(sidebar::project_row(
                    &self.project_name,
                    &self.project_label,
                    room,
                    window,
                    cx,
                ))
                .into_any_element(),
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
                        archivable: thread.preparing().is_none(),
                    };
                    (id, row)
                };
                let shown = id.clone();
                let on_press = self.press(cx, move |view, window, cx| {
                    view.show_thread(&shown, window, cx)
                });
                let on_archive = self.press(cx, move |view, window, cx| {
                    view.ask_to_archive(&id, window, cx)
                });
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
        if self.thread().read(cx).grant_views().is_empty() {
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
                thread.transcript().items().is_empty()
                    && slot.notice.is_none()
                    && thread.preparing().is_none(),
            )
        };
        let title_bar = self.render_title_bar(placement, main_width, window, cx);
        let body = if self.review_panel.is_some() {
            self.render_review(cx)
        } else if empty {
            self.render_empty_state(cx).into_any_element()
        } else {
            self.render_messages(cx).into_any_element()
        };
        let bottom = match self.render_dock(main_width, window, cx) {
            Some(dock) => dock,
            None if self.review_panel.is_some() => div().into_any_element(),
            None => self.render_composer(window, cx),
        };
        let problem = self.render_save_problem(cx);
        let limit_notice = self.render_limit_notice(cx);

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
            cx.defer_in(window, move |view, window, cx| {
                if view.visible && view.drawer_open {
                    focus.focus(window, cx);
                }
            });
        } else if placement != Placement::Drawer && window.has_active_sheet(cx) {
            window.close_sheet(cx);
            match self.field_focus(cx) {
                Some(field) if self.composer_has_keys() => field.focus(window, cx),
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
            .children(
                self.dock_request
                    .is_none()
                    .then(|| self.render_review_toolbar(cx)),
            )
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
                            .children(limit_notice)
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

/// What a thread's row in the list needs to know about it at `now`.
fn row_facts(thread: &Thread, now: Instant) -> RowFacts {
    let transcript = thread.transcript();
    let status = thread.status();
    // Only a failed thread needs its error, so the others never search their transcript.
    let failure = (status == Status::Failed)
        .then(|| thread.failure_summary())
        .flatten();
    let folder = match (thread.preparing(), thread.works_in()) {
        (Some(_), WorksIn::RiftCopy) => sidebar::Folder::MakingRiftCopy,
        (Some(_), WorksIn::ProjectFolder) => sidebar::Folder::Project,
        (None, WorksIn::RiftCopy) => sidebar::Folder::RiftCopy,
        (None, WorksIn::ProjectFolder) => sidebar::Folder::Project,
    };
    RowFacts {
        status,
        draft: thread.is_draft(),
        folder,
        unread: thread.is_unread(),
        waiting_tool: transcript
            .pending_approval()
            .map(|request| request.view().tool.display_name.clone()),
        working_for: thread
            .turn_started_at()
            .map(|started| now.saturating_duration_since(started)),
        failure,
        stopped_on_quit: matches!(transcript.items().last(), Some(Item::Stopped { .. })),
        background_tasks: thread.background_tasks().len(),
        updated_at: thread.updated_at(),
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
fn spawn_notice(
    error: &anyhow::Error,
    program: &Path,
    presentation: &newjen_core::AgentPresentation,
    gone_copy: Option<&str>,
) -> Notice {
    if let Some(copy) = gone_copy {
        return Notice::error(
            presentation.start_failed,
            format!(
                "This thread's Rift copy at {copy} is gone, so {} has nowhere to work. \
                 Archive the thread and start a new one.",
                presentation.label
            ),
        );
    }
    let (title, detail) = presentation.start_failure(error, program);
    Notice::error(&title, detail)
}

/// Rift's own message is the last line it printed, so it is not shown twice.
fn copy_failed(error: &CopyError, home: Option<&Path>) -> Notice {
    let (detail, output) = match error {
        CopyError::Cancelled => (error.to_string(), Vec::new()),
        CopyError::Failed { detail, output, .. } => {
            let mut lines: Vec<String> = output
                .iter()
                .map(|line| line.trim_end().to_string())
                .filter(|line| !line.trim().is_empty())
                .collect();
            if lines.last().map(|line| line.trim()) == Some(detail.trim()) {
                lines.pop();
            }
            let skip = lines.len().saturating_sub(COPY_OUTPUT_LINES);
            (detail.clone(), lines.split_off(skip))
        }
        CopyError::Exists { path, .. } => (
            format!(
                "A folder already exists at {}. Move it out of the way, then try again.",
                tilde(path, home)
            ),
            Vec::new(),
        ),
        CopyError::Registered(path) => (
            format!(
                "Rift still lists a copy at {}, but its folder is gone. Run `rift gc` in a \
                 terminal to make Rift forget it (it also empties every Rift trash on this \
                 Mac), then try again.",
                tilde(path, home)
            ),
            Vec::new(),
        ),
    };
    Notice {
        kind: NoticeKind::CopyFailed { output },
        title: COPY_FAILED.to_string(),
        detail,
    }
}

/// A copy starts from the project folder, which `blocker`'s agent may be editing, and making
/// one runs `jj new` there.
fn refusal(blocker: &str, blocker_making_copy: bool, jj_repo: bool) -> Notice {
    if blocker_making_copy {
        return Notice::held(
            "Another Rift copy is being made",
            format!("Wait until “{blocker}” has its copy before starting work in this folder."),
        );
    }
    let snapshot = if jj_repo {
        "a jj snapshot of the project"
    } else {
        "the project folder as it is"
    };
    Notice::held(
        "Another thread is working in the project folder",
        format!(
            "A Rift copy starts from {snapshot}, so wait until “{blocker}” finishes, or stop it."
        ),
    )
}

fn missing_tool(tools: &Tools, jj_repo: bool) -> Option<Notice> {
    if !tools.rift.is_absolute() {
        return Some(Notice::held(
            "Rift isn't installed",
            "Newjen makes Rift copies with `rift`. Install it with `npm i -g rift-snapshot`, then \
             quit and reopen Newjen."
                .to_string(),
        ));
    }
    if jj_repo && !tools.jj.is_absolute() {
        return Some(Notice::held(
            "jj isn't installed",
            "This project is a jj repo, and a Rift copy of it needs `jj`. Install it with \
             `brew install jj`, then quit and reopen Newjen."
                .to_string(),
        ));
    }
    None
}

fn archive_question(
    title: &str,
    path: &str,
    presentation: &newjen_core::AgentPresentation,
) -> (String, String) {
    (
        format!("Archive “{title}”?"),
        format!(
            "This deletes its Rift copy at {path}. Changes {} made there that aren't in your \
             project are lost.",
            presentation.label
        ),
    )
}

fn in_jj_repo(project: &Path) -> bool {
    project.ancestors().any(|dir| dir.join(".jj").exists())
}

fn empty_caption(
    works_in: WorksIn,
    project: &str,
    presentation: &newjen_core::AgentPresentation,
) -> String {
    let place = match works_in {
        WorksIn::ProjectFolder => project.to_string(),
        WorksIn::RiftCopy => format!("a new Rift copy of {project}"),
    };
    presentation.empty_caption(&place)
}

fn works_in_label(works_in: WorksIn) -> (ui::IconName, &'static str) {
    match works_in {
        WorksIn::ProjectFolder => (ui::IconName::Folder, "Project folder"),
        WorksIn::RiftCopy => (ui::IconName::Copy, "Rift copy"),
    }
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
fn sent_answer(choice: ApprovalIntent, rule_cut: bool) -> ApprovalIntent {
    match choice {
        ApprovalIntent::AllowForSession if rule_cut => ApprovalIntent::AllowOnce,
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
    use newjen_core::Transcript;
    use newjen_core::agent::{AgentBatch, AgentEvent, Checkpoint};
    use std::io;

    #[test]
    fn always_allow_on_a_cut_rule_is_sent_as_allow_once() {
        let always = ApprovalIntent::AllowForSession;
        assert_eq!(sent_answer(always, true), ApprovalIntent::AllowOnce);
        // Shown in full: remembered as asked.
        assert_eq!(sent_answer(always, false), always);
        // Other answers are never changed.
        assert_eq!(
            sent_answer(ApprovalIntent::Deny, true),
            ApprovalIntent::Deny
        );
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
        transcript.apply(&AgentBatch {
            events: vec![AgentEvent::InterruptTimedOut],
            checkpoint: Checkpoint::Save,
        });
        transcript.apply(&AgentBatch {
            events: vec![AgentEvent::Exited {
                code: None,
                requested: true,
            }],
            checkpoint: Checkpoint::Save,
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
        transcript.apply(&AgentBatch {
            events: vec![AgentEvent::WriteFailed(
                "Broken pipe (os error 32)".to_string(),
            )],
            checkpoint: Checkpoint::Save,
        });
        let busy = is_busy(transcript.status());
        assert!(!busy, "the composer must not stay on \"Working\"");
        let items = transcript.items();
        let last = items.len() - 1;
        assert!(matches!(items[last], Item::Error { .. }));
        assert!(offers_retry(last, items.len(), None, busy));
    }

    #[test]
    fn rows_are_the_items_then_the_waiting_message_then_the_notice() {
        let at = |ix| Entry::at(ix, 2, true, true);
        assert_eq!(at(1), Some(Entry::Item(1)));
        assert_eq!(at(2), Some(Entry::HeldMessage));
        assert_eq!(at(3), Some(Entry::MakingCopy));
        assert_eq!(at(4), Some(Entry::Notice));
        assert_eq!(at(5), None);
        assert_eq!(Entry::count(2, true, true), 5);
        assert_eq!(Entry::at(2, 2, false, true), Some(Entry::Notice));
        assert_eq!(Entry::at(2, 2, false, false), None);
        assert_eq!(Entry::count(0, false, false), 0);
    }

    #[test]
    fn a_failed_copy_shows_rifts_message_once_and_the_lines_before_it() {
        let mut output: Vec<String> = (1..=12).map(|n| format!("line {n}")).collect();
        output.insert(3, "   ".to_string());
        output.push("postcreate hook failed (exit 1)  ".to_string());
        let failed = CopyError::Failed {
            command: "rift create --copy-all".to_string(),
            detail: "postcreate hook failed (exit 1)".to_string(),
            output,
        };
        let notice = copy_failed(&failed, None);
        assert_eq!(notice.title, "Couldn't create the Rift copy");
        assert_eq!(notice.detail, "postcreate hook failed (exit 1)");
        let NoticeKind::CopyFailed { output } = notice.kind else {
            panic!("not a copy failure");
        };
        let last: Vec<String> = (5..=12).map(|n| format!("line {n}")).collect();
        assert_eq!(output, last);

        let home = Path::new("/Users/someone");
        let exists = CopyError::Exists {
            path: home.join(".rifts/app/newjen-1a2b3c4d"),
            rift_id: None,
        };
        assert!(
            copy_failed(&exists, Some(home))
                .detail
                .starts_with("A folder already exists at ~/.rifts/app/newjen-1a2b3c4d.")
        );
        let listed = CopyError::Registered(home.join(".rifts/app/newjen-1a2b3c4d"));
        let detail = copy_failed(&listed, Some(home)).detail;
        assert!(detail.contains("`rift gc`"), "{detail}");
    }

    #[test]
    fn a_refused_copy_names_the_thread_to_wait_for() {
        let busy = refusal("Fix the parser", false, true);
        assert!(matches!(busy.kind, NoticeKind::Held));
        assert_eq!(
            busy.title,
            "Another thread is working in the project folder"
        );
        assert_eq!(
            busy.detail,
            "A Rift copy starts from a jj snapshot of the project, so wait until “Fix the \
             parser” finishes, or stop it."
        );
        assert!(
            refusal("Fix the parser", false, false)
                .detail
                .starts_with("A Rift copy starts from the project folder as it is,")
        );
        let making = refusal("Fix the parser", true, true);
        assert_eq!(making.title, "Another Rift copy is being made");
        assert!(making.detail.contains("“Fix the parser” has its copy"));
    }

    #[test]
    fn a_missing_tool_says_how_to_install_it() {
        let tools = |rift: &str, jj: &str| Tools {
            rift: PathBuf::from(rift),
            jj: PathBuf::from(jj),
            env: Vec::new(),
        };
        let rift = missing_tool(&tools("rift", "/bin/jj"), false).unwrap();
        assert_eq!(rift.title, "Rift isn't installed");
        assert!(rift.detail.contains("`npm i -g rift-snapshot`"));
        let jj = missing_tool(&tools("/bin/rift", "jj"), true).unwrap();
        assert!(jj.detail.contains("`brew install jj`"));
        // A plain folder is copied without jj.
        assert!(missing_tool(&tools("/bin/rift", "jj"), false).is_none());
        assert!(missing_tool(&tools("/bin/rift", "/bin/jj"), true).is_none());
    }

    #[test]
    fn archiving_a_copy_says_what_is_deleted() {
        assert_eq!(
            archive_question(
                "Fix the parser",
                "~/.rifts/Newjen/newjen-1a2b3c4d",
                &Default::default()
            ),
            (
                "Archive “Fix the parser”?".to_string(),
                "This deletes its Rift copy at ~/.rifts/Newjen/newjen-1a2b3c4d. Changes Claude \
                 made there that aren't in your project are lost."
                    .to_string()
            )
        );
    }

    #[test]
    fn the_empty_state_says_where_claude_will_work() {
        assert_eq!(
            empty_caption(WorksIn::ProjectFolder, "~/Newjen", &Default::default()),
            "Claude Code runs in ~/Newjen. You approve each tool call before it runs."
        );
        assert_eq!(
            empty_caption(WorksIn::RiftCopy, "~/Newjen", &Default::default()),
            "Claude Code runs in a new Rift copy of ~/Newjen. You approve each tool call before \
             it runs."
        );
    }

    #[test]
    fn a_vanished_copy_is_named_instead_of_the_program() {
        let notice = spawn_notice(
            &failed(io::ErrorKind::NotFound),
            Path::new("/opt/bin/claude"),
            &Default::default(),
            Some("~/.rifts/app/newjen-1a2b3c4d"),
        );
        assert_eq!(notice.title, "Could not start Claude Code");
        assert!(
            notice
                .detail
                .starts_with("This thread's Rift copy at ~/.rifts/app/newjen-1a2b3c4d is gone"),
            "{}",
            notice.detail
        );
        assert!(!notice.detail.contains("Install Claude Code"));
    }

    fn failed(kind: io::ErrorKind) -> anyhow::Error {
        // The same shape `Session::spawn` returns: the OS error with context around it.
        anyhow::Error::new(io::Error::from(kind)).context("could not start `claude` in /work")
    }

    #[test]
    fn spawn_notice_names_the_actual_problem() {
        let program = Path::new("/opt/bin/claude");
        let missing = spawn_notice(
            &failed(io::ErrorKind::NotFound),
            program,
            &Default::default(),
            None,
        );
        assert!(missing.detail.contains("could not find `/opt/bin/claude`"));
        assert!(missing.detail.contains("Install Claude Code"));

        let denied = spawn_notice(
            &failed(io::ErrorKind::PermissionDenied),
            program,
            &Default::default(),
            None,
        );
        assert!(denied.detail.contains("not allowed to run"));
        assert!(!denied.detail.contains("Install Claude Code"));

        let other = spawn_notice(
            &failed(io::ErrorKind::OutOfMemory),
            program,
            &Default::default(),
            None,
        );
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
