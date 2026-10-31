//! Newjen: a macOS window for running Claude Code.
//!
//! This file only starts things: it cleans up `claude` processes a crashed Newjen left behind,
//! picks the project folder, opens the thread database and loads the project's threads, starts
//! GPUI with Kit's default theme and fonts, and opens the window. The window's content lives in
//! `chat_view.rs`.
//!
//! Usage: `newjen [project-dir]` (or set `NEWJEN_PROJECT`). Defaults to the current directory.
//! Threads are saved in `~/Library/Application Support/Newjen` (or `$NEWJEN_HOME`).

mod approval;
mod assets;
mod chat_view;
mod format;
mod keys;
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
use newjen_core::rift::Tools;
use newjen_core::{SessionConfig, Store, Thread, orphans, store};

use crate::assets::AppAssets;
use crate::chat_view::{ChatView, Startup};
use crate::keys::{
    CloseWindow, Hide, HideOthers, JumpToThread, Minimize, NewThread, NextThread, PreviousThread,
    Quit, ToggleSidebar,
};
use gpui_kit::component::ActiveTheme as _;

fn main() {
    // Kill `claude` processes whose Newjen crashed, before starting new ones.
    orphans::reap();

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let project = project_dir(home.as_deref());
    // Check before opening a window: a missing folder would otherwise surface later as a
    // confusing "could not start Claude" error.
    if let Err(problem) = check_project(&project) {
        eprintln!("newjen: {problem}");
        eprintln!("usage: newjen [project-dir]");
        std::process::exit(2);
    }
    // Without its database Newjen cannot run (there is nowhere to keep threads). It still
    // starts GPUI to say so in a window: started from Finder or the Dock there is no terminal
    // to read this line.
    let startup = open_threads(&project, home).inspect_err(|problem| {
        eprintln!("newjen: could not open the thread database: {problem:#}");
    });

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
                Ok(startup) => open_window(startup, cx),
                Err(problem) => show_database_problem(&problem, cx),
            };
            if let Err(error) = opened {
                eprintln!("newjen: could not open the window: {error:#}");
                cx.quit();
                return;
            }
            cx.activate(true);
        });
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

    cx.set_menus([
        Menu::new("Newjen").items([
            MenuItem::action("Hide Newjen", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::separator(),
            MenuItem::action("Quit Newjen", Quit),
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

/// Opens the thread database, records that `project` was opened, and loads its threads (newest
/// first, already settled after the last run: see `Thread::load`). Nothing is started: a
/// thread's `claude` starts with the first message sent to it.
///
/// A database that cannot be opened stops Newjen after saying why (`show_database_problem`);
/// threads that cannot be read are reported in the window and the project opens without them.
fn open_threads(project: &Path, home: Option<PathBuf>) -> anyhow::Result<Startup> {
    let store = Store::open()?;
    let project = store.open_project(project)?;
    let base_config = SessionConfig::new(project.path.clone());
    let (threads, problem) = match Thread::load_all(&store, &project, &base_config) {
        Ok(threads) => (threads, None),
        Err(error) => {
            eprintln!("newjen: could not load the saved threads: {error:#}");
            (
                Vec::new(),
                Some(format!("Couldn't load threads: {error:#}")),
            )
        }
    };
    Ok(Startup {
        store,
        project,
        base_config,
        threads,
        problem,
        home,
        tools: Tools::find(),
    })
}

/// The folder Claude works in: the first argument, else `$NEWJEN_PROJECT`, else the current
/// directory. Apps started from Finder get `/` as their current directory, which is never what
/// you want, so that becomes the home folder.
fn project_dir(home: Option<&Path>) -> PathBuf {
    let chosen = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("NEWJEN_PROJECT")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let chosen = match home {
        Some(home) if chosen == Path::new("/") => home.to_path_buf(),
        _ => chosen,
    };
    // An absolute, symlink-free path, so tool summaries can show paths relative to it.
    chosen.canonicalize().unwrap_or(chosen)
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

/// When the thread database cannot be opened: a plain window with a native alert that says what
/// went wrong and where the database is, and quits when it is dismissed.
fn show_database_problem(problem: &anyhow::Error, cx: &mut App) -> anyhow::Result<()> {
    let folder = store::default_dir()
        .map(|dir| dir.display().to_string())
        .unwrap_or_else(|_| "~/Library/Application Support/Newjen".to_string());
    let detail = database_problem_detail(problem, &folder);
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
            "Newjen couldn't open its thread database",
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

/// What the window shows behind the database alert: only the theme's background.
struct Blank;

impl Render for Blank {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(cx.theme().colors.background)
    }
}

fn open_window(startup: Startup, cx: &mut App) -> anyhow::Result<()> {
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
        let view = cx.new(|cx| ChatView::new(startup, window, cx));
        if let Some(field) = view.read(cx).field_focus(cx) {
            window.focus(&field, cx);
        }
        view
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
