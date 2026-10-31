use std::path::{Path, PathBuf};

use gpui::{
    AppContext, Context, Entity, FocusHandle, IntoElement, ParentElement, PathPromptOptions,
    Render, Styled, Subscription, Task, Window, div, prelude::*, px,
};
use gpui_kit::base::v_flex;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, TitleBar};
use newjen_core::rift::Tools;
use newjen_core::{Project, SessionConfig, Store, Thread, thread};

use crate::chat_view::{ChatView, Startup};
use crate::format::tilde;
use crate::keys::{OpenProject, OpenRecentProject, ShowProjects};
use crate::ui;

struct OpenedProject {
    project: Project,
    chat: Entity<ChatView>,
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
    _errors: Task<()>,
    _focus: Subscription,
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
            focus: cx.focus_handle(),
            _errors: listener,
            _focus: cx.on_focus_lost(window, |view, window, cx| {
                if view.active.is_none() {
                    cx.defer_in(window, |view, window, cx| {
                        if view.active.is_none() {
                            window.focus(&view.focus, cx);
                        }
                    });
                }
            }),
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
        let loading = cx.background_spawn(async move {
            let path = project_folder(&path)?;
            let project = store.open_project(&path)?;
            let loaded = match opened.iter().position(|opened| *opened == path) {
                Some(index) => LoadedProject::Cached(index),
                None => {
                    LoadedProject::New(Box::new(load_project(store.clone(), project, home, tools)?))
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
                                chat.update(cx, |chat, _| {
                                    chat.set_project_chooser(std::rc::Rc::new(
                                        move |window, cx| {
                                            let _ = shell.update(cx, |view, cx| {
                                                view.show_projects(&ShowProjects, window, cx);
                                            });
                                        },
                                    ));
                                });
                                view.projects.push(OpenedProject { project, chat });
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
        window.set_window_title(&format!(
            "{} | Newjen",
            self.projects[index].project.path.display()
        ));
        cx.notify();
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
                ui::button(
                    ("recent-project", project.id as u64),
                    label,
                    ui::Look::Ghost,
                    ui::Size::Md,
                    cx,
                )
                .w_full()
                .flex_none()
                .debug_selector(|| "recent-project-row".to_string())
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
        let row = cx.debug_bounds("recent-project-row").unwrap();
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
