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

use std::path::PathBuf;
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
use ely_gpui_component::theme::{ActiveTheme, Elevation, Radius, TextSize};
use ely_gpui_component::typography::{Caption, Kbd, Title};
use gpui::{
    Animation, AnimationExt, AnyElement, App, Context, ElementId, Entity, FocusHandle, Focusable,
    IntoElement, ParentElement, Pixels, Render, SharedString, Styled, Subscription, Task, Window,
    div, prelude::*, px,
};
use newjen_core::{
    Item, PermissionAnswer, Session, SessionConfig, SessionEvent, Status, ToolState, Transcript,
};

use crate::approval;
use crate::format::short_path;
use crate::keys::{self, AllowOnce, Deny, Stop};
use crate::rows::{self, OnRestart};

/// The conversation column is capped at this width and centered, as in the mocks.
const COLUMN_WIDTH: Pixels = px(720.0);

/// How far the approval dock slides up as it appears.
const DOCK_RISE: f32 = 6.0;

/// ⌘↵ and Esc are ignored for this long after a new approval appears, and for this long after
/// each ignored press. Without it a double-tap, or a held key repeating, could answer the next
/// queued request before its dock is even visible. Mouse clicks are not affected.
const KEY_ANSWER_DELAY: Duration = Duration::from_millis(300);

/// Rough room the title bar keeps for the centered "Newjen" title and the traffic lights, and
/// the width of one small mono character, used to decide how much of the project path fits.
const TITLE_WIDTH: f32 = 60.0;
const TITLE_BAR_INSET: f32 = 100.0;
const PATH_CHAR_WIDTH: f32 = 7.0;

/// A problem in the app itself (not reported by Claude), shown as an error row at the end.
struct Notice {
    title: String,
    detail: String,
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
    /// Focus target while the field is disabled for an approval, so ⌘↵ and Esc reach us.
    root: FocusHandle,
    /// True while the field is disabled because an approval is pending.
    field_locked: bool,
    /// The approval the dock is showing, to notice when a new one takes its place.
    dock_request: Option<String>,
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
            field_locked: false,
            dock_request: None,
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
        config.resume = resume;
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
            Err(error) => {
                self.notice = Some(Notice {
                    title: "Could not start Claude Code".to_string(),
                    detail: format!(
                        "{error:#}.\n\nInstall Claude Code and run `claude` once in a terminal to \
                         log in, then try again. Newjen looked for `{}` (set NEWJEN_CLAUDE to \
                         use another path).",
                        program.display()
                    ),
                });
            }
        }
    }

    /// One event from the session: update the transcript and redraw.
    fn apply(&mut self, event: SessionEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.transcript.apply(&event);
        self.sync_field(window, cx);
        cx.notify();
    }

    /// Keeps the text field in step with the conversation: disabled (and focus on the root)
    /// while an approval is pending, and a placeholder that fits the moment.
    fn sync_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let front = self
            .transcript
            .pending_approval()
            .map(|request| request.request_id.clone());
        if front != self.dock_request {
            // A new request is on the dock: give the user a moment to see it.
            self.keys_ready_at = front.as_ref().map(|_| Instant::now() + KEY_ANSWER_DELAY);
            self.dock_request = front;
        }
        let pending = self.dock_request.is_some();
        if pending && !self.field_locked {
            self.field_locked = true;
            self.field
                .update(cx, |field, cx| field.set_disabled(true, cx));
            window.focus(&self.root, cx);
        } else if !pending && self.field_locked {
            self.field_locked = false;
            self.field
                .update(cx, |field, cx| field.set_disabled(false, cx));
            let handle = self.field_focus(cx);
            window.focus(&handle, cx);
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
        matches!(self.transcript.status(), Status::Working | Status::Waiting)
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
                self.transcript.push_user(&text);
                // Ely's composer leaves clearing the field to its owner.
                self.field.update(cx, |field, cx| field.set_text("", cx));
            }
            Err(error) => {
                self.notice = Some(Notice {
                    title: "Could not reach Claude".to_string(),
                    detail: format!("{error:#}. Try again to restart the session."),
                });
            }
        }
        self.sync_field(window, cx);
        cx.notify();
    }

    /// Answers the pending approval. `request_id` is the request the button belonged to; a
    /// stale click (that request was already answered) does nothing.
    fn answer(
        &mut self,
        request_id: Option<&str>,
        answer: PermissionAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(request) = self.transcript.pending_approval().cloned() else {
            return;
        };
        if request_id.is_some_and(|id| id != request.request_id) {
            return;
        }
        // Resolve first: a second click or key press now finds nothing to answer.
        self.transcript
            .resolve_approval(&request.request_id, answer);
        if let Some(session) = &self.session
            && let Err(error) = session.answer(&request, answer)
        {
            self.notice = Some(Notice {
                title: "Could not send your answer to Claude".to_string(),
                detail: format!("{error:#}. Try again to restart the session."),
            });
        }
        self.sync_field(window, cx);
        cx.notify();
    }

    /// ⌘. or the stop button: deny-and-stop a pending approval, otherwise interrupt the turn.
    fn stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.transcript.pending_approval().is_some() {
            self.answer(None, PermissionAnswer::DenyAndStop, window, cx);
            return;
        }
        if !self.is_busy() {
            return;
        }
        if let Some(session) = &self.session
            && let Err(error) = session.interrupt()
        {
            self.notice = Some(Notice {
                title: "Could not stop Claude".to_string(),
                detail: format!("{error:#}. Try again to restart the session."),
            });
            cx.notify();
        }
    }

    /// "Try again" on an error: a fresh `claude` that resumes the same conversation.
    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notice = None;
        // The old process's own exit event will never arrive (its pump is dropped below), so
        // end its turn here as a requested stop would. Otherwise a restart pressed mid-turn
        // (old error cards keep their button) leaves the view stuck in Working.
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
    /// and pushes the moment back, so holding the key down never answers anything.
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

    fn on_allow_once(&mut self, _: &AllowOnce, window: &mut Window, cx: &mut Context<Self>) {
        if self.keys_ready() {
            self.answer(None, PermissionAnswer::AllowOnce, window, cx);
        }
    }

    fn on_deny(&mut self, _: &Deny, window: &mut Window, cx: &mut Context<Self>) {
        if self.keys_ready() {
            self.answer(None, PermissionAnswer::Deny, window, cx);
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
        let items = self.transcript.items();
        let row = if let Some(item) = items.get(ix) {
            rows::item(item, ix + 1 == items.len(), &on_restart, cx)
        } else if let Some(notice) = &self.notice {
            rows::error("notice".into(), &notice.title, &notice.detail, &on_restart)
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

    /// The approval dock, shown instead of the composer while Claude waits for an answer.
    fn render_dock(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        let detail = approval::detail(request, Some(&self.project));
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
                    view.answer(Some(answered_id.as_ref()), answer, window, cx)
                });
            },
        )
        .detail(detail);

        let theme = cx.theme();
        // Ely's prompt draws its own surface, border and 8 px corners; this adds the floating
        // shadow on the same shape.
        let dock = div()
            .relative()
            .rounded(theme.radius(Radius::Lg))
            .shadow(theme.elevation(Elevation::Floating))
            .child(prompt);
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
        let bottom = match self.render_dock(cx) {
            Some(dock) => dock,
            None => self.render_composer(cx),
        };

        // The outer div carries our key context and shortcut handlers; Ely's root FocusScope
        // inside it handles Tab and takes focus back when the focused element disappears.
        div()
            .key_context(keys::CONTEXT)
            .size_full()
            .on_action(cx.listener(Self::on_allow_once))
            .on_action(cx.listener(Self::on_deny))
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
