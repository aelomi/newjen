use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext, Context, Entity, FocusHandle, IntoElement, ParentElement, PathPromptOptions,
    Render, Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_kit::base::v_flex;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, TitleBar};
use newjen_core::rift::Tools;
use newjen_core::{Project, SessionConfig, Store, Thread, thread};

use crate::attention::{NativeAttention, NativeQuitGuard};
use crate::chat_view::{Attention, ChatView, QuitBlockers, Startup};
use crate::format::tilde;
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
    /// The badge count last shown, so AppKit is only asked when it changes.
    badge: usize,
    attention_note: Option<&'static str>,
    /// The agent program every loaded project's threads start, for the tests.
    #[cfg(test)]
    stand_in: Option<PathBuf>,
    _errors: Task<()>,
    _focus: [Subscription; 2],
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
            badge: 0,
            attention_note: None,
            #[cfg(test)]
            stand_in: None,
            _errors: listener,
            _attention_tasks: Vec::new(),
            _focus: [
                cx.on_focus_lost(window, |view, window, cx| {
                    if view.active.is_none() {
                        cx.defer_in(window, |view, window, cx| {
                            if view.active.is_none() {
                                window.focus(&view.focus, cx);
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
        }
        view
    }

    pub fn open_project(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
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
        let loading = cx.background_spawn(async move {
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
        if let Some(active) = self.active
            && self.focus.is_focused(window)
        {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.take_focus(window, cx));
        }
    }

    fn show_projects(&mut self, _: &ShowProjects, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(active) = self.active.take() {
            self.projects[active]
                .chat
                .update(cx, |chat, cx| chat.set_visible(false, window, cx));
        }
        window.focus(&self.focus, cx);
        window.set_window_title("Newjen");
        cx.notify();
    }

    fn pick_project(&mut self, _: &OpenProject, window: &mut Window, cx: &mut Context<Self>) {
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

    pub fn quit(&mut self, cx: &mut Context<Self>) {
        let quitting: Vec<_> = self
            .projects
            .iter()
            .flat_map(|project| project.chat.update(cx, |chat, cx| chat.begin_quit(cx)))
            .collect();
        thread::finish_quit(&self.store, quitting);
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
        if self.quit_phase != QuitPhase::Running {
            return;
        }
        let blockers = self.quit_blockers(cx);
        if !blockers.any() {
            self.quit_phase = QuitPhase::Stopping;
            self.allow_quit();
            cx.quit();
            return;
        }
        self.quit_phase = QuitPhase::Confirming;
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            "Quit Newjen and stop its work?",
            Some(&blockers.detail()),
            &["Keep working", "Quit and stop"],
            cx,
        );
        cx.spawn(async move |view, cx| {
            let confirmed = answer.await == Ok(1);
            let _ = view.update(cx, |view, cx| {
                if confirmed {
                    view.quit_phase = QuitPhase::Stopping;
                    view.allow_quit();
                    cx.quit();
                } else {
                    view.quit_phase = QuitPhase::Running;
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn allow_quit(&self) {
        if let Some(guard) = &self.quit_guard {
            guard.allow_quit();
        }
    }

    fn welcome(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let open = ui::button_with_icon(
            "open-project",
            "Open Project…",
            ui::IconName::Folder,
            ui::Look::Primary,
            ui::Size::Md,
            cx,
        )
        .disabled(self.picking || self.opening.is_some())
        .on_click(cx.listener(|view, _, window, cx| view.pick_project(&OpenProject, window, cx)));
        let mut content = v_flex()
            .gap_4()
            .min_h_0()
            .max_h_full()
            .w_full()
            .max_w(px(560.))
            .child(div().text_lg().child("Open a project to get started"))
            .child(div().child(open));
        if self.recent.is_empty() {
            content = content.child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("Choose the folder where your agents will work."),
            );
        } else {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Recent projects"),
            );
            let rows = self.recent.iter().map(|project| {
                let path = project.path.clone();
                let label = tilde(&path, self.home.as_deref());
                let id = project.id;
                ui::button(
                    ("recent-project", id as u64),
                    label,
                    ui::Look::Ghost,
                    ui::Size::Md,
                    cx,
                )
                .w_full()
                .flex_none()
                .debug_selector(move || format!("recent-project-{id}"))
                .justify_start()
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.open_project(path.clone(), window, cx)
                }))
                .into_any_element()
            });
            content = content.child(
                v_flex()
                    .id("recent-projects")
                    .debug_selector(|| "recent-projects".to_string())
                    .min_h_0()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .children(rows),
            );
        }
        v_flex()
            .size_full()
            .child(TitleBar::new().child("Newjen"))
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

impl Render for ProjectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match self.active {
            Some(active) => self.projects[active].chat.clone().into_any_element(),
            None => self.welcome(cx),
        };
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::pick_project))
            .on_action(cx.listener(Self::show_projects))
            .on_action(cx.listener(Self::open_recent))
            .bg(cx.theme().background)
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
        cx.update(|_, cx| shell.update(cx, |view, cx| view.quit(cx)));
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
        // Nothing to lose: the app quits without a question.
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        assert!(!cx.has_pending_prompt());
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        cx.update(|_, cx| {
            shell.update(cx, |view, _| view.quit_phase = QuitPhase::Running);
        });
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
        assert!(cx.has_pending_prompt());
        // A second request while the question is up does not ask again.
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Confirming
        );
        cx.simulate_prompt_answer("Keep working");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Running
        );
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_blockers(cx).terminals),
            1
        );
        cx.update(|window, cx| shell.update(cx, |view, cx| view.request_quit(window, cx)));
        cx.simulate_prompt_answer("Quit and stop");
        cx.run_until_parked();
        assert_eq!(
            cx.update(|_, cx| shell.read(cx).quit_phase),
            QuitPhase::Stopping
        );
        // Quitting stops the terminal with everything else.
        cx.update(|_, cx| shell.update(cx, |view, cx| view.quit(cx)));
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
}
