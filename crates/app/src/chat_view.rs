//! The one window's content: a conversation with Claude Code.
//!
//! `ChatView` is a GPUI *entity* (think of a component instance with its own state that GPUI
//! keeps alive). It owns:
//! - the `Transcript` (what to draw, from `newjen-core`),
//! - the running `Session` (the `claude` process),
//! - a background task that feeds the session's events into the transcript,
//! - the composer's text field.
//!
//! Every change ends with `cx.notify()`, which asks GPUI to call `render` again.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use ely_gpui_component::agent::{AgentState, AgentStatus, Permission, PermissionPrompt};
use ely_gpui_component::chat::{InputHint, MessageList, PromptInput};
use ely_gpui_component::data_display::{Badge, Tone};
use ely_gpui_component::forms::TextInput;
use ely_gpui_component::layout::{h_stack, v_stack};
use ely_gpui_component::motion;
use ely_gpui_component::primitives::FocusScope;
use ely_gpui_component::shell::TitleBar;
use ely_gpui_component::theme::{ActiveTheme, ControlSize, Elevation, Radius, TextSize};
use ely_gpui_component::typography::{Caption, Kbd, Title};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, ElementId, Entity, FocusHandle, Focusable,
    IntoElement, KeyDownEvent, ParentElement, Pixels, Render, SharedString, Styled, Subscription,
    Task, Window, div, font, prelude::*, px,
};
use newjen_core::{
    Conversation, Item, PermissionAnswer, Session, SessionConfig, SessionEvent, Status, ToolState,
    Transcript, session_scope,
};

use crate::approval::{self, Room};
use crate::format::{fit_rows, short_path, text_rows};
use crate::keys::{self, ApprovalKey, Stop};
use crate::rows::{self, OnRestart};

/// The conversation column is capped at this width and centered, as in the mocks.
const COLUMN_WIDTH: Pixels = px(720.0);

/// How far the approval dock slides up as it appears.
const DOCK_RISE: f32 = 6.0;

/// ⌘↵ and Esc are ignored for this long after a new approval appears, and for this long after
/// each ignored press. Without it a quick double-tap could answer the next queued request
/// before its dock is even visible. (A held key is handled separately: its repeats never
/// answer anything, see `keys::approval_key`.) Mouse clicks are not affected.
const KEY_ANSWER_DELAY: Duration = Duration::from_millis(300);

// Sizes used to work out how much of an approval's detail fits above the dock's buttons (see
// `dock_room`). Ely's PermissionPrompt has no height limit, so without this a long command at
// the smallest window size (640x480) pushes Allow and Deny below the window's edge.
/// Left and right padding around the bottom column (`px_4`).
const PAGE_PADDING: f32 = 32.0;
/// Space above and below the dock (`pt_2` + `pb_4`).
const BOTTOM_PADDING: f32 = 24.0;
/// The prompt's border (1 px each side) and padding (`p_4`), plus the detail box's side
/// padding (`px_2`) or the title's icon and gap, whichever is wider.
const DETAIL_INSET: f32 = 2.0 + 32.0 + 22.0;
/// The prompt's border and padding, its two gaps (`gap_3`) and the detail box's own padding
/// (`py_1p5`), top to bottom. The button row and the text rows are added on top.
const PROMPT_CHROME: f32 = 2.0 + 32.0 + 24.0 + 12.0;
/// The scope row under the dock ("Always allow remembers …"): one line plus the gap above it
/// (`gap_2`).
const SCOPE_GAP: f32 = 8.0;
/// Height kept for the conversation above the dock, so you still see what Claude is doing.
const CONVERSATION_RESERVE: f32 = 80.0;
/// GPUI's default line height: the golden ratio times the font size.
const LINE_HEIGHT: f32 = 1.618;
/// The title wraps to at most this many rows (a deep file path is cut in its middle).
const TITLE_ROWS: usize = 2;
/// The detail never takes more rows than this, nor fewer (the conversation shrinks instead).
const DETAIL_ROWS_MAX: usize = 9;
const DETAIL_ROWS_MIN: usize = 3;

/// Rough room the title bar keeps for the centered "Newjen" title and the traffic lights, and
/// the width of one small mono character, used to decide how much of the project path fits.
const TITLE_WIDTH: f32 = 60.0;
const TITLE_BAR_INSET: f32 = 100.0;
const PATH_CHAR_WIDTH: f32 = 7.0;

/// A message from the app itself (not reported by Claude), shown as the last row. It is cleared
/// by the next prompt, answer or stop that reaches Claude.
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

pub struct ChatView {
    /// The project directory Claude works in.
    project: PathBuf,
    /// The project path for display, e.g. `~/Newjen`.
    project_label: SharedString,
    transcript: Transcript,
    session: Option<Session>,
    /// Applies session events to the transcript. Dropping it stops that, which matters on
    /// restart: the old process's late events must not touch the new session's state.
    pump: Option<Task<()>>,
    notice: Option<Notice>,
    /// The composer's text field (an Ely entity, so it keeps its own text and caret).
    field: Entity<TextInput>,
    /// The placeholder currently set on the field, to avoid resetting it every event.
    placeholder: SharedString,
    /// Focus target while an approval is pending (the dock replaces the composer, so the field
    /// is not on screen), so ⌘↵ and Esc reach us.
    root: FocusHandle,
    /// The approval the dock is showing, to notice when a new one takes its place.
    dock_request: Option<String>,
    /// That approval's path outside the project, if any (`approval::outside_path`). Worked out
    /// once when the request reaches the dock, because it reads the disk and `render` runs on
    /// every streamed frame.
    dock_outside: Option<String>,
    /// Keyboard answers (⌘↵, Esc) are ignored until this moment; see `KEY_ANSWER_DELAY`.
    keys_ready_at: Option<Instant>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(
        project: PathBuf,
        project_label: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = SharedString::from(format!("Ask Claude to work on {project_label}…"));
        let field = cx.new(|cx| {
            TextInput::new(window, cx)
                .multi_line(1, 8)
                .placeholder(placeholder.clone())
        });
        // Quitting (⌘Q) runs this synchronously; GPUI allows ~200 ms, and `shutdown` takes at
        // most ~150 ms. Dropping the view also shuts the session down (`Session`'s Drop).
        let on_quit = cx.on_app_quit(|view: &mut ChatView, _cx| {
            view.pump = None;
            if let Some(session) = view.session.take() {
                session.shutdown();
            }
            async {}
        });
        let mut view = ChatView {
            transcript: Transcript::with_cwd(project.clone()),
            project,
            project_label,
            session: None,
            pump: None,
            notice: None,
            field,
            placeholder,
            root: cx.focus_handle(),
            dock_request: None,
            dock_outside: None,
            keys_ready_at: None,
            _subscriptions: vec![on_quit],
        };
        view.start_session(None, window, cx);
        view
    }

    /// The text field's focus handle, so `main` can focus it when the window opens.
    pub fn field_focus(&self, cx: &App) -> FocusHandle {
        self.field.read(cx).focus_handle(cx)
    }

    /// Starts `claude` (continuing `resume` if given) and the task that reads its events.
    /// Any previous session is stopped first.
    fn start_session(
        &mut self,
        resume: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Order matters: stop listening to the old process before killing it, so its final
        // `Exited` event never reaches the transcript.
        self.pump = None;
        if let Some(old) = self.session.take() {
            old.shutdown();
        }

        let mut config = SessionConfig::new(self.project.clone());
        config.conversation = resume.map(Conversation::Resume);
        let program = config.program.clone();
        match Session::spawn(config) {
            Ok((session, events)) => {
                self.session = Some(session);
                // `spawn_in` runs this async loop on the UI thread; awaiting `recv` does not
                // block it. The loop ends when the channel closes or the view is gone.
                self.pump = Some(cx.spawn_in(window, async move |view, cx| {
                    while let Ok(event) = events.recv().await {
                        let applied =
                            view.update_in(cx, |view, window, cx| view.apply(event, window, cx));
                        if applied.is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(error) => self.notice = Some(spawn_notice(&error, &program)),
        }
    }

    /// One event from the session: update the transcript and redraw.
    fn apply(&mut self, event: SessionEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.transcript.apply(&event);
        self.sync_field(window, cx);
        cx.notify();
    }

    /// Keeps focus and the text field in step with the conversation: focus on the root while
    /// an approval is pending (so ⌘↵ and Esc reach `on_key_down`), back in the field
    /// afterwards, and a placeholder that fits the moment.
    fn sync_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let front = self
            .transcript
            .pending_approval()
            .map(|request| request.request_id.clone());
        if front != self.dock_request {
            // A new request is on the dock: give the user a moment to see it.
            self.keys_ready_at = front.as_ref().map(|_| Instant::now() + KEY_ANSWER_DELAY);
            match (&self.dock_request, &front) {
                // The dock appears in place of the composer.
                (None, Some(_)) => window.focus(&self.root, cx),
                // The last approval was answered: the composer is back.
                (Some(_), None) => {
                    let handle = self.field_focus(cx);
                    window.focus(&handle, cx);
                }
                // One request replaced by the next, or nothing to nothing: focus stays.
                _ => {}
            }
            self.dock_request = front;
            self.dock_outside = self
                .transcript
                .pending_approval()
                .and_then(|request| approval::outside_path(request, &self.project));
        }

        let placeholder = SharedString::from(match self.transcript.status() {
            Status::Working | Status::Waiting => "Claude is working…".to_string(),
            _ if self.transcript.items().is_empty() => {
                format!("Ask Claude to work on {}…", self.project_label)
            }
            _ => "Reply to Claude…".to_string(),
        });
        if placeholder != self.placeholder {
            self.placeholder = placeholder.clone();
            self.field
                .update(cx, |field, cx| field.set_placeholder(placeholder, cx));
        }
    }

    fn is_busy(&self) -> bool {
        is_busy(self.transcript.status())
    }

    /// Sends the composer's text as a prompt (Enter or the send button).
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.field.read(cx).text().trim().to_string();
        if text.is_empty() || self.is_busy() {
            return;
        }
        let Some(session) = &self.session else {
            return; // The notice explains why there is no session.
        };
        match session.send_prompt(&text) {
            Ok(()) => {
                // Writing worked, so an older "could not reach Claude" notice is out of date.
                self.notice = None;
                self.transcript.push_user(&text);
                // Ely's composer leaves clearing the field to its owner.
                self.field.update(cx, |field, cx| field.set_text("", cx));
            }
            // Nothing was sent: the text stays in the field (so nothing typed is lost) and the
            // conversation stays as it was, not "Working", so the composer remains usable.
            Err(error) => self.notice = notice_after(Err(error), "Could not reach Claude"),
        }
        self.sync_field(window, cx);
        cx.notify();
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
        let Some(request) = self.transcript.pending_approval().cloned() else {
            return;
        };
        if request_id.is_some_and(|id| id != request.request_id) {
            return;
        }
        let answer = sent_answer(choice, rule_cut);
        // Resolve first: a second click or key press now finds nothing to answer.
        self.transcript
            .resolve_approval(&request.request_id, answer);
        if let Some(session) = &self.session {
            // No note after the click when "Always allow" acts once: the scope row under the
            // dock already said so before the click.
            let sent = session.answer(&request, answer);
            self.notice = notice_after(sent, "Could not send your answer to Claude");
        }
        self.sync_field(window, cx);
        cx.notify();
    }

    /// ⌘. or the stop button: deny-and-stop a pending approval, otherwise interrupt the turn.
    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transcript.pending_approval().is_some() {
            self.answer(None, PermissionAnswer::DenyAndStop, false, window, cx);
            return;
        }
        if !self.is_busy() {
            return;
        }
        if let Some(session) = &self.session {
            self.notice = notice_after(session.interrupt(), "Could not stop Claude");
            cx.notify();
        }
    }

    /// "Try again" on an error: a fresh `claude` that resumes the same conversation.
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        // The old process's own exit event will never arrive (its pump is dropped below), so
        // end its turn here as a requested stop would. Otherwise a restart pressed mid-turn
        // (the app's notice keeps its button then, see `rows::can_retry`) leaves the view
        // stuck in Working.
        if self.session.is_some() {
            self.transcript.apply(&SessionEvent::Exited {
                code: None,
                requested: true,
            });
        }
        let resume = self.transcript.session_id().map(str::to_string);
        self.start_session(resume, window, cx);
        self.sync_field(window, cx);
        cx.notify();
    }

    /// Whether a keyboard answer may go through now. A press that comes too soon is ignored
    /// and pushes the moment back, so tapping quickly never answers a request you have not
    /// seen yet.
    fn keys_ready(&mut self) -> bool {
        let now = Instant::now();
        match self.keys_ready_at {
            Some(ready_at) if now < ready_at => {
                self.keys_ready_at = Some(now + KEY_ANSWER_DELAY);
                false
            }
            _ => true,
        }
    }

    /// ⌘↵ (allow once) and Esc (deny) while an approval is pending. GPUI calls this for every
    /// key press inside the view, before the focused element sees it (the "capture" phase).
    /// With no approval pending it leaves every key alone, so Esc still reaches the text field
    /// (where it may be cancelling a half-typed IME character).
    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.transcript.pending_approval().is_none() {
            return;
        }
        let Some(key) = keys::approval_key(event) else {
            return;
        };
        // An approval key while the dock is up is ours, even when it is ignored below.
        cx.stop_propagation();
        match key {
            // Held down: macOS is repeating the key. Only a fresh press answers.
            ApprovalKey::Repeat => {}
            ApprovalKey::Answer(answer) => {
                if self.keys_ready() {
                    self.answer(None, answer, false, window, cx);
                }
            }
        }
    }

    fn on_stop(&mut self, _: &Stop, window: &mut Window, cx: &mut Context<Self>) {
        self.stop(window, cx);
    }

    /// A callback for Ely's "Try again" buttons, which receive `&mut App` rather than our
    /// `Context`, so it goes back through the entity.
    fn restart_callback(&self, cx: &Context<Self>) -> OnRestart {
        let view = cx.entity();
        Rc::new(move |window: &mut Window, cx: &mut App| {
            view.update(cx, |view, cx| view.restart(window, cx));
        })
    }

    /// Row `ix` of the message list: a transcript item, or the app notice after them.
    fn render_row(
        &mut self,
        ix: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on_restart = self.restart_callback(cx);
        let busy = self.is_busy();
        let items = self.transcript.items();
        let retry =
            offers_retry(ix, items.len(), self.notice.as_ref(), busy).then_some(&on_restart);
        let row = if let Some(item) = items.get(ix) {
            rows::item(item, ix + 1 == items.len(), retry, cx)
        } else if let Some(notice) = &self.notice {
            rows::error("notice".into(), &notice.title, &notice.detail, retry)
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

    fn render_title_bar(&self, window: &Window, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        // Ely's TitleBar centers a plain-text title only, so the path sits in its leading
        // slot, a little clear of the traffic lights. That slot never shrinks, so a long
        // path would cover the title and push the status off screen: cut it in the middle
        // to what fits beside the title at the current window width.
        let width = f32::from(window.viewport_size().width);
        let room = ((width - TITLE_WIDTH) / 2.0 - TITLE_BAR_INSET).max(PATH_CHAR_WIDTH * 12.0);
        let max_chars = (room / PATH_CHAR_WIDTH) as usize;
        let path = div()
            .pl_2()
            .max_w(px(room))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(theme.text_size(TextSize::Xs))
            .font_family(theme.mono_family.clone())
            .text_color(theme.colors.fg_subtle)
            .child(short_path(&self.project_label, max_chars));
        let bar = TitleBar::new("title-bar").title("Newjen").child(path);
        let state = match self.transcript.status() {
            Status::Working => AgentState::Working,
            Status::Waiting => AgentState::Waiting,
            Status::Done => AgentState::Done,
            Status::Failed => AgentState::Failed,
            // Ely's agent pill has no "stopped" state; a status badge says it instead.
            Status::Stopped => return bar.action(Badge::new("Stopped").dot().tone(Tone::Danger)),
            Status::Idle => return bar,
        };
        bar.action(AgentStatus::new("agent-status", state))
    }

    fn render_empty_state(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        v_stack()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .child(Title::new("What should Claude work on?"))
            .child(
                // Centered and kept to the column, so a long path wraps under the title.
                div().max_w(COLUMN_WIDTH).text_center().child(
                    Caption::new(format!(
                        "Claude Code runs in {}. You approve each tool call before it runs.",
                        self.project_label
                    ))
                    .text_size(theme.text_size(TextSize::Base)),
                ),
            )
    }

    fn render_messages(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.transcript.items().len() + usize::from(self.notice.is_some());
        div().flex_1().min_h_0().child(MessageList::new(
            "conversation",
            count,
            cx.processor(|view, ix: usize, window, cx| view.render_row(ix, window, cx)),
        ))
    }

    /// How much of an approval fits in the dock at the current window size. Returns the title
    /// cut to `TITLE_ROWS` and the room left for the detail: columns from the dock's width and
    /// the mono font's character width, rows from the window height minus the title bar, the
    /// dock's padding, buttons and title, the scope row under it, and a strip of conversation.
    fn dock_room(&self, title: &str, window: &Window, cx: &App) -> (String, Room) {
        let theme = cx.theme();
        let rem = window.rem_size();
        // Ely's prompt draws its text at the small size, the detail in the mono font.
        let font_size = theme.text_size(TextSize::Sm).to_pixels(rem);
        let text_system = window.text_system();
        let mono = text_system.resolve_font(&font(theme.mono_family.clone()));
        let char_width = text_system
            .advance(mono, font_size, '0')
            .map(|size| f32::from(size.width))
            .unwrap_or(f32::from(font_size) * 0.6)
            .max(1.0);
        let viewport = window.viewport_size();
        let dock_width = (f32::from(viewport.width) - PAGE_PADDING).min(f32::from(COLUMN_WIDTH));
        // One column spare: real wrapping is a little less tidy than the estimate.
        let cols = ((dock_width - DETAIL_INSET) / char_width).max(0.0) as usize;
        let cols = cols.saturating_sub(1);

        let title = fit_rows(title, cols, TITLE_ROWS);
        let line_height = f32::from(font_size) * LINE_HEIGHT;
        let used = f32::from(theme.titlebar_height().to_pixels(rem))
            + BOTTOM_PADDING
            + PROMPT_CHROME
            + f32::from(theme.control_height(ControlSize::Md).to_pixels(rem))
            + text_rows(&title, cols) as f32 * line_height
            + SCOPE_GAP
            + line_height
            + CONVERSATION_RESERVE;
        let free = (f32::from(viewport.height) - used).max(0.0);
        let rows = ((free / line_height) as usize).clamp(DETAIL_ROWS_MIN, DETAIL_ROWS_MAX);
        (title, Room { cols, rows })
    }

    /// The approval dock, shown instead of the composer while Claude waits for an answer.
    fn render_dock(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let request = self.transcript.pending_approval()?;
        let mut title = approval::title(request, Some(&self.project));
        // Say when more approvals are queued behind this one (each tool waiting for an answer
        // shows as Waiting), so a second request is never a surprise.
        let waiting = self
            .transcript
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
        if waiting > 1 {
            title.push_str(&format!(" (1 of {waiting})"));
        }
        let (title, room) = self.dock_room(&title, window, cx);
        // `sync_field` keeps `dock_outside` in step with the dock's request.
        let detail = approval::detail(
            request,
            Some(&self.project),
            self.dock_outside.as_deref(),
            room,
        );
        let scope = approval::scope_row(
            &session_scope(request),
            request,
            detail.shortened,
            room.cols,
        );
        let rule_cut = scope.once;
        let request_id = SharedString::from(request.request_id.clone());
        let view = cx.entity();
        let answered_id = request_id.clone();
        let prompt = PermissionPrompt::new(
            (ElementId::from("approval"), request_id.clone()),
            title,
            move |permission, window, cx| {
                let answer = match permission {
                    Permission::Once => PermissionAnswer::AllowOnce,
                    Permission::Always => PermissionAnswer::AllowForSession,
                    Permission::Deny => PermissionAnswer::Deny,
                };
                view.update(cx, |view, cx| {
                    view.answer(Some(answered_id.as_ref()), answer, rule_cut, window, cx)
                });
            },
        )
        .detail(detail.text);

        let theme = cx.theme();
        // Ely's prompt draws its own surface, border and 8 px corners; this adds the floating
        // shadow on the same shape.
        let card = div()
            .rounded(theme.radius(Radius::Lg))
            .shadow(theme.elevation(Elevation::Floating))
            .child(prompt);
        // Ely's prompt has no slot between its detail and its buttons, so the scope row sits
        // just under the card. `dock_room` counts it, so the buttons stay on screen.
        let dock = v_stack()
            .relative()
            .gap_2()
            .child(card)
            .child(rows::scope(&scope, cx));
        // Enters once per request: fade in while rising 6 px, 120 ms, ease-out. Ely shortens
        // it to 1 ms when its theme's `reduced_motion` flag is set; note that nothing sets that
        // flag from macOS's Reduce Motion setting yet, so today it always plays.
        let enter =
            Animation::new(motion::duration(motion::FAST, cx)).with_easing(motion::ease_out_cubic);
        Some(
            dock.with_animation((ElementId::from("dock"), request_id), enter, |dock, t| {
                dock.opacity(t).top(px(DOCK_RISE * (1.0 - t)))
            })
            .into_any_element(),
        )
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let composer = PromptInput::new("composer", &self.field, move |window, cx| {
            view.update(cx, |view, cx| view.send(window, cx));
        });
        if self.is_busy() {
            let view = cx.entity();
            let theme = cx.theme();
            let hint = h_stack()
                .gap_1()
                .text_size(theme.text_size(TextSize::Xs))
                .text_color(theme.colors.fg_subtle)
                .child(Kbd::new("cmd-."))
                .child("to stop");
            composer
                .tool(hint)
                .busy(move |window, cx| view.update(cx, |view, cx| view.stop(window, cx)))
                .into_any_element()
        } else {
            composer.tool(InputHint).into_any_element()
        }
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, fg, font) = (theme.colors.bg, theme.colors.fg, theme.font_family.clone());
        let empty = self.transcript.items().is_empty() && self.notice.is_none();
        let body = if empty {
            self.render_empty_state(cx).into_any_element()
        } else {
            self.render_messages(cx).into_any_element()
        };
        let bottom = match self.render_dock(window, cx) {
            Some(dock) => dock,
            None => self.render_composer(cx),
        };

        // The outer div carries our key context and shortcut handlers; Ely's root FocusScope
        // inside it handles Tab and takes focus back when the focused element disappears.
        div()
            .key_context(keys::CONTEXT)
            .size_full()
            .capture_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::on_stop))
            .child(
                FocusScope::new(&self.root)
                    .root()
                    .size_full()
                    .flex()
                    .flex_col()
                    .bg(bg)
                    .text_color(fg)
                    .font_family(font)
                    .child(self.render_title_bar(window, cx))
                    .child(body)
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .flex()
                            .justify_center()
                            .px_4()
                            .pt_2()
                            .pb_4()
                            .child(div().w_full().max_w(COLUMN_WIDTH).child(bottom)),
                    ),
            )
    }
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
