use gpui_kit::component::{WindowExt as _, dialog::DialogEntrance};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, IntoElement, ParentElement, PathPromptOptions,
    Render, ScrollHandle, Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_kit::base::v_flex;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use newjen_core::rift::Tools;
use newjen_core::{Project, SessionConfig, Store, Thread, thread};

use crate::attention::{NativeAttention, NativeQuitGuard};
use crate::chat_view::{Attention, ChatView, QuitBlockers, Startup};
use crate::format::{relative_time, tilde};
use crate::keys::{OpenProject, OpenRecentProject, ShowProjects};
use crate::ui;

/// How often the attention adapter delivers the notifications it holds (see
/// `NativeAttention::poll`).
const ATTENTION_POLL: std::time::Duration = std::time::Duration::from_secs(1);

struct OpenedProject {
    project: Project,
    chat: Entity<ChatView>,
    _attention: Subscription,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum QuitPhase {
    #[default]
    Running,
    Confirming,
    Saving,
    Stopping,
}

enum LoadedProject {
    Cached(usize),
    New(Box<Startup>),
}

pub struct ProjectView {
    store: Store,
    home: Option<PathBuf>,
    tools: Tools,
    projects: Vec<OpenedProject>,
    active: Option<usize>,
    recent: Vec<Project>,
    opening: Option<PathBuf>,
    pending: Option<PathBuf>,
    picking: bool,
    problem: Option<String>,
    save_problem_seen: bool,
    background_problem: Option<String>,
    focus: FocusHandle,
    /// The Dock badge and desktop notifications, once the window is open (not in tests).
    attention: Option<NativeAttention>,
    quit_guard: Option<NativeQuitGuard>,
    quit_phase: QuitPhase,
    quit_attempt: u64,
    quit_save: Option<thread::QuitSave>,
    quit_observation: Option<newjen_core::store::SaveObservation>,
    #[cfg(test)]
    load_control: Option<async_channel::Receiver<()>>,
    settling: Vec<thread::QuitSave>,
    quit_elapsed: Duration,
    quit_started: Option<std::time::Instant>,
    #[cfg(test)]
    quit_control: Option<async_channel::Receiver<Result<(), String>>>,
    quit_failure: Option<String>,
    quit_dialog: bool,
    confirm_dialog: bool,
    chooser_search: Entity<InputState>,
    chooser_row: usize,
    chooser_scroll: ScrollHandle,
    keep_open_focus: FocusHandle,
    focus_quit_choice: bool,
    /// The badge count last shown, so AppKit is only asked when it changes.
    badge: usize,
    attention_note: Option<&'static str>,
    /// The agent program every loaded project's threads start, for the tests.
    #[cfg(test)]
    stand_in: Option<PathBuf>,
    _errors: Task<()>,
    _focus: [Subscription; 2],
    _search: Subscription,
    _attention_tasks: Vec<Task<()>>,
}

impl ProjectView {
    pub fn new(
        store: Store,
        home: Option<PathBuf>,
        initial: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (recent, problem) = match store.projects() {
            Ok(projects) => (projects, None),
            Err(error) => (
                vec![],
                Some(format!("Couldn't read recent projects: {error:#}")),
            ),
        };
        crate::update_menus(&recent, cx);
        let errors = store.write_errors();
        let listener = cx.spawn(async move |view, cx| {
            while let Ok(error) = errors.recv().await {
                if view
                    .update(cx, |view, cx| {
                        if !view.save_problem_seen {
                            view.background_problem = Some(error.to_string());
                            view.save_problem_seen = true;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let focus = cx.focus_handle();
        let chooser_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let search = cx.subscribe_in(
            &chooser_search,
            window,
            |view, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    view.chooser_row = 0;
                    view.chooser_scroll.scroll_to_item(0);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } if view.active.is_none() => {
                    view.open_chosen(window, cx)
                }
                _ => {}
            },
        );
        let mut view = Self {
            store,
            home,
            tools: Tools::find(),
            projects: vec![],
            active: None,
            recent,
            opening: None,
            pending: None,
            picking: false,
            problem,
            save_problem_seen: false,
            background_problem: None,
            focus: focus.clone(),
            attention: None,
            quit_guard: None,
            quit_phase: QuitPhase::Running,
            quit_attempt: 0,
            quit_save: None,
            quit_observation: None,
            #[cfg(test)]
            load_control: None,
            settling: Vec::new(),
            quit_elapsed: Duration::ZERO,
            quit_started: None,
            #[cfg(test)]
            quit_control: None,
            quit_failure: None,
            quit_dialog: false,
            confirm_dialog: false,
            chooser_search,
            chooser_row: 0,
            chooser_scroll: ScrollHandle::new(),
            keep_open_focus: cx.focus_handle(),
            focus_quit_choice: false,
            badge: 0,
            attention_note: None,
            #[cfg(test)]
            stand_in: None,
            _errors: listener,
            _attention_tasks: Vec::new(),
            _search: search,
            _focus: [
                cx.on_focus_lost(window, |view, window, cx| {
                    if view.active.is_none() {
                        cx.defer_in(window, |view, window, cx| {
                            if view.active.is_none() {
                                view.focus_chooser(window, cx);
                            }
                        });
                    }
                }),
                // The window's own handle is for the projects screen only: while a project
                // is shown, its keys live under the chat view.
                cx.on_focus(&focus, window, |view, window, cx| {
                    view.hand_focus_to_project(window, cx);
                }),
            ],
        };
        window.focus(&view.focus, cx);
        view.remove_pending_copies(cx);
        if let Some(initial) = initial {
            view.open_project(initial, window, cx);
        } else {
            view.focus_chooser(window, cx);
        }
        view
    }

    pub fn is_stopping(&self) -> bool {
        matches!(self.quit_phase, QuitPhase::Saving | QuitPhase::Stopping)
    }

    pub fn open_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Running {
            return;
        }
        if self.opening.is_some() {
            self.pending = Some(path);
            return;
        }
        self.opening = Some(path.clone());
        let store = self.store.clone();
        let home = self.home.clone();
        let tools = self.tools.clone();
        let opened: Vec<PathBuf> = self
            .projects
            .iter()
            .map(|slot| slot.project.path.clone())
            .collect();
        #[cfg(test)]
        let stand_in = self.stand_in.clone();
        #[cfg(test)]
        let load_control = self.load_control.take();
        let loading = cx.background_spawn(async move {
            #[cfg(test)]
            if let Some(control) = load_control {
                control.recv().await.unwrap();
            }
            let path = project_folder(&path)?;
            let project = store.open_project(&path)?;
            let loaded = match opened.iter().position(|opened| *opened == path) {
                Some(index) => LoadedProject::Cached(index),
                None => {
                    let startup = load_project(store.clone(), project, home, tools)?;
                    #[cfg(test)]
                    let startup = {
                        let mut startup = startup;
                        if let Some(program) = stand_in {
                            startup.base_config.program = program;
                        }
                        startup
                    };
                    LoadedProject::New(Box::new(startup))
                }
            };
            Ok::<_, anyhow::Error>((loaded, store.projects()?))
        });
        cx.spawn_in(window, async move |view, cx| {
            let loaded = loading.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.opening = None;
                if view.quit_phase == QuitPhase::Saving {
                    let settling = view.quit_save.take().into_iter().collect();
                    view.quit_save = Some(thread::QuitSave::start(
                        view.store.clone(),
                        [],
                        view.quit_observation.as_ref().unwrap().clone(),
                        settling,
                    ));
                    cx.notify();
                    return;
                }
                if view.quit_phase != QuitPhase::Running {
                    return;
                }
                match loaded {
                    Ok((loaded, recent)) => {
                        let index = match loaded {
                            LoadedProject::Cached(index) => index,
                            LoadedProject::New(startup) => {
                                if let Some(active) = view.active {
                                    view.projects[active]
                                        .chat
                                        .update(cx, |chat, cx| chat.set_visible(false, window, cx));
                                }
                                let project = startup.project.clone();
                                let chat = cx.new(|cx| ChatView::new(*startup, window, cx));
                                let shell = cx.weak_entity();
                                let opener = cx.weak_entity();
                                chat.update(cx, |chat, _| {
                                    chat.set_project_chooser(std::rc::Rc::new(
                                        move |window, cx| {
                                            let _ = shell.update(cx, |view, cx| {
                                                view.show_projects(&ShowProjects, window, cx);
                                            });
                                        },
                                    ));
                                    chat.set_thread_opener(std::rc::Rc::new(
                                        move |thread_id, window, cx| {
                                            let _ = opener.update(cx, |view, cx| {
                                                view.reveal_thread(thread_id, window, cx);
                                            });
                                        },
                                    ));
                                });
                                let attention = cx.subscribe(&chat, Self::attend);
                                view.projects.push(OpenedProject {
                                    project,
                                    chat,
                                    _attention: attention,
                                });
                                for (index, project) in view.projects.iter().enumerate() {
                                    let peers = view
                                        .projects
                                        .iter()
                                        .enumerate()
                                        .filter(|(peer, _)| *peer != index)
                                        .map(|(_, project)| project.chat.downgrade())
                                        .collect();
                                    project
                                        .chat
                                        .update(cx, |chat, _| chat.set_copy_peers(peers));
                                }
                                view.projects.len() - 1
                            }
                        };
                        view.show(index, window, cx);
                        view.refresh_badge(cx);
                        let note = view.attention_note;
                        view.projects[index]
                            .chat
                            .update(cx, |chat, cx| chat.set_attention_note(note, cx));
                        view.recent = recent;
                        crate::update_menus(&view.recent, cx);
                        view.problem = None;
                    }
                    Err(error) => view.problem = Some(format!("Couldn't open project: {error:#}")),
                }
                if let Some(pending) = view.pending.take() {
                    view.open_project(pending, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn show(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(active) = self.active
            && active != index
        {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.set_visible(false, window, cx));
        }
        self.active = Some(index);
        self.projects[index]
            .chat
            .update(cx, |chat, cx| chat.set_visible(true, window, cx));
        // Checked after the frame that shows the project: focus a listener moves in this
        // frame is not drawn until the next one.
        cx.defer_in(window, |view, window, cx| {
            view.hand_focus_to_project(window, cx)
        });
        window.set_window_title(&format!(
            "{} | Newjen",
            self.projects[index].project.path.display()
        ));
        cx.notify();
    }

    /// Gives the keyboard to the shown project if the window's own handle has it: the chat
    /// view's shortcuts and approval keys only work with focus inside it.
    fn hand_focus_to_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_stopping() {
            return;
        }
        if let Some(active) = self.active
            && self.focus.is_focused(window)
        {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.take_focus(window, cx));
        }
    }

    fn show_projects(&mut self, _: &ShowProjects, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Running {
            return;
        }
        if let Some(active) = self.active.take() {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.set_visible(false, window, cx));
        }
        self.focus_chooser(window, cx);
        window.set_window_title("Newjen");
        cx.notify();
    }

    fn pick_project(&mut self, _: &OpenProject, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Running {
            return;
        }
        if self.picking || self.opening.is_some() {
            return;
        }
        self.picking = true;
        let selected = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open Project".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let selected = selected.await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.picking = false;
                match selected {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            view.open_project(path, window, cx);
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        view.problem = Some(format!("Couldn't choose a folder: {error:#}"))
                    }
                    Err(error) => view.problem = Some(format!("Couldn't choose a folder: {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_recent(
        &mut self,
        action: &OpenRecentProject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_phase != QuitPhase::Running {
            return;
        }
        self.open_project(action.0.clone(), window, cx);
    }

    fn remove_pending_copies(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let tools = self.tools.clone();
        let removed = cx.background_spawn(async move {
            let mut problems = Vec::new();
            for removal in thread::Removal::pending(&store)? {
                if let Err(error) = removal.run(&tools) {
                    problems.push(format!("{error:#}"));
                }
            }
            anyhow::ensure!(problems.is_empty(), "{}", problems.join("\n"));
            Ok::<_, anyhow::Error>(())
        });
        cx.spawn(async move |view, cx| {
            if let Err(error) = removed.await {
                let _ = view.update(cx, |view, cx| {
                    view.background_problem =
                        Some(format!("Couldn't remove an archived Rift copy: {error:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub fn quit(&mut self, cx: &mut Context<Self>) -> Result<(), thread::QuitError> {
        if self.quit_phase == QuitPhase::Stopping {
            return Ok(());
        }
        for old in std::mem::take(&mut self.settling) {
            let _ = old.wait_blocking();
        }
        let quitting: Vec<_> = self
            .projects
            .iter()
            .flat_map(|project| project.chat.update(cx, |chat, cx| chat.begin_quit(cx)))
            .collect();
        thread::finish_quit(&self.store, quitting)
    }

    // -----------------------------------------------------------------------------------------
    // Attention and quitting, over every loaded project
    // -----------------------------------------------------------------------------------------

    /// The Dock badge, desktop notifications and the native quit hook. Called once when the
    /// window opens; they live as long as the app.
    pub fn install_attention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let attention = NativeAttention::new();
        let clicks = attention.clicks();
        self.attention = Some(attention);
        self.refresh_badge(cx);
        self.sync_attention_note(cx);
        // A click on a notification brings the thread on screen. It never answers.
        self._attention_tasks
            .push(cx.spawn_in(window, async move |view, cx| {
                while let Ok(id) = clicks.recv().await {
                    let _ = view.update_in(cx, |view, window, cx| {
                        cx.activate(true);
                        if let Some(attention) = &mut view.attention {
                            attention.dismiss(&id);
                        }
                        view.reveal_thread(&id, window, cx);
                    });
                }
            }));
        self._attention_tasks.push(cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(ATTENTION_POLL).await;
                let polled = view.update(cx, |view, cx| {
                    if let Some(attention) = &mut view.attention {
                        attention.poll();
                    }
                    view.sync_attention_note(cx);
                });
                if polled.is_err() {
                    break;
                }
            }
        }));
        match NativeQuitGuard::install() {
            Ok(guard) => {
                let requests = guard.requests();
                self.quit_guard = Some(guard);
                self._attention_tasks
                    .push(cx.spawn_in(window, async move |view, cx| {
                        while requests.recv().await.is_ok() {
                            let _ = view.update_in(cx, |view, window, cx| {
                                cx.activate(true);
                                view.request_quit(window, cx);
                            });
                        }
                    }));
            }
            Err(error) => {
                self.problem = Some(format!(
                    "Native quit confirmation is unavailable: {error:#}"
                ));
            }
        }
    }

    /// A project's thread changed in a way the badge or a notification cares about.
    fn attend(&mut self, _: Entity<ChatView>, event: &Attention, cx: &mut Context<Self>) {
        if let Some(attention) = &mut self.attention {
            match event {
                Attention::Dismiss(thread_id) => attention.dismiss(thread_id),
                Attention::Notify {
                    thread_id,
                    title,
                    kind,
                } => attention.notify(thread_id, title, *kind),
            }
        }
        self.refresh_badge(cx);
        self.sync_attention_note(cx);
    }

    /// Tells every project's inbox why desktop notifications are off, when they are.
    fn sync_attention_note(&mut self, cx: &mut Context<Self>) {
        let note = self
            .attention
            .as_ref()
            .and_then(NativeAttention::capability_note);
        if note == self.attention_note {
            return;
        }
        self.attention_note = note;
        for project in &self.projects {
            project
                .chat
                .update(cx, |chat, cx| chat.set_attention_note(note, cx));
        }
    }

    /// Threads waiting for an approval, in every loaded project.
    fn waiting_threads(&self, cx: &App) -> usize {
        self.projects
            .iter()
            .map(|project| project.chat.read(cx).waiting_threads(cx))
            .sum()
    }

    fn refresh_badge(&mut self, cx: &App) {
        let waiting = self.waiting_threads(cx);
        if waiting == self.badge {
            return;
        }
        self.badge = waiting;
        if let Some(attention) = &self.attention {
            attention.set_badge(waiting);
        }
    }

    /// Brings `thread_id` on screen, switching to its project if needed.
    fn reveal_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Running {
            return;
        }
        let Some(index) = self
            .projects
            .iter()
            .position(|project| project.chat.read(cx).has_thread(thread_id, cx))
        else {
            return;
        };
        self.show(index, window, cx);
        self.projects[index]
            .chat
            .update(cx, |chat, cx| chat.show_thread(thread_id, window, cx));
    }

    fn quit_blockers(&self, cx: &App) -> QuitBlockers {
        let mut blockers = QuitBlockers::default();
        for project in &self.projects {
            blockers.add(&project.chat.read(cx).quit_blockers(cx));
        }
        blockers
    }

    /// ⌘Q, ⌘W, the window's close button and macOS asking the app to quit all come here:
    /// one confirmation when work would be lost, otherwise the app quits.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase == QuitPhase::Confirming {
            self.confirm_quit(window, cx);
            return;
        }
        if self.quit_phase != QuitPhase::Running {
            if self.quit_dialog {
                window.activate_window();
                cx.activate(true);
            }
            return;
        }
        let blockers = self.quit_blockers(cx);
        let unsent = self
            .projects
            .iter()
            .any(|p| p.chat.read(cx).has_unsent_text(cx));
        if !blockers.any() && !unsent {
            self.start_saving(window, cx);
            return;
        }
        self.quit_phase = QuitPhase::Confirming;
        self.confirm_dialog = true;
        let mut threads = Vec::new();
        for project in &self.projects {
            threads.extend(project.chat.read(cx).busy_threads(cx));
        }
        let mut notes = blockers.notes();
        if unsent {
            notes.push("Unsent composer text will be discarded.".to_string());
        }
        let message = if threads.is_empty() {
            "Quitting stops what is still running."
        } else {
            "These stop mid-task. Their work so far is kept."
        };
        let view = cx.entity().downgrade();
        window.activate_window();
        cx.activate(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let (keep, quit, cancel) = (view.clone(), view.clone(), view.clone());
            let material = crate::theme::material(cx);
            let muted = cx.theme().muted_foreground;
            let keep_focus = view
                .upgrade()
                .map(|view| view.read(cx).keep_open_focus.clone());
            let keep_button = ui::button("quit-keep", "", ui::Look::Secondary, ui::Size::Md, cx)
                .child(
                    gpui_kit::base::h_flex()
                        .gap_2()
                        .items_center()
                        .child("Keep working")
                        .child(ui::key_caps("escape", cx)),
                )
                .on_click(move |_, window, cx| {
                    let _ = keep.update(cx, |view, cx| view.keep_working(window, cx));
                });
            let keep_button = match &keep_focus {
                Some(focus) => keep_button.track_focus(focus),
                None => keep_button,
            };
            let quit_button = ui::button("quit-confirm", "", ui::Look::Secondary, ui::Size::Md, cx)
                .danger()
                .child(
                    gpui_kit::base::h_flex()
                        .gap_2()
                        .items_center()
                        .child("Quit")
                        .child(ui::key_caps("cmd-q", cx)),
                )
                .on_click(move |_, window, cx| {
                    let _ = quit.update(cx, |view, cx| view.confirm_quit(window, cx));
                });
            let list = (!threads.is_empty()).then(|| {
                v_flex()
                    .id("quit-threads")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .p_1()
                    .rounded_lg()
                    .bg(material.glass_selected)
                    .children(threads.iter().enumerate().map(|(ix, (title, activity))| {
                        gpui_kit::base::h_flex()
                            .id(("quit-thread", ix))
                            .gap_2()
                            .px_2()
                            .py_1p5()
                            .items_center()
                            .child(
                                div().flex_none().child(
                                    gpui_kit::component::spinner::Spinner::new()
                                        .small()
                                        .color(cx.theme().info),
                                ),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(220.))
                                    .child(ui::ellipsis(title.clone())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(ui::ellipsis(activity.clone())),
                            )
                    }))
            });
            let _ = window;
            dialog
                .centered()
                .width(px(440.))
                .bg(material.card)
                .entrance(DialogEntrance::None)
                .close_button(false)
                .on_ok(|_, _, _| false)
                .on_cancel(move |_, window, cx| {
                    let _ = cancel.update(cx, |view, cx| view.keep_working(window, cx));
                    false
                })
                .title(
                    div()
                        .debug_selector(|| "quit-confirm-title".into())
                        .child("Quit Newjen?"),
                )
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_color(muted).child(message))
                        .children(list)
                        .children(
                            notes
                                .iter()
                                .map(|note| div().text_sm().text_color(muted).child(note.clone())),
                        ),
                )
                .footer(
                    gpui_kit::base::h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            div()
                                .debug_selector(|| "quit-keep-working".into())
                                .child(keep_button),
                        )
                        .child(
                            div()
                                .debug_selector(|| "quit-confirm-button".into())
                                .child(quit_button),
                        ),
                )
        });
        cx.defer_in(window, |view, window, cx| {
            if view.quit_phase == QuitPhase::Confirming {
                window.focus(&view.keep_open_focus, cx);
            }
        });
        cx.notify();
    }

    fn close_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_dialog {
            self.confirm_dialog = false;
            window.close_dialog(cx);
        }
    }

    fn keep_working(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Confirming {
            return;
        }
        self.close_confirm(window, cx);
        self.quit_phase = QuitPhase::Running;
        if let Some(active) = self.active {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.take_focus(window, cx));
        } else {
            self.focus_chooser(window, cx);
        }
        cx.notify();
    }

    fn confirm_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Confirming {
            return;
        }
        self.close_confirm(window, cx);
        self.start_saving(window, cx);
    }

    fn start_saving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_phase = QuitPhase::Saving;
        self.quit_attempt += 1;
        let attempt = self.quit_attempt;
        self.quit_elapsed = Duration::ZERO;
        self.quit_started = Some(cx.background_executor().now());
        self.quit_failure = None;
        self.focus_quit_choice = false;
        window.focus(&self.focus, cx);
        let failure = self.store.observe_saves();
        self.quit_observation = Some(failure.clone());
        let quitting: Vec<_> = self
            .projects
            .iter()
            .flat_map(|p| {
                p.chat.update(cx, |chat, cx| {
                    chat.set_quit_saving(true, cx);
                    chat.prepare_quit(window, cx)
                })
            })
            .collect();
        self.quit_save = Some(thread::QuitSave::start(
            self.store.clone(),
            quitting,
            failure,
            std::mem::take(&mut self.settling),
        ));
        cx.notify();
        cx.spawn_in(window, async move |view, cx| {
            let mut delay = Duration::from_millis(25);
            loop {
                cx.background_executor().timer(delay).await;
                let next = view
                    .update_in(cx, |view, window, cx| {
                        if view.quit_phase != QuitPhase::Saving || view.quit_attempt != attempt {
                            return None;
                        }
                        let elapsed = cx.background_executor().now() - view.quit_started.unwrap();
                        view.advance_saving(attempt, elapsed, window, cx);
                        if view.quit_phase != QuitPhase::Saving {
                            return None;
                        }
                        let until_boundary = [
                            Duration::from_millis(300),
                            Duration::from_secs(3),
                            Duration::from_secs(10),
                        ]
                        .into_iter()
                        .find(|deadline| *deadline > elapsed)
                        .map(|deadline| deadline - elapsed);
                        Some(
                            until_boundary
                                .unwrap_or(Duration::from_millis(25))
                                .min(Duration::from_millis(25)),
                        )
                    })
                    .ok()
                    .flatten();
                let Some(next) = next else {
                    break;
                };
                delay = next;
            }
        })
        .detach();
    }

    fn advance_saving(
        &mut self,
        attempt: u64,
        elapsed: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.quit_phase != QuitPhase::Saving || self.quit_attempt != attempt {
            return;
        }
        if self.focus_quit_choice {
            window.focus(&self.keep_open_focus, cx);
            self.focus_quit_choice = false;
        }
        let had_choices =
            self.quit_elapsed >= Duration::from_secs(10) || self.quit_failure.is_some();
        self.quit_elapsed = elapsed;
        if self.quit_failure.is_none() {
            let result = self.quit_save.as_ref().and_then(|save| save.poll());
            #[cfg(test)]
            let result = if let Some(control) = &self.quit_control {
                control.try_recv().ok()
            } else {
                result
            };
            match result {
                Some(Ok(())) if self.opening.is_none() => {
                    self.commit_quit(window, cx);
                    return;
                }
                Some(Ok(())) => {}
                Some(Err(error)) => self.quit_failure = Some(error),
                None => {}
            }
        }
        if !self.quit_dialog
            && (self.quit_elapsed >= Duration::from_millis(300) || self.quit_failure.is_some())
        {
            self.quit_dialog = true;
            window.activate_window();
            cx.activate(true);
            let view = cx.entity().downgrade();
            window.open_dialog(cx, move |dialog, _, cx| {
                let Some(entity) = view.upgrade() else { return dialog; };
                let state = entity.read(cx);
                let choices = state.quit_elapsed >= Duration::from_secs(10) || state.quit_failure.is_some();
                let title = if state.quit_failure.is_some() { "Couldn't save your work" }
                    else if state.quit_elapsed >= Duration::from_secs(10) { "Your work isn't confirmed saved" }
                    else if state.quit_elapsed >= Duration::from_secs(3) { "Saving is taking longer than usual…" }
                    else { "Saving your work…" };
                let message = if choices { "Recent changes may be lost if you quit. Work will stay stopped if you keep Newjen open." }
                    else { "Newjen is saving your changes and stopping work. Please keep this window open." };
                let selector = if state.quit_failure.is_some() { "quit-failed" }
                    else if choices { "quit-timed-out" }
                    else if state.quit_elapsed >= Duration::from_secs(3) { "quit-slow" }
                    else { "quit-saving" };
                let keep_focus = state.keep_open_focus.clone();
                let keep = view.clone();
                let anyway = view.clone();
                dialog.centered().bg(crate::theme::material(cx).card).entrance(DialogEntrance::None).close_button(false).overlay_closable(false).keyboard(false).on_ok(|_, _, _| false).on_cancel(|_, _, _| false)
                    .title(div().debug_selector(move || selector.into()).child(title)).child(message)
                    .when(choices, |dialog| dialog.footer(gpui_kit::base::h_flex().gap_2()
                        .child(div().debug_selector(|| "quit-anyway-button".into()).child(ui::button("quit-anyway", "Quit anyway", ui::Look::Secondary, ui::Size::Md, cx)
                            .on_click(move |_, window, cx| { let _ = anyway.update(cx, |view, cx| {
                                if view.quit_attempt == attempt && view.quit_phase == QuitPhase::Saving { view.commit_quit(window, cx); }
                            }); })))
                        .child(div().debug_selector(|| "quit-keep-button".into()).child(ui::button("keep-newjen-open", "Keep Newjen open", ui::Look::Primary, ui::Size::Md, cx).track_focus(&keep_focus)
                            .on_click(move |_, window, cx| { let _ = keep.update(cx, |view, cx| view.keep_open(attempt, window, cx)); }))) ))
            });
        }
        if !had_choices
            && (self.quit_elapsed >= Duration::from_secs(10) || self.quit_failure.is_some())
        {
            self.focus_quit_choice = true;
        }
        window.refresh();
        cx.notify();
    }

    fn keep_open(&mut self, attempt: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.quit_phase != QuitPhase::Saving || self.quit_attempt != attempt {
            return;
        }
        self.quit_attempt += 1;
        self.quit_phase = QuitPhase::Running;
        if let Some(save) = self.quit_save.take() {
            self.settling.push(save);
        }
        self.quit_dialog = false;
        for p in &self.projects {
            p.chat
                .update(cx, |chat, cx| chat.set_quit_saving(false, cx));
        }
        window.close_dialog(cx);
        if let Some(active) = self.active {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.take_focus(window, cx));
        } else {
            self.focus_chooser(window, cx);
        }
        cx.notify();
    }

    fn commit_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.quit_phase = QuitPhase::Stopping;
        if self.quit_dialog {
            window.close_dialog(cx);
            self.quit_dialog = false;
        }
        self.allow_quit();
        cx.quit();
    }

    fn allow_quit(&self) {
        if let Some(guard) = &self.quit_guard {
            guard.allow_quit();
        }
    }

    fn chooser_matches(&self, cx: &App) -> Vec<usize> {
        filter_recents(
            &self.recent,
            &self.chooser_search.read(cx).value(),
            self.home.as_deref(),
        )
    }

    fn focus_chooser(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recent.is_empty() {
            window.focus(&self.focus, cx);
        } else {
            let handle = gpui::Focusable::focus_handle(self.chooser_search.read(cx), cx);
            window.focus(&handle, cx);
        }
    }

    fn open_chosen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let matches = self.chooser_matches(cx);
        if let Some(&ix) = matches.get(self.chooser_row.min(matches.len().saturating_sub(1))) {
            let path = self.recent[ix].path.clone();
            self.open_project(path, window, cx);
        }
    }

    fn move_chooser_row(&mut self, down: bool, cx: &mut Context<Self>) {
        let count = self.chooser_matches(cx).len();
        let row = self.chooser_row.min(count.saturating_sub(1));
        self.chooser_row = if down {
            (row + 1).min(count.saturating_sub(1))
        } else {
            row.saturating_sub(1)
        };
        self.chooser_scroll.scroll_to_item(self.chooser_row);
        cx.notify();
    }

    fn welcome(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let material = crate::theme::material(cx);
        let open = ui::button_with_icon(
            "open-project",
            "Open Folder…",
            ui::IconName::FolderOpen,
            ui::Look::Primary,
            ui::Size::Md,
            cx,
        )
        .disabled(self.picking || self.opening.is_some())
        .on_click(cx.listener(|view, _, window, cx| view.pick_project(&OpenProject, window, cx)));
        let mut content = ui::card(v_flex(), cx)
            .p_4()
            .gap_3()
            .min_h_0()
            .max_h_full()
            .w_full()
            .max_w(px(520.))
            .capture_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, _, cx| {
                if view.active.is_some() || event.keystroke.modifiers.modified() {
                    return;
                }
                let key = event.keystroke.key.as_str();
                if matches!(key, "up" | "down") {
                    view.move_chooser_row(key == "down", cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                gpui_kit::base::h_flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Projects"),
                    )
                    .child(open),
            );
        if self.recent.is_empty() {
            content = content.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Choose the folder where your agents will work."),
            );
        } else {
            let matches = self.chooser_matches(cx);
            let selected = self.chooser_row.min(matches.len().saturating_sub(1));
            let now = newjen_core::store::now_ms();
            let rows: Vec<_> = matches
                .iter()
                .enumerate()
                .map(|(position, &ix)| {
                    let project = &self.recent[ix];
                    let path = project.path.clone();
                    let name = project_name(&project.path);
                    let shown = tilde(&project.path, self.home.as_deref());
                    let id = project.id;
                    let ago = relative_time(project.last_opened_at, now);
                    let hint = shown.clone();
                    gpui_kit::base::h_flex()
                        .id(("recent-project", id as u64))
                        .debug_selector(move || format!("recent-project-{id}"))
                        .flex_none()
                        .w_full()
                        .gap_3()
                        .px_2()
                        .py_2()
                        .items_center()
                        .rounded_lg()
                        .cursor_pointer()
                        .when(position == selected, |row| row.bg(material.glass_selected))
                        .hover(|row| row.bg(material.glass_hover))
                        .tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(hint.clone())
                                .build(window, cx)
                        })
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.open_project(path.clone(), window, cx)
                        }))
                        .child(ui::monogram(&name, cx))
                        .child(
                            v_flex().flex_1().min_w_0().child(ui::ellipsis(name)).child(
                                ui::middle_ellipsis(shown)
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(ago),
                        )
                        .into_any_element()
                })
                .collect();
            content = content
                .child(
                    gpui_kit::component::input::Input::new(&self.chooser_search)
                        .prefix(ui::icon(
                            ui::IconName::Search,
                            gpui_kit::component::Size::Small,
                            cx.theme().muted_foreground,
                        ))
                        .cleanable(true),
                )
                .child(if rows.is_empty() {
                    div()
                        .py_4()
                        .text_color(cx.theme().muted_foreground)
                        .child("No projects match your search.")
                        .into_any_element()
                } else {
                    v_flex()
                        .id("recent-projects")
                        .debug_selector(|| "recent-projects".to_string())
                        .track_scroll(&self.chooser_scroll)
                        .min_h_0()
                        .max_h(px(340.))
                        .overflow_y_scroll()
                        .children(rows)
                        .into_any_element()
                });
        }
        v_flex()
            .size_full()
            .child(ui::glass_title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(content),
            )
            .into_any_element()
    }
}

/// The recent projects whose folder name or `~` path contains `query`, ignoring case, as
/// indexes into `projects` in their original order.
fn filter_recents(projects: &[Project], query: &str, home: Option<&Path>) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    projects
        .iter()
        .enumerate()
        .filter(|(_, project)| {
            query.is_empty()
                || project_name(&project.path).to_lowercase().contains(&query)
                || tilde(&project.path, home).to_lowercase().contains(&query)
        })
        .map(|(ix, _)| ix)
        .collect()
}

fn project_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.active {
            Some(active) => self.projects[active].chat.clone().into_any_element(),
            None => self.welcome(cx),
        };
        v_flex()
            .size_full()
            .relative()
            .capture_key_down(cx.listener(|view, _, _, cx| {
                if view.is_stopping() && !view.quit_dialog {
                    cx.stop_propagation();
                }
            }))
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::pick_project))
            .on_action(cx.listener(Self::show_projects))
            .on_action(cx.listener(Self::open_recent))
            .text_color(cx.theme().foreground)
            .child(div().flex_1().min_h_0().child(body))
            .when_some(
                self.background_problem
                    .clone()
                    .or_else(|| self.problem.clone()),
                |root, problem| {
                    root.child(
                        div()
                            .flex_none()
                            .p_2()
                            .text_sm()
                            .bg(cx.theme().muted)
                            .child(problem)
                            .child(
                                ui::button(
                                    "dismiss-project-error",
                                    "Dismiss",
                                    ui::Look::Ghost,
                                    ui::Size::Sm,
                                    cx,
                                )
                                .on_click(cx.listener(
                                    |view, _, _, cx| {
                                        view.problem = None;
                                        view.background_problem = None;
                                        cx.notify();
                                    },
                                )),
                            ),
                    )
                },
            )
            .when(self.is_stopping(), |root| {
                root.child(div().absolute().inset_0().occlude())
            })
            .when_some(self.opening.as_ref(), |root, _| {
                root.child(div().flex_none().p_2().text_sm().child("Opening project…"))
            })
    }
}

fn project_folder(path: &Path) -> anyhow::Result<PathBuf> {
    crate::check_project(path).map_err(anyhow::Error::msg)?;
    Ok(path.canonicalize()?)
}

fn load_project(
    store: Store,
    project: Project,
    home: Option<PathBuf>,
    tools: Tools,
) -> anyhow::Result<Startup> {
    let base_config = SessionConfig::new(project.path.clone());
    let threads = Thread::load_all(&store, &project, &base_config)?;
    // Rift hooks run with the login shell's PATH, not the minimal one an app started from
    // Finder has. Resolving it can take a moment, which is why this runs off the UI thread.
    let tools = tools.with_env([(
        "PATH".to_string(),
        newjen_core::resolved_path().to_string_lossy().into_owned(),
    )]);
    Ok(Startup {
        store,
        project,
        base_config,
        threads,
        problem: None,
        home,
        tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};
    use gpui_kit::base::Root;

    fn setup<'a>(
        cx: &'a mut TestAppContext,
        name: &str,
    ) -> (Entity<ProjectView>, PathBuf, &'a mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::keys::bind(cx);
        });
        cx.executor().allow_parking();
        let path = std::env::temp_dir().join(format!(
            "newjen-project-{name}-{}-{}",
            std::process::id(),
            newjen_core::store::now_ms()
        ));
        let store = Store::open_in(&path).unwrap();
        let mut shell = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| ProjectView::new(store, None, None, window, cx));
            shell = Some(view.clone());
            Root::new(view, window, cx)
        });
        (shell.unwrap(), path, cx)
    }

    async fn open(shell: &Entity<ProjectView>, path: PathBuf, cx: &mut VisualTestContext) {
        cx.update(|window, cx| shell.update(cx, |view, cx| view.open_project(path, window, cx)));
        cx.condition(shell, |view, _| view.opening.is_none()).await;
        cx.run_until_parked();
    }

    fn finish(shell: &Entity<ProjectView>, path: PathBuf, cx: &mut VisualTestContext) {
        cx.update(|_, cx| shell.update(cx, |view, cx| view.quit(cx)))
            .unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }

    #[gpui::test]
    async fn welcome_picker_cancellation_and_project_switching_preserve_drafts(
        cx: &mut TestAppContext,
    ) {
        let (shell, path, cx) = setup(cx, "switch");
        assert!(cx.update(|_, cx| shell.read(cx).store.projects().unwrap().is_empty()));
        assert!(cx.update(|_, cx| shell.read(cx).projects.is_empty()));
        cx.simulate_keystrokes("cmd-o");
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(!options.files && options.directories && !options.multiple);
            None
        });
        cx.condition(&shell, |view, _| !view.picking).await;
        assert!(cx.update(|_, cx| shell.read(cx).active.is_none()));
        let a = path.join("api");
        let b = path.join("website");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        cx.simulate_keystrokes("cmd-o");
        cx.simulate_path_prompt_response(|_| Some(vec![a.clone()]));
        cx.condition(&shell, |view, _| view.active.is_some()).await;
        cx.run_until_parked();
        let first = cx.update(|_, cx| shell.read(cx).projects[0].chat.clone());
        let first_field = cx.update(|_, cx| first.read(cx).field_focus(cx).unwrap());
        cx.simulate_input("keep the api draft");
        cx.simulate_keystrokes("cmd-shift-o");
        assert!(cx.update(|_, cx| shell.read(cx).active.is_none()));
        open(&shell, a.clone(), cx).await;
        let project_row = cx.debug_bounds("switch-project").unwrap();
        cx.simulate_click(project_row.center(), gpui::Modifiers::default());
        assert!(cx.update(|_, cx| shell.read(cx).active.is_none()));
        open(&shell, a.clone(), cx).await;
        open(&shell, b, cx).await;
        cx.simulate_input("website draft");
        let alias = path.join("alias");
        std::os::unix::fs::symlink(&a, &alias).unwrap();
        open(&shell, alias, cx).await;
        assert_eq!(cx.update(|_, cx| shell.read(cx).projects.len()), 2);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).projects[0].chat.clone()),
            first
        );
        assert!(cx.update(|window, _| first_field.is_focused(window)));
        cx.simulate_input(" still here");
        cx.simulate_keystrokes("cmd-a");
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
            "keep the api draft still here"
        );
        open(&shell, path.join("missing"), cx).await;
        assert_eq!(cx.update(|_, cx| shell.read(cx).active), Some(0));
        assert!(cx.update(|_, cx| {
            shell
                .read(cx)
                .problem
                .as_ref()
                .unwrap()
                .contains("does not exist")
        }));
        finish(&shell, path, cx);
    }

    #[gpui::test]
    async fn a_project_brought_back_from_the_projects_screen_has_the_keyboard(
        cx: &mut TestAppContext,
    ) {
        use crate::chat_view::input_tests::{answers, pending, script, wait_answers};
        let (shell, path, cx) = setup(cx, "return-focus");
        // Focus listeners only see focus paths of an active window, as in the app.
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let program = script(&path);
        cx.update(|_, cx| shell.update(cx, |view, _| view.stand_in = Some(program)));
        let api = path.join("api");
        let website = path.join("website");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::create_dir_all(&website).unwrap();
        open(&shell, api.clone(), cx).await;
        let first = cx.update(|_, cx| shell.read(cx).projects[0].chat.clone());
        cx.simulate_input("first thread");
        cx.simulate_keystrokes("cmd-n");
        cx.simulate_input("second thread");
        assert_eq!(cx.update(|_, cx| first.read(cx).shown(cx).0), 0);
        open(&shell, website.clone(), cx).await;
        cx.simulate_keystrokes("cmd-shift-o");
        assert!(cx.update(|_, cx| shell.read(cx).active.is_none()));
        let back = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                shell.update(cx, |view, cx| {
                    view.open_recent(&OpenRecentProject(api.clone()), window, cx)
                })
            });
        };
        back(cx);
        cx.condition(&shell, |view, _| view.opening.is_none()).await;
        cx.run_until_parked();
        assert_eq!(cx.update(|_, cx| shell.read(cx).active), Some(0));
        let field = cx.update(|_, cx| first.read(cx).field_focus(cx).unwrap());
        assert!(
            cx.update(|window, _| field.is_focused(window)),
            "the composer of the returned project has the keyboard"
        );
        cx.simulate_keystrokes("cmd-2");
        assert_eq!(
            cx.update(|_, cx| first.read(cx).shown(cx).0),
            1,
            "⌘2 switches threads without a click first"
        );
        cx.simulate_input(" and typing lands in it");
        assert_eq!(
            cx.update(|_, cx| first.read(cx).shown(cx).1),
            "first thread and typing lands in it"
        );
        // The same way back to a thread whose approval dock replaced the composer.
        cx.simulate_keystrokes("enter");
        pending(&first, cx).await;
        open(&shell, website, cx).await;
        cx.simulate_keystrokes("cmd-shift-o");
        back(cx);
        cx.condition(&shell, |view, _| view.opening.is_none()).await;
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(400));
        cx.simulate_keystrokes("cmd-enter");
        wait_answers(cx, &path, 1).await;
        assert_eq!(
            answers(&path)[0]["response"]["response"]["behavior"],
            "allow"
        );
        cx.simulate_keystrokes("cmd-1");
        assert_eq!(
            cx.update(|_, cx| first.read(cx).shown(cx).0),
            0,
            "⌘1 switches threads from the dock without a click first"
        );
        // Focus that lands on the window's own handle while a project is shown (a click on
        // the chrome outside the chat, a late focus restore) goes on into the project.
        cx.update(|window, cx| {
            let handle = shell.read(cx).focus.clone();
            window.focus(&handle, cx);
        });
        cx.simulate_keystrokes("cmd-2");
        assert_eq!(
            cx.update(|_, cx| first.read(cx).shown(cx).0),
            1,
            "⌘2 works after the window's handle took the keyboard"
        );
        assert!(!cx.update(|window, cx| shell.read(cx).focus.is_focused(window)));
        finish(&shell, path, cx);
    }

    fn project(path: &str) -> Project {
        Project {
            id: 1,
            path: PathBuf::from(path),
            created_at: 0,
            last_opened_at: 0,
            landing_mode: Default::default(),
        }
    }

    #[test]
    fn the_search_matches_names_and_paths_without_regard_to_case() {
        let projects = [
            project("/Users/ana/Newjen"),
            project("/Users/ana/code/pstack"),
            project("/Users/ana/code/Rift"),
            project("/opt/dotfiles"),
        ];
        let home = Some(Path::new("/Users/ana"));
        let found = |query: &str| filter_recents(&projects, query, home);
        assert_eq!(found(""), [0, 1, 2, 3]);
        assert_eq!(found("   "), [0, 1, 2, 3]);
        assert_eq!(found("RIFT"), [2]);
        assert_eq!(found("code"), [1, 2]);
        assert_eq!(found("~/code"), [1, 2]);
        assert_eq!(found("/opt"), [3]);
        assert_eq!(found("nothing here"), Vec::<usize>::new());
        assert_eq!(project_name(Path::new("/Users/ana/Newjen")), "Newjen");
        assert_eq!(project_name(Path::new("/")), "/");
    }

    #[gpui::test]
    async fn the_chooser_filters_and_opens_with_the_keys(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "chooser-keys");
        cx.update(|window, _| window.activate_window());
        let folder = |name: &str| {
            let folder = path.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            folder.canonicalize().unwrap()
        };
        let (alpha, beta, gamma) = (folder("alpha"), folder("beta"), folder("gamma"));
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                for folder in [&alpha, &beta, &gamma] {
                    view.store.open_project(folder).unwrap();
                }
                view.recent = view.store.projects().unwrap();
                view.focus_chooser(window, cx);
                cx.notify();
            })
        });
        cx.run_until_parked();
        let opened = |shell: &Entity<ProjectView>, cx: &mut VisualTestContext| {
            cx.update(|_, cx| {
                let view = shell.read(cx);
                view.projects[view.active.unwrap()].project.path.clone()
            })
        };
        // Newest first: gamma, beta, alpha. Down moves to beta, Enter opens it.
        cx.simulate_keystrokes("down enter");
        cx.condition(&shell, |view, _| {
            view.opening.is_none() && view.active.is_some()
        })
        .await;
        assert_eq!(opened(&shell, cx), beta);
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| view.show_projects(&ShowProjects, window, cx))
        });
        cx.run_until_parked();
        // Typing filters the list and Enter opens the first match.
        cx.simulate_input("ALP");
        assert!(cx.debug_bounds("recent-project-1").is_some());
        assert!(cx.debug_bounds("recent-project-2").is_none());
        assert!(cx.debug_bounds("recent-project-3").is_none());
        cx.simulate_keystrokes("enter");
        cx.condition(&shell, |view, _| {
            view.opening.is_none() && view.active.is_some()
        })
        .await;
        assert_eq!(opened(&shell, cx), alpha);
        finish(&shell, path, cx);
    }

    #[gpui::test]
    async fn clicking_any_recent_project_row_opens_that_project(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "recent-rows");
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let folder = |name: &str| {
            let folder = path.join(name);
            std::fs::create_dir_all(&folder).unwrap();
            folder.canonicalize().unwrap()
        };
        let (api, website, docs) = (folder("api"), folder("website"), folder("docs"));
        // Two projects were opened in earlier runs; only `api` is loaded now.
        cx.update(|_, cx| {
            shell.update(cx, |view, _| {
                view.store.open_project(&docs).unwrap();
                view.store.open_project(&website).unwrap();
            })
        });
        open(&shell, api.clone(), cx).await;
        cx.simulate_keystrokes("cmd-shift-o");
        cx.run_until_parked();
        let (ids, rows): (Vec<i64>, Vec<PathBuf>) = cx.update(|_, cx| {
            let view = shell.read(cx);
            view.recent
                .iter()
                .map(|project| (project.id, project.path.clone()))
                .unzip()
        });
        assert_eq!(rows[0], api, "the just-hidden project is listed first");
        let click_row = |cx: &mut VisualTestContext, id: i64| {
            let selector: &'static str = Box::leak(format!("recent-project-{id}").into_boxed_str());
            let row = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("missing {selector}"));
            cx.simulate_click(row.center(), gpui::Modifiers::default());
        };
        click_row(cx, ids[2]);
        cx.condition(&shell, |view, _| {
            view.opening.is_none() && view.active.is_some()
        })
        .await;
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| {
                let view = shell.read(cx);
                view.projects[view.active.unwrap()].project.path.clone()
            }),
            rows[2],
            "a recent project that is not loaded opens from its row"
        );
        // Back on the projects screen by menu action, a loaded but hidden project's row.
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| view.show_projects(&ShowProjects, window, cx))
        });
        cx.run_until_parked();
        click_row(cx, ids[0]);
        cx.condition(&shell, |view, _| {
            view.opening.is_none() && view.active.is_some()
        })
        .await;
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| {
                let view = shell.read(cx);
                view.projects[view.active.unwrap()].project.path.clone()
            }),
            api
        );
        assert_eq!(cx.update(|_, cx| shell.read(cx).projects.len()), 2);
        finish(&shell, path, cx);
    }

    #[gpui::test]
    async fn quitting_asks_once_for_work_in_any_loaded_project(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "quit-gate");
        let first = path.join("first");
        let second = path.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        open(&shell, first, cx).await;
        // Nothing to lose: saving starts without a question.
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        assert!(!cx.has_pending_prompt());
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Saving
        );
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| view.keep_open(view.quit_attempt, window, cx))
        });
        cx.run_until_parked();
        // A terminal in the first project, then the second project on screen.
        cx.simulate_keystrokes("cmd-`");
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_blockers(cx).terminals),
            1
        );
        open(&shell, second, cx).await;
        assert_eq!(cx.update(|_, cx| shell.read(cx).active), Some(1));
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_blockers(cx).terminals),
            1
        );
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        cx.run_until_parked();
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        assert!(cx.debug_bounds("quit-confirm-title").is_some());
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Confirming
        );
        // Esc keeps working.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_blockers(cx).terminals),
            1
        );
        // So does the button.
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        cx.run_until_parked();
        click(cx, "quit-keep-working");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        // Quit answers yes.
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        cx.run_until_parked();
        click(cx, "quit-confirm-button");
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Saving
        );
        // Quitting stops the terminal with everything else.
        cx.update(|_, cx| shell.update(cx, |view, cx| view.quit(cx)))
            .unwrap();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_blockers(cx).terminals),
            0
        );
        std::fs::remove_dir_all(path).unwrap();
    }

    #[gpui::test]
    async fn queued_requests_finish_on_the_latest_project(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "queued");
        let first = path.join("first");
        let last = path.join("last");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&last).unwrap();
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                view.open_project(first, window, cx);
                view.open_project(last.clone(), window, cx);
            })
        });
        cx.condition(&shell, |view, _| {
            view.opening.is_none() && view.pending.is_none()
        })
        .await;
        assert_eq!(
            cx.update(|_, cx| {
                let view = shell.read(cx);
                view.projects[view.active.unwrap()].project.path.clone()
            }),
            last.canonicalize().unwrap()
        );
        finish(&shell, path, cx);
    }
    #[gpui::test]
    fn recent_projects_scroll_in_a_small_window_with_long_names(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "recent-layout");
        cx.update(|_, cx| {
            shell.update(cx, |view, cx| {
                for index in 0..40 {
                    view.store
                        .open_project(&path.join(format!(
                            "international-team-{index}/顧客管理-請求処理と監査レポート-service"
                        )))
                        .unwrap();
                }
                view.recent = view.store.projects().unwrap();
                cx.notify();
            })
        });
        cx.simulate_resize(gpui::size(px(640.), px(480.)));
        cx.run_until_parked();
        let list = cx.debug_bounds("recent-projects").unwrap();
        let row = cx.debug_bounds("recent-project-1").unwrap();
        assert!(list.bottom() <= px(480.), "{list:?}");
        assert!(list.size.height > px(100.), "{list:?}");
        assert!(row.right() <= px(640.), "{row:?}");
        finish(&shell, path, cx);
    }
    #[gpui::test]
    async fn confirmed_dialog_cannot_take_keyboard_from_the_project_chooser(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::component::WindowExt as _;
        let (shell, path, cx) = setup(cx, "late-dialog-focus");
        let project = path.join("project");
        std::fs::create_dir_all(&project).unwrap();
        open(&shell, project, cx).await;
        cx.update(|window, cx| {
            window.open_alert_dialog(cx, |alert, _, _| {
                alert.title("Confirm project action").confirm()
            })
        });
        cx.run_until_parked();
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        cx.simulate_keystrokes("enter");
        cx.simulate_keystrokes("cmd-shift-o");
        cx.run_until_parked();
        assert!(cx.update(|_, cx| shell.read(cx).active.is_none()));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        cx.run_until_parked();
        cx.simulate_keystrokes("cmd-o");
        assert!(
            cx.did_prompt_for_paths(),
            "the welcome screen keeps its Open Project shortcut"
        );
        cx.simulate_path_prompt_response(|_| None);
        cx.condition(&shell, |view, _| !view.picking).await;
        finish(&shell, path, cx);
    }
    fn controlled_quit(
        shell: &Entity<ProjectView>,
        cx: &mut VisualTestContext,
    ) -> async_channel::Sender<Result<(), String>> {
        let (send, receive) = async_channel::unbounded();
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                view.quit_control = Some(receive);
                view.request_quit(window, cx);
            })
        });
        cx.run_until_parked();
        send
    }

    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing {selector}"));
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    }

    fn press_enter(cx: &mut VisualTestContext) {
        let keystroke = gpui::Keystroke::parse("enter").unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        cx.run_until_parked();
    }

    fn elapse(cx: &mut VisualTestContext, millis: u64) {
        cx.executor().advance_clock(Duration::from_millis(millis));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn quit_gate_deadlines_rebuild_one_dialog_and_late_success_exits(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "quit-deadlines");
        let completion = controlled_quit(&shell, cx);
        let attempt = cx.update(|_, cx| shell.read(cx).quit_attempt);
        elapse(cx, 299);
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
        elapse(cx, 1);
        assert!(cx.debug_bounds("quit-saving").is_some());
        elapse(cx, 2700);
        assert!(cx.debug_bounds("quit-slow").is_some());
        for _ in 0..3 {
            cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        }
        assert_eq!(cx.update(|_, cx| shell.read(cx).quit_attempt), attempt);
        elapse(cx, 7000);
        assert!(cx.debug_bounds("quit-timed-out").is_some());
        assert!(cx.debug_bounds("quit-keep-button").is_some());
        completion.send_blocking(Ok(())).unwrap();
        elapse(cx, 25);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }

    #[gpui::test]
    fn quit_gate_keep_open_revokes_old_completion_and_allows_a_fresh_attempt(
        cx: &mut TestAppContext,
    ) {
        let (shell, path, cx) = setup(cx, "quit-revoke");
        let completion = controlled_quit(&shell, cx);
        let old = cx.update(|_, cx| shell.read(cx).quit_attempt);
        elapse(cx, 10025);
        let keep = cx.debug_bounds("quit-keep-button").unwrap();
        cx.simulate_click(keep.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        completion.send_blocking(Ok(())).unwrap();
        elapse(cx, 100);
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                view.advance_saving(old, Duration::from_secs(20), window, cx)
            })
        });
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        let new = controlled_quit(&shell, cx);
        cx.update(|window, cx| shell.update(cx, |view, cx| view.keep_open(old, window, cx)));
        new.send_blocking(Ok(())).unwrap();
        elapse(cx, 25);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }

    #[gpui::test]
    fn quit_gate_failure_shows_choices_immediately_and_force_quit_skips_shutdown_save(
        cx: &mut TestAppContext,
    ) {
        let (shell, path, cx) = setup(cx, "quit-failure");
        let completion = controlled_quit(&shell, cx);
        completion.send_blocking(Err("disk failed".into())).unwrap();
        elapse(cx, 25);
        assert!(cx.debug_bounds("quit-failed").is_some());
        let quit = cx.debug_bounds("quit-anyway-button").unwrap();
        cx.simulate_click(quit.center(), gpui::Modifiers::default());
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }
    #[gpui::test]
    async fn quit_gate_blocks_silent_input_and_preserves_discarded_composer_on_keep_open(
        cx: &mut TestAppContext,
    ) {
        let (shell, path, cx) = setup(cx, "quit-input");
        let project = path.join("project");
        std::fs::create_dir_all(&project).unwrap();
        open(&shell, project, cx).await;
        cx.simulate_input("unfinished thought");
        let (completion, receive) = async_channel::unbounded();
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                view.quit_control = Some(receive);
                view.request_quit(window, cx);
            })
        });
        cx.run_until_parked();
        assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
        click(cx, "quit-keep-working");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        cx.run_until_parked();
        // ⌘Q again, with the question up, is the answer yes.
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        cx.run_until_parked();
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
        let draft = cx.update(|_, cx| shell.read(cx).projects[0].chat.read(cx).draft_for_test(cx));
        cx.simulate_input("must not be inserted");
        cx.simulate_keystrokes("cmd-enter cmd-n cmd-`");
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).projects[0].chat.read(cx).draft_for_test(cx)),
            draft
        );
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_blockers(cx).terminals),
            0
        );
        elapse(cx, 10000);
        elapse(cx, 25);
        assert!(
            cx.update(|window, cx| shell.read(cx).keep_open_focus.is_focused(window)),
            "keep-open owns focus"
        );
        press_enter(cx);
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).projects[0].chat.read(cx).draft_for_test(cx)),
            draft
        );
        completion.send_blocking(Ok(())).unwrap();
        elapse(cx, 25);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        finish(&shell, path, cx);
    }

    #[gpui::test]
    fn quit_gate_fast_success_never_opens_dialog(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "quit-fast");
        let completion = controlled_quit(&shell, cx);
        completion.send_blocking(Ok(())).unwrap();
        elapse(cx, 25);
        assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }

    #[gpui::test]
    fn quit_gate_warning_fits_small_window_and_failure_keep_open_can_retry(
        cx: &mut TestAppContext,
    ) {
        let (shell, path, cx) = setup(cx, "quit-small");
        cx.simulate_resize(gpui::size(px(640.), px(480.)));
        let completion = controlled_quit(&shell, cx);
        completion.send_blocking(Err("disk failed".into())).unwrap();
        elapse(cx, 25);
        for selector in ["quit-failed", "quit-keep-button", "quit-anyway-button"] {
            let bounds = cx.debug_bounds(selector).unwrap();
            assert!(
                bounds.origin.x >= px(0.) && bounds.right() <= px(640.),
                "{selector}: {bounds:?}"
            );
            assert!(
                bounds.origin.y >= px(0.) && bounds.bottom() <= px(480.),
                "{selector}: {bounds:?}"
            );
        }
        elapse(cx, 25);
        assert!(
            cx.update(|window, cx| shell.read(cx).keep_open_focus.is_focused(window)),
            "keep-open owns focus"
        );
        press_enter(cx);
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        let retry = controlled_quit(&shell, cx);
        retry.send_blocking(Ok(())).unwrap();
        elapse(cx, 25);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }
    #[gpui::test]
    fn quit_gate_enter_and_escape_cannot_dismiss_brief_or_slow_saving(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "quit-no-dismiss");
        let _completion = controlled_quit(&shell, cx);
        for millis in [300, 2700] {
            elapse(cx, millis);
            assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
            press_enter(cx);
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
            assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
            assert!(cx.update(|_, cx| shell.read(cx).quit_dialog));
            assert_eq!(
                cx.update(|_, cx| shell.read(cx).quit_phase),
                QuitPhase::Saving
            );
        }
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| view.keep_open(view.quit_attempt, window, cx))
        });
        finish(&shell, path, cx);
    }

    #[gpui::test]
    async fn quit_gate_waits_for_project_loading_then_covers_loader_writes(
        cx: &mut TestAppContext,
    ) {
        let (shell, path, cx) = setup(cx, "quit-loading");
        let project = path.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let (release, held) = async_channel::unbounded();
        cx.update(|window, cx| {
            shell.update(cx, |view, cx| {
                view.load_control = Some(held);
                view.open_project(project.clone(), window, cx);
            })
        });
        let completion = controlled_quit(&shell, cx);
        completion.send_blocking(Ok(())).unwrap();
        elapse(cx, 25);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Saving
        );
        assert!(cx.update(|_, cx| shell.read(cx).opening.is_some()));
        release.send_blocking(()).unwrap();
        cx.condition(&shell, |view, _| view.opening.is_none()).await;
        let final_save =
            cx.update(|_, cx| shell.update(cx, |view, _| view.quit_save.take().unwrap()));
        let saved = final_save.wait_blocking();
        assert_eq!(saved, Ok(()));
        assert!(cx.update(|_, cx| {
            shell
                .read(cx)
                .store
                .projects()
                .unwrap()
                .iter()
                .any(|saved| saved.path == project.canonicalize().unwrap())
        }));
        completion.send_blocking(saved).unwrap();
        elapse(cx, 25);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }
    #[gpui::test]
    fn quit_gate_requires_an_explicit_keyboard_choice_to_quit_anyway(cx: &mut TestAppContext) {
        let (shell, path, cx) = setup(cx, "quit-keyboard-choice");
        let completion = controlled_quit(&shell, cx);
        completion
            .send_blocking(Err("write failed".into()))
            .unwrap();
        elapse(cx, 50);
        assert!(cx.update(|window, cx| shell.read(cx).keep_open_focus.is_focused(window)));
        cx.simulate_keystrokes("shift-tab");
        press_enter(cx);
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        finish(&shell, path, cx);
    }
}
