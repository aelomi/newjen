//! Newjen: a macOS window for running Claude Code.
//!
//! This file only starts things: it cleans up `claude` processes a crashed Newjen left behind,
//! picks the project folder, starts GPUI with Ely's theme and fonts, and opens the window.
//! The window's content lives in `chat_view.rs`.
//!
//! Usage: `newjen [project-dir]` (or set `NEWJEN_PROJECT`). Defaults to the current directory.

mod approval;
mod chat_view;
mod format;
mod keys;
mod rows;

use std::path::{Path, PathBuf};

use ely_gpui_component::Assets;
use ely_gpui_component::forms::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use ely_gpui_component::theme::{ActiveTheme, Mode, Theme};
use gpui::{
    App, AppContext, Bounds, Menu, MenuItem, OsAction, QuitMode, SharedString, TitlebarOptions,
    WindowBounds, WindowOptions, px, size,
};
use newjen_core::orphans;

use crate::chat_view::ChatView;
use crate::keys::{CloseWindow, Hide, HideOthers, Minimize, Quit};

fn main() {
    // Kill `claude` processes whose Newjen crashed, before starting new ones.
    orphans::reap();

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let project = project_dir(home.as_deref());
    let project_label = SharedString::from(format::tilde(&project, home.as_deref()));

    gpui_platform::application()
        .with_assets(Assets)
        // Closing the window quits the app (on macOS GPUI otherwise keeps running).
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx: &mut App| {
            if let Err(error) = ely_gpui_component::init(cx) {
                eprintln!("newjen: Ely failed to start: {error:#}");
                cx.quit();
                return;
            }
            // Match the system's light/dark mode from the first frame (no flash).
            Theme::set_mode_now(Mode::from(cx.window_appearance()), cx);

            keys::bind(cx);
            install_menus(cx);

            if let Err(error) = open_window(project, project_label, cx) {
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
        // Ely's text field handles these actions; the OS variant lets macOS route them too.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Close", CloseWindow),
        ]),
    ]);
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

fn open_window(project: PathBuf, project_label: SharedString, cx: &mut App) -> anyhow::Result<()> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1000.0), px(720.0)),
            cx,
        ))),
        // A transparent title bar: Ely's TitleBar draws it, around the system traffic lights.
        titlebar: Some(TitlebarOptions {
            title: Some("Newjen".into()),
            appears_transparent: true,
            traffic_light_position: Some(cx.theme().traffic_light_origin()),
        }),
        window_min_size: Some(size(px(640.0), px(480.0))),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        // Follow the system when it switches between light and dark.
        window
            .observe_window_appearance(|window, cx| {
                Theme::set_mode(Mode::from(window.appearance()), cx);
            })
            .detach();
        let view = cx.new(|cx| ChatView::new(project, project_label, window, cx));
        let field = view.read(cx).field_focus(cx);
        window.focus(&field, cx);
        view
    })?;
    Ok(())
}
