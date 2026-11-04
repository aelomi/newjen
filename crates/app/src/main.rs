//! Newjen: a macOS window for running Claude Code.
//!
//! Usage: `newjen [project-dir]` (or set `NEWJEN_PROJECT`). With neither, choose a project in the app.
//! Threads are saved in `~/Library/Application Support/Newjen` (or `$NEWJEN_HOME`).

mod approval;
mod assets;
mod attention;
mod chat_view;
mod context_chip;
mod format;
mod keys;
mod project_view;
mod reply;
mod rows;
mod sidebar;
mod terminal;
mod theme;
mod title_bar;
mod ui;
mod update;
mod update_view;
mod usage_view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext, Bounds, Context, IntoElement, Menu, MenuItem, OsAction, PromptLevel, QuitMode,
    Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use newjen_core::instance::{HandoffError, Instance, LaunchRequest, PendingLaunch, Startup};
use newjen_core::{Project, Store, orphans, store, usage};

use crate::assets::AppAssets;
use crate::keys::{
    AboutNewjen, CheckForUpdates, CloseWindow, Hide, HideOthers, JumpToNeedsYou, JumpToThread,
    Minimize, NewThread, NextThread, OpenProject, OpenRecentProject, PreviousThread, Quit,
    ReleaseNotesOnGithub, Settings, ShowProjects, ToggleSidebar, ToggleTerminal, WhatsNewInNewjen,
};
use crate::project_view::ProjectView;
use gpui_kit::component::ActiveTheme as _;

fn main() {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let projects: std::io::Result<Vec<PathBuf>> = launch_projects()
        .into_iter()
        .map(std::path::absolute)
        .collect();
    let (project, more) = match &projects {
        Ok(projects) => match projects.split_first() {
            Some((first, rest)) => (Some(first.clone()), rest.to_vec()),
            None => (None, Vec::new()),
        },
        Err(_) => (None, Vec::new()),
    };
    let request = match &project {
        Some(path) => LaunchRequest::OpenProject(path.clone()),
        None => LaunchRequest::Activate,
    };
    let acquired = projects.map_err(anyhow::Error::from).and_then(|_| {
        let folder = store::default_dir()?;
        let started = Instance::start(&folder, &request)?;
        if matches!(started, Startup::Forwarded) {
            for path in &more {
                Instance::start(&folder, &LaunchRequest::OpenProject(path.clone()))?;
            }
        }
        Ok(started)
    });
    let (instance, startup) = match acquired {
        Ok(Startup::Forwarded) => return,
        Ok(Startup::Owner(instance)) => {
            orphans::reap();
            let startup =
                Store::open_in(instance.directory()).map_err(|problem| StartupProblem::Database {
                    problem,
                    folder: instance.directory().to_path_buf(),
                });
            if let Ok(store) = &startup {
                usage::attach_store(store);
            }
            (Some(Arc::new(instance)), startup)
        }
        Err(problem) if problem.is::<HandoffError>() => {
            eprintln!("newjen: {problem:#}");
            std::process::exit(1);
        }
        Err(problem) => (None, Err(StartupProblem::Ownership(problem))),
    };
    let project = match request {
        LaunchRequest::Activate => None,
        LaunchRequest::OpenProject(path) => Some(path),
    };
    if let Err(problem) = &startup {
        eprintln!("newjen: {}: {}", problem.title(), problem.detail());
    }

    let (launches, received) = async_channel::bounded(16);
    let application = gpui_platform::application()
        .with_assets(AppAssets(gpui_kit::assets::Assets))
        .with_quit_mode(QuitMode::LastWindowClosed);
    let opened_urls = launches.clone();
    application.on_open_urls(move |urls| {
        for url in urls {
            match project_url(&url) {
                Ok(path) => send_launch(&opened_urls, LaunchRequest::OpenProject(path)),
                Err(error) => eprintln!("newjen: could not open project URL: {error:#}"),
            }
        }
    });
    let ipc_launches = launches.clone();
    let queued_launches = launches.clone();
    application.on_reopen(move |_| send_launch(&launches, LaunchRequest::Activate));
    let running_instance = instance.clone();
    application.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        // Match the system's light/dark mode from the first frame (no flash).
        gpui_kit::component::Theme::change(cx.window_appearance(), None, cx);
        // The window's glass materials sit on Kit's theme; Reduce Transparency is read here too.
        theme::install(cx);
        // And its Reduce Motion setting; the chat view's clock keeps it in step.
        gpui_kit::base::apply_system_reduce_motion(cx);

        keys::bind(cx);
        install_menus(cx);

        let opened = match startup {
            Ok(store) => {
                update::install(&store, cx);
                for path in more {
                    send_launch(&queued_launches, LaunchRequest::OpenProject(path));
                }
                open_window(
                    store,
                    home,
                    project,
                    received,
                    ipc_launches,
                    running_instance.unwrap(),
                    cx,
                )
            }
            Err(problem) => show_startup_problem(&problem, cx),
        };
        if let Err(error) = opened {
            eprintln!("newjen: could not open the window: {error:#}");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
    drop(instance);
}

enum StartupProblem {
    Ownership(anyhow::Error),
    Database {
        problem: anyhow::Error,
        folder: PathBuf,
    },
}

impl StartupProblem {
    fn title(&self) -> &'static str {
        match self {
            Self::Ownership(_) => "Newjen couldn't open its data folder",
            Self::Database { .. } => "Newjen couldn't open its thread database",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::Ownership(problem) => format!(
                "{problem:#}.\n\nCheck that the project and data folders are valid and writable, then try again. Set NEWJEN_HOME to use another data folder."
            ),
            Self::Database { problem, folder } => {
                database_problem_detail(problem, &folder.display().to_string())
            }
        }
    }
}

enum LaunchEvent {
    Local(LaunchRequest),
    Forwarded(PendingLaunch),
}

fn send_launch(sender: &async_channel::Sender<LaunchEvent>, request: LaunchRequest) {
    if let Err(error) = sender.try_send(LaunchEvent::Local(request)) {
        eprintln!("newjen: could not accept the launch: {error}");
    }
}

fn project_url(value: &str) -> anyhow::Result<PathBuf> {
    let url = url::Url::parse(value)?;
    anyhow::ensure!(
        url.scheme() == "file",
        "only folder file URLs are supported"
    );
    anyhow::ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "folder URL has a query or fragment"
    );
    let path = url
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("invalid local folder URL"))?;
    anyhow::ensure!(path.is_absolute(), "folder URL must be absolute");
    anyhow::ensure!(
        !path.as_os_str().as_encoded_bytes().contains(&0),
        "folder URL contains a null byte"
    );
    Ok(path)
}

/// The menu bar and the app-wide actions behind it, so Newjen behaves like any Mac app
/// (⌘H, ⌘M, ⌘W, the Edit menu). ⌘Q and ⌘W are bound where the window is opened: they ask
/// before stopping work (`ProjectView::request_quit`).
fn install_menus(cx: &mut App) {
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &Settings, _: &mut App| {});
    // A key press is handled while GPUI is busy with the window, which can't be updated from
    // inside that, so these two act on it a moment later (`defer`).
    cx.on_action(|_: &Minimize, cx: &mut App| {
        if let Some(window) = cx.active_window() {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.minimize_window());
            });
        }
    });

    update_menus(&[], cx);
}

fn update_menus(recent: &[Project], cx: &mut App) {
    cx.set_menus([
        Menu::new("Newjen").items([
            MenuItem::action("About Newjen", AboutNewjen),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::action("Settings…", Settings),
            MenuItem::separator(),
            MenuItem::action("Hide Newjen", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::separator(),
            MenuItem::action("Quit Newjen", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Open Project…", OpenProject),
            MenuItem::action("Projects…", ShowProjects),
            MenuItem::submenu(Menu::new("Open Recent").items(recent.iter().map(|project| {
                MenuItem::action(
                    project.path.display().to_string(),
                    OpenRecentProject(project.path.clone()),
                )
            }))),
        ]),
        // The chat view handles these.
        Menu::new("Thread").items([
            MenuItem::action("New Thread", NewThread),
            MenuItem::separator(),
            MenuItem::action("Previous Thread", PreviousThread),
            MenuItem::action("Next Thread", NextThread),
            MenuItem::action("Next Thread Needing You", JumpToNeedsYou),
            // ⌘1 to ⌘9, by place in the list. macOS shows each item's key from its binding.
            MenuItem::submenu(
                Menu::new("Go to Thread").items((0..keys::JUMP_KEYS.len()).map(|place| {
                    MenuItem::action(format!("Thread {}", place + 1), JumpToThread(place))
                })),
            ),
        ]),
        // The Kit text field handles these actions; the OS variant lets macOS route them too.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Toggle Terminal", ToggleTerminal),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Close", CloseWindow),
        ]),
        Menu::new("Help").items([
            MenuItem::action("What's New in Newjen", WhatsNewInNewjen),
            MenuItem::action("Release Notes on GitHub", ReleaseNotesOnGithub),
        ]),
    ]);
}

/// The folders to open: every argument (an update relaunches with the projects that were
/// open, the one to show last), else `NEWJEN_PROJECT`. macOS's `-psn_` argument is no folder.
fn launch_projects() -> Vec<PathBuf> {
    let from_arguments: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .filter(|argument| !argument.as_encoded_bytes().starts_with(b"-psn_"))
        .map(PathBuf::from)
        .collect();
    if !from_arguments.is_empty() {
        return from_arguments;
    }
    std::env::var_os("NEWJEN_PROJECT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect()
}

/// The project must be an existing folder, since Claude runs inside it.
fn check_project(project: &Path) -> Result<(), String> {
    if project.is_dir() {
        Ok(())
    } else if project.exists() {
        Err(format!("{} is not a folder.", project.display()))
    } else {
        Err(format!("{} does not exist.", project.display()))
    }
}

fn show_startup_problem(problem: &StartupProblem, cx: &mut App) -> anyhow::Result<()> {
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.on_action(|_: &CloseWindow, cx: &mut App| cx.quit());
    let detail = problem.detail();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(480.0), px(320.0)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Newjen".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let window = cx.open_window(options, |_, cx| cx.new(|_| Blank))?;
    window.update(cx, |_, window, cx| {
        let answer = window.prompt(
            PromptLevel::Critical,
            problem.title(),
            Some(&detail),
            &["Quit"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            let _ = answer.await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    })?;
    Ok(())
}

/// The alert's text under its title when the thread database cannot be opened.
fn database_problem_detail(problem: &anyhow::Error, folder: &str) -> String {
    format!(
        "{problem:#}.\n\nNewjen keeps its threads in newjen.db in {folder}. Check that the folder \
         can be written to. If the file is damaged, moving it aside lets Newjen start with an \
         empty list. Set NEWJEN_HOME to use another folder."
    )
}

/// What the window shows behind the startup alert: only the theme's background.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().colors.background)
    }
}

fn open_window(
    store: Store,
    home: Option<PathBuf>,
    project: Option<PathBuf>,
    received: async_channel::Receiver<LaunchEvent>,
    forwarded: async_channel::Sender<LaunchEvent>,
    instance: Arc<Instance>,
    cx: &mut App,
) -> anyhow::Result<()> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1200.0), px(760.0)),
            cx,
        ))),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("Newjen".into()),
            ..gpui_kit::component::TitleBar::title_bar_options()
        }),
        app_owns_titlebar_drag: true,
        window_background: theme::window_background(cx),
        window_min_size: Some(size(px(640.0), px(480.0))),
        ..Default::default()
    };
    let (window_handle, view) = gpui_kit::open_window(options, cx, |window, cx| {
        // Follow the system when it switches between light and dark.
        window
            .observe_window_appearance(|window, cx| {
                gpui_kit::component::Theme::change(window.appearance(), Some(window), cx);
                theme::apply(cx);
            })
            .detach();
        let view = cx.new(|cx| ProjectView::new(store, home, project, window, cx));
        // The Dock badge, notifications and the native quit hook, once for the app's life.
        view.update(cx, |view, cx| view.install_attention(window, cx));
        let closing = view.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            let _ = closing.update(cx, |view, cx| view.ask_to_quit(window, cx));
            false
        });
        view
    })?;
    // Reduce Transparency has no change notice, so it is re-read like Reduce Motion.
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let followed = window_handle.update(cx, |_, window, cx| {
                theme::follow_reduce_transparency(window, cx)
            });
            if followed.is_err() {
                break;
            }
        }
    })
    .detach();
    let target = view.downgrade();
    cx.spawn(async move |cx| {
        loop {
            let owner = instance.clone();
            match cx
                .background_executor()
                .spawn(async move { owner.receive() })
                .await
            {
                Ok(Some(request)) => match forwarded.try_send(LaunchEvent::Forwarded(request)) {
                    Ok(()) | Err(async_channel::TrySendError::Full(_)) => {}
                    Err(async_channel::TrySendError::Closed(_)) => break,
                },
                Ok(None) => {}
                Err(error) => eprintln!("newjen: could not receive launch: {error:#}"),
            }
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Ok(event) = received.recv().await {
            let request = match &event {
                LaunchEvent::Local(request) => request.clone(),
                LaunchEvent::Forwarded(pending) if !pending.is_expired() => {
                    pending.request().clone()
                }
                LaunchEvent::Forwarded(_) => continue,
            };
            let accepted = window_handle.update(cx, |_, window, cx| {
                target.update(cx, |view, cx| {
                    if view.is_stopping() {
                        return false;
                    }
                    match request {
                        LaunchRequest::Activate => {}
                        LaunchRequest::OpenProject(path) => view.open_project(path, window, cx),
                    }
                    window.activate_window();
                    cx.activate(true);
                    true
                })
            });
            match accepted {
                Ok(Ok(true)) => {
                    if let LaunchEvent::Forwarded(pending) = event {
                        let _ = cx
                            .background_executor()
                            .spawn(async move { pending.accept() })
                            .await;
                    }
                }
                Ok(Ok(false)) => {}
                _ => break,
            }
        }
    })
    .detach();
    // Quit targets this window even when minimized, and defers past the key event's update.
    let asking = view.downgrade();
    let request = move |cx: &mut App, confirms: bool| {
        let view = asking.clone();
        cx.defer(move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = view.update(cx, |view, cx| {
                    if confirms {
                        view.request_quit(window, cx)
                    } else {
                        view.ask_to_quit(window, cx)
                    }
                });
            });
        });
    };
    let quit = request.clone();
    // Only a second ⌘Q answers the question yes; ⌘W just keeps it up.
    cx.on_action(move |_: &Quit, cx| quit(cx, true));
    cx.on_action(move |_: &CloseWindow, cx| request(cx, false));
    // Quitting settles and saves every thread and stops every agent and terminal. GPUI runs
    // this hook to completion; its 200 ms limit applies only to the returned future. The hook
    // keeps the view alive itself: closing the window drops the window's hold on it before the
    // app quits.
    cx.on_app_quit(move |cx| {
        if let Err(error) = view.update(cx, |view, cx| view.quit(cx)) {
            eprintln!("newjen: warning: {error}");
        }
        async {}
    })
    .detach();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_urls_decode_spaces_unicode_and_reject_unsupported_urls() {
        assert_eq!(
            project_url("file:///tmp/project%20with%20spaces").unwrap(),
            PathBuf::from("/tmp/project with spaces")
        );
        assert_eq!(
            project_url("file:///tmp/%E6%97%A5%E6%9C%AC%E8%AA%9E").unwrap(),
            PathBuf::from("/tmp/日本語")
        );
        for value in [
            "https://example.com/project",
            "newjen://project",
            "file://remote/tmp/project",
            "file:///tmp/project?other=1",
            "file:///tmp/%00",
        ] {
            assert!(project_url(value).is_err(), "accepted {value}");
        }
    }

    #[test]
    fn a_database_problem_says_what_and_where() {
        let problem = anyhow::anyhow!("file is not a database").context("could not open newjen.db");
        let detail = database_problem_detail(
            &problem,
            "/Users/someone/Library/Application Support/Newjen",
        );
        assert!(
            detail.starts_with("could not open newjen.db: file is not a database."),
            "{detail}"
        );
        assert!(detail.contains("in /Users/someone/Library/Application Support/Newjen."));
        assert!(detail.contains("NEWJEN_HOME"));
    }

    #[test]
    fn check_project_accepts_only_existing_folders() {
        let dir = std::env::temp_dir();
        assert_eq!(check_project(&dir), Ok(()));

        let missing = dir.join("newjen-no-such-folder-for-tests");
        let error = check_project(&missing).unwrap_err();
        assert!(error.ends_with("does not exist."), "{error}");

        let file = dir.join(format!("newjen-check-project-{}.txt", std::process::id()));
        std::fs::write(&file, "not a folder").unwrap();
        let error = check_project(&file).unwrap_err();
        std::fs::remove_file(&file).unwrap();
        assert!(error.ends_with("is not a folder."), "{error}");
    }
}
