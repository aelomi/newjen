//! Newjen: a macOS window for running Claude Code.
//!
//! Usage: `newjen [project-dir]` (or set `NEWJEN_PROJECT`). With neither, choose a project in the app.
//! Threads are saved in `~/Library/Application Support/Newjen` (or `$NEWJEN_HOME`).

mod approval;
mod assets;
mod chat_view;
mod format;
mod keys;
mod project_view;
mod reply;
mod rows;
mod sidebar;
mod theme;
mod title_bar;
mod ui;

use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext, Bounds, Context, IntoElement, Menu, MenuItem, OsAction, PromptLevel, QuitMode,
    Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use newjen_core::instance::InstanceLock;
use newjen_core::{Project, Store, orphans, store};

use crate::assets::AppAssets;
use crate::keys::{
    CloseWindow, Hide, HideOthers, JumpToThread, Minimize, NewThread, NextThread, OpenProject,
    OpenRecentProject, PreviousThread, Quit, ShowProjects, ToggleSidebar,
};
use crate::project_view::ProjectView;
use gpui_kit::component::ActiveTheme as _;

fn main() {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let project = project_dir();
    // One Newjen per data folder: a second one would drive the same threads and store. Only
    // the owner reaps crashed `claude` processes and opens the database. Without its database
    // Newjen cannot run; it still starts GPUI to say so in a window, because started from
    // Finder or the Dock there is no terminal to read the line.
    let (instance, startup) = match acquire_instance() {
        Ok(instance) => {
            orphans::reap();
            let startup =
                Store::open_in(instance.directory()).map_err(|problem| StartupProblem::Database {
                    problem,
                    folder: instance.directory().to_path_buf(),
                });
            (Some(instance), startup)
        }
        Err(problem) => (None, Err(problem)),
    };
    if let Err(problem) = &startup {
        eprintln!("newjen: {}: {}", problem.title(), problem.detail());
    }

    gpui_platform::application()
        .with_assets(AppAssets(gpui_kit::assets::Assets))
        // Closing the window quits the app (on macOS GPUI otherwise keeps running).
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            // Match the system's light/dark mode from the first frame (no flash).
            gpui_kit::component::Theme::change(cx.window_appearance(), None, cx);
            // And its Reduce Motion setting; the chat view's clock keeps it in step.
            gpui_kit::base::apply_system_reduce_motion(cx);

            keys::bind(cx);
            install_menus(cx);

            let opened = match startup {
                Ok(store) => open_window(store, home, project, cx),
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
    AlreadyRunning(PathBuf),
    Lock(anyhow::Error),
    Database {
        problem: anyhow::Error,
        folder: PathBuf,
    },
}

impl StartupProblem {
    fn title(&self) -> &'static str {
        match self {
            Self::AlreadyRunning(_) => "Newjen is already running",
            Self::Lock(_) => "Newjen couldn't lock its data folder",
            Self::Database { .. } => "Newjen couldn't open its thread database",
        }
    }

    fn detail(&self) -> String {
        match self {
            Self::AlreadyRunning(folder) => format!(
                "Newjen is already using {}. Switch to the running Newjen window and open the \
                 project there (File > Open Project). Set NEWJEN_HOME to use a separate data \
                 folder.",
                folder.display()
            ),
            Self::Lock(problem) => format!(
                "{problem:#}.\n\nCheck that the data folder can be written to, then try again. \
                 Set NEWJEN_HOME to use another folder."
            ),
            Self::Database { problem, folder } => {
                database_problem_detail(problem, &folder.display().to_string())
            }
        }
    }
}

fn acquire_instance() -> Result<InstanceLock, StartupProblem> {
    let folder = store::default_dir().map_err(StartupProblem::Lock)?;
    InstanceLock::try_acquire(&folder)
        .map_err(StartupProblem::Lock)?
        .ok_or(StartupProblem::AlreadyRunning(folder))
}

/// The menu bar and the app-wide actions behind it, so Newjen behaves like any Mac app
/// (⌘H, ⌘M, ⌘W, the Edit menu).
fn install_menus(cx: &mut App) {
    cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    // A key press is handled while GPUI is busy with the window, which can't be updated from
    // inside that, so these two act on it a moment later (`defer`).
    cx.on_action(|_: &Minimize, cx: &mut App| {
        if let Some(window) = cx.active_window() {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.minimize_window());
            });
        }
    });
    // Closing the only window quits (`QuitMode::LastWindowClosed`), which stops Claude cleanly.
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        if let Some(window) = cx.active_window() {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
        }
    });

    update_menus(&[], cx);
}

fn update_menus(recent: &[Project], cx: &mut App) {
    cx.set_menus([
        Menu::new("Newjen").items([
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
        Menu::new("View").items([MenuItem::action("Toggle Sidebar", ToggleSidebar)]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Close", CloseWindow),
        ]),
    ]);
}

fn project_dir() -> Option<PathBuf> {
    std::env::args_os().nth(1).map(PathBuf::from).or_else(|| {
        std::env::var_os("NEWJEN_PROJECT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    })
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

/// When Newjen cannot start (another Newjen owns the data folder, or the thread database
/// cannot be opened): a plain window with a native alert that says what went wrong, and quits
/// when it is dismissed.
fn show_startup_problem(problem: &StartupProblem, cx: &mut App) -> anyhow::Result<()> {
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
        window_min_size: Some(size(px(640.0), px(480.0))),
        ..Default::default()
    };
    let (_, view) = gpui_kit::open_window(options, cx, |window, cx| {
        // Follow the system when it switches between light and dark.
        window
            .observe_window_appearance(|window, cx| {
                gpui_kit::component::Theme::change(window.appearance(), Some(window), cx);
            })
            .detach();
        cx.new(|cx| ProjectView::new(store, home, project, window, cx))
    })?;
    // Quitting (⌘Q, or closing the window, which quits) settles and saves every thread and
    // stops every `claude`. GPUI allows about 200 ms for this. The hook keeps the view alive
    // itself: closing the window drops the window's hold on it before the app quits.
    cx.on_app_quit(move |cx| {
        view.update(cx, |view, cx| view.quit(cx));
        async {}
    })
    .detach();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
