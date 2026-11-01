use std::io::{Read, Write};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point as TermPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, Term, TermMode, cell::Flags},
    vte::ansi::{Color, CursorShape, NamedColor, Processor},
};
use anyhow::{Context as _, Result};
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, FontStyle, FontWeight, Hsla,
    InteractiveElement, IntoElement, KeyDownEvent, Keystroke, MouseButton, ParentElement, Pixels,
    Point, Render, SharedString, Styled, Task, TextAlign, TextRun, Window, canvas, div, fill, font,
    point, px, rgb, size,
};
use gpui_kit::component::{
    ActiveTheme as _,
    input::{Copy, Paste},
};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

const INITIAL_GRID: GridSize = GridSize { cols: 80, rows: 16 };

#[derive(Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: u16,
    rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows.into()
    }
    fn screen_lines(&self) -> usize {
        self.rows.into()
    }
    fn columns(&self) -> usize {
        self.cols.into()
    }
}

impl From<GridSize> for PtySize {
    fn from(value: GridSize) -> Self {
        Self {
            rows: value.rows,
            cols: value.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

#[derive(Clone)]
struct Listener(mpsc::Sender<Input>);

enum Input {
    Write(Vec<u8>),
    Close,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            let _ = self.0.send(Input::Write(text.into_bytes()));
        }
    }
}

struct PtySession {
    master: Option<Box<dyn MasterPty + Send>>,
    input: Option<mpsc::Sender<Input>>,
    term: Arc<Mutex<Term<Listener>>>,
    alive: Arc<AtomicBool>,
    #[cfg(test)]
    output_open: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    problem: Arc<Mutex<Option<String>>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pid: i32,
    identity: Option<(u64, u64)>,
    size: GridSize,
}

impl PtySession {
    fn open(
        cwd: &Path,
        shell: &Path,
        args: &[&str],
    ) -> Result<(Self, async_channel::Receiver<()>)> {
        anyhow::ensure!(cwd.is_dir(), "The terminal folder no longer exists.");
        let pair = native_pty_system().openpty(INITIAL_GRID.into())?;
        let mut command = CommandBuilder::new(shell);
        command.args(args);
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // The login shell's PATH, as sessions and Rift hooks get it: an app started from
        // Finder inherits a minimal one.
        command.env("PATH", newjen_core::resolved_path());
        for name in newjen_core::env::SESSION_ENV {
            command.env_remove(name);
        }
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .context("Could not start the shell")?;
        drop(pair.slave);
        let pid = child
            .process_id()
            .and_then(|pid| i32::try_from(pid).ok())
            .unwrap_or(0);
        let identity = process_identity(pid);
        let killer = child.clone_killer();
        let alive = Arc::new(AtomicBool::new(true));
        let output_open = Arc::new(AtomicBool::new(true));
        let failed = Arc::new(AtomicBool::new(false));
        let problem = Arc::new(Mutex::new(None));
        let (input, commands) = mpsc::channel::<Input>();
        let term = Arc::new(Mutex::new(Term::new(
            Config::default(),
            &INITIAL_GRID,
            Listener(input.clone()),
        )));
        let (wake, updates) = async_channel::bounded(1);

        let writer_problem = problem.clone();
        let writer_failed = failed.clone();
        let writer_wake = wake.clone();
        std::thread::spawn(move || {
            while let Ok(Input::Write(bytes)) = commands.recv() {
                if let Err(error) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                    writer_failed.store(true, Ordering::Release);
                    kill_session(pid, identity);
                    *writer_problem.lock().unwrap() =
                        Some(format!("Terminal input failed: {error}"));
                    let _ = writer_wake.try_send(());
                    break;
                }
            }
        });
        let reader_term = term.clone();
        let reader_problem = problem.clone();
        let reader_failed = failed.clone();
        let reader_open = output_open.clone();
        let reader_wake = wake.clone();
        std::thread::spawn(move || {
            let mut parser: Processor = Processor::new();
            let mut bytes = [0; 16384];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        parser.advance(&mut *reader_term.lock().unwrap(), &bytes[..count]);
                        let _ = reader_wake.try_send(());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        if error.raw_os_error() != Some(libc::EIO) {
                            reader_failed.store(true, Ordering::Release);
                            kill_session(pid, identity);
                            *reader_problem.lock().unwrap() =
                                Some(format!("Terminal output failed: {error}"));
                            let _ = reader_wake.try_send(());
                        }
                        break;
                    }
                }
            }
            reader_open.store(false, Ordering::Release);
            let _ = reader_wake.try_send(());
        });
        let waiter_alive = alive.clone();
        let waiter_input = input.clone();
        let waiter_problem = problem.clone();
        std::thread::spawn(move || {
            let result = child.wait();
            waiter_alive.store(false, Ordering::Release);
            let _ = waiter_input.send(Input::Close);
            waiter_problem
                .lock()
                .unwrap()
                .get_or_insert_with(|| match result {
                    Ok(status) => format!(
                        "Shell exited ({status}). Close and reopen the terminal to start another."
                    ),
                    Err(error) => format!("Could not read the shell's exit status: {error}"),
                });
            let _ = wake.try_send(());
        });
        Ok((
            Self {
                master: Some(pair.master),
                input: Some(input),
                term,
                alive,
                #[cfg(test)]
                output_open,
                failed,
                problem,
                killer,
                pid,
                identity,
                size: INITIAL_GRID,
            },
            updates,
        ))
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(input) = &self.input {
            let _ = input.send(Input::Write(bytes));
        }
    }

    fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        if let Some(master) = &self.master
            && let Err(error) = master.resize(size.into())
        {
            *self.problem.lock().unwrap() = Some(format!("Terminal resize failed: {error}"));
            return;
        }
        self.term.lock().unwrap().resize(size);
        self.size = size;
    }

    fn shutdown(&mut self) {
        if let Some(_master) = self.master.take() {
            // Job control gives background jobs their own process groups within this session.
            kill_session(self.pid, self.identity);
            if self.alive.swap(false, Ordering::AcqRel)
                && self.identity.is_some()
                && process_identity(self.pid) == self.identity
            {
                let _ = self.killer.kill();
            }
        }
        if let Some(input) = self.input.take() {
            let _ = input.send(Input::Close);
        }
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn process_identity(pid: i32) -> Option<(u64, u64)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let length = size_of::<libc::proc_bsdinfo>() as i32;
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            length,
        )
    };
    if read != length {
        return None;
    }
    let info = unsafe { info.assume_init() };
    Some((info.pbi_start_tvsec, info.pbi_start_tvusec))
}

fn kill_session(session: i32, identity: Option<(u64, u64)>) {
    if session <= 1 {
        return;
    }
    if let Some(current) = process_identity(session)
        && Some(current) != identity
    {
        return;
    }
    unsafe {
        let count = libc::proc_listallpids(std::ptr::null_mut(), 0);
        if count <= 0 {
            return;
        }
        let mut pids = vec![0_i32; count as usize + 512];
        let capacity = i32::try_from(pids.len() * size_of::<i32>()).unwrap_or(i32::MAX);
        let count = libc::proc_listallpids(pids.as_mut_ptr().cast(), capacity);
        for pid in pids.into_iter().take(count.max(0) as usize) {
            if pid > 1 && libc::getsid(pid) == session {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}

/// The shell the terminal runs: the user's, or zsh.
fn login_shell() -> std::path::PathBuf {
    std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .unwrap_or_else(|| "/bin/zsh".into())
}

/// The shell's name for the panel's tab ("zsh").
pub fn shell_name() -> String {
    login_shell().file_name().map_or_else(
        || "shell".into(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub struct TerminalPanel {
    session: Option<PtySession>,
    error: Option<String>,
    focus: FocusHandle,
    bounds: Bounds<Pixels>,
    cell: gpui::Size<Pixels>,
    selecting: bool,
    scroll_remainder: f32,
    _updates: Option<Task<()>>,
}

impl TerminalPanel {
    pub fn new(cwd: &Path, cx: &mut Context<Self>) -> Self {
        let shell = login_shell();
        let (session, error, updates) = match PtySession::open(cwd, &shell, &["-l", "-i"]) {
            Ok((session, updates)) => (Some(session), None, Some(updates)),
            Err(error) => (
                None,
                Some(format!("Couldn't open the terminal: {error:#}")),
                None,
            ),
        };
        let task = updates.map(|updates| {
            cx.spawn(async move |view, cx| {
                while updates.recv().await.is_ok() {
                    if view
                        .update(cx, |view, cx| {
                            if let Some(session) = &mut view.session
                                && session.failed.load(Ordering::Acquire)
                            {
                                session.shutdown();
                            }
                            cx.notify();
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
        });
        Self {
            session,
            error,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            cell: size(px(8.), px(20.)),
            selecting: false,
            scroll_remainder: 0.,
            _updates: task,
        }
    }

    pub fn has_session(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.master.is_some())
    }

    pub fn shutdown(&mut self) {
        if let Some(session) = &mut self.session {
            session.shutdown();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        if event.keystroke.modifiers.platform {
            match event.keystroke.key.as_str() {
                "v" => self.paste(&Paste, window, cx),
                "c" => self.copy(&Copy, window, cx),
                _ => return,
            }
        } else {
            let mode = *session.term.lock().unwrap().mode();
            if let Some(bytes) = key_bytes(&event.keystroke, mode) {
                session.term.lock().unwrap().scroll_display(Scroll::Bottom);
                session.write(bytes);
            } else {
                return;
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = &self.session
            && let Some(text) = cx.read_from_clipboard().and_then(|item| item.text())
        {
            let bracketed = session
                .term
                .lock()
                .unwrap()
                .mode()
                .contains(TermMode::BRACKETED_PASTE);
            let text = text.replace('\u{1b}', "").replace('\n', "\r");
            session.write(if bracketed {
                format!("\u{1b}[200~{text}\u{1b}[201~").into_bytes()
            } else {
                text.into_bytes()
            });
        }
        cx.stop_propagation();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self
            .session
            .as_ref()
            .and_then(|session| session.term.lock().unwrap().selection_to_string())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        cx.stop_propagation();
    }

    fn at(&self, position: Point<Pixels>, term: &Term<Listener>) -> TermPoint {
        let col = (f32::from(position.x - self.bounds.origin.x) / f32::from(self.cell.width))
            .floor()
            .max(0.) as usize;
        let row = (f32::from(position.y - self.bounds.origin.y) / f32::from(self.cell.height))
            .floor()
            .max(0.) as usize;
        TermPoint::new(
            Line(row.min(term.screen_lines() - 1) as i32 - term.grid().display_offset() as i32),
            Column(col.min(term.columns() - 1)),
        )
    }
}

impl Focusable for TerminalPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn key_bytes(key: &Keystroke, mode: TermMode) -> Option<Vec<u8>> {
    if key.modifiers.platform {
        return None;
    }
    let app = mode.contains(TermMode::APP_CURSOR);
    let text = match key.key.as_str() {
        "enter" => "\r".to_string(),
        "tab" if key.modifiers.shift => "\u{1b}[Z".to_string(),
        "tab" => "\t".to_string(),
        "backspace" if key.modifiers.alt => "\u{1b}\u{7f}".to_string(),
        "backspace" => "\u{7f}".to_string(),
        "escape" => "\u{1b}".to_string(),
        "up" | "down" | "right" | "left" | "home" | "end" => {
            let suffix = match key.key.as_str() {
                "up" => 'A',
                "down" => 'B',
                "right" => 'C',
                "left" => 'D',
                "home" => 'H',
                _ => 'F',
            };
            let modifier = 1
                + u8::from(key.modifiers.shift)
                + 2 * u8::from(key.modifiers.alt)
                + 4 * u8::from(key.modifiers.control);
            if modifier > 1 {
                format!("\u{1b}[1;{modifier}{suffix}")
            } else {
                format!("\u{1b}{}{suffix}", if app { 'O' } else { '[' })
            }
        }
        "f1" => "\u{1b}OP".to_string(),
        "f2" => "\u{1b}OQ".to_string(),
        "f3" => "\u{1b}OR".to_string(),
        "f4" => "\u{1b}OS".to_string(),
        "f5" => "\u{1b}[15~".to_string(),
        "f6" => "\u{1b}[17~".to_string(),
        "f7" => "\u{1b}[18~".to_string(),
        "f8" => "\u{1b}[19~".to_string(),
        "f9" => "\u{1b}[20~".to_string(),
        "f10" => "\u{1b}[21~".to_string(),
        "f11" => "\u{1b}[23~".to_string(),
        "f12" => "\u{1b}[24~".to_string(),
        "insert" => "\u{1b}[2~".to_string(),
        "space" if key.modifiers.control => "\0".to_string(),
        "delete" => "\u{1b}[3~".to_string(),
        "pageup" => "\u{1b}[5~".to_string(),
        "pagedown" => "\u{1b}[6~".to_string(),
        _ if key.modifiers.control => {
            let c = key.key.chars().next()?;
            let c = match c {
                'a'..='z' => (c as u8 - b'a' + 1) as char,
                'A'..='Z' => (c as u8 - b'A' + 1) as char,
                ' ' | '@' => '\0',
                '[' => '\u{1b}',
                '\\' => '\u{1c}',
                ']' => '\u{1d}',
                '^' => '\u{1e}',
                '_' => '\u{1f}',
                _ => return None,
            };
            c.to_string()
        }
        _ => {
            let text = if key.modifiers.alt {
                &key.key
            } else {
                key.key_char.as_ref()?
            };
            if text.chars().any(char::is_control) {
                return None;
            }
            if key.modifiers.alt {
                format!("\u{1b}{text}")
            } else {
                text.clone()
            }
        }
    };
    Some(text.into_bytes())
}

impl Render for TerminalPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let default_fg = theme.foreground;
        let default_bg = theme.background;
        let selection_bg = theme.selection;
        let mono = font(theme.mono_font_family.clone());
        let font_size = theme.mono_font_size;
        let error = self.error.clone().or_else(|| {
            self.session
                .as_ref()
                .and_then(|session| session.problem.lock().unwrap().clone())
        });
        let view = cx.entity();
        div()
            .id("terminal-panel")
            .key_context("TerminalPanel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .bg(default_bg)
            .text_color(default_fg)
            .on_key_down(cx.listener(Self::key))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &gpui::MouseDownEvent, window, cx| {
                    view.focus.focus(window, cx);
                    if let Some(session) = &view.session {
                        let mut term = session.term.lock().unwrap();
                        let p = view.at(event.position, &term);
                        term.selection = Some(Selection::new(SelectionType::Simple, p, Side::Left));
                        view.selecting = true;
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|view, event: &gpui::MouseMoveEvent, _, cx| {
                if view.selecting && event.pressed_button == Some(MouseButton::Left) {
                    if let Some(session) = &view.session {
                        let mut term = session.term.lock().unwrap();
                        let p = view.at(event.position, &term);
                        if let Some(selection) = &mut term.selection {
                            selection.update(p, Side::Right);
                        }
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.selecting = false),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, _| view.selecting = false),
            )
            .on_scroll(cx.listener(|view, event: &gpui::ScrollEvent, _, cx| {
                if let Some(session) = &view.session {
                    let delta = match event.delta {
                        gpui::ScrollDelta::Lines(p) => p.y,
                        gpui::ScrollDelta::Pixels(p) => {
                            f32::from(p.y) / f32::from(view.cell.height)
                        }
                    };
                    view.scroll_remainder += delta;
                    let lines = view.scroll_remainder.trunc() as i32;
                    view.scroll_remainder -= lines as f32;
                    session
                        .term
                        .lock()
                        .unwrap()
                        .scroll_display(Scroll::Delta(lines));
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .children(error.map(|error| div().px_2().py_1().text_sm().child(error)))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        view.update(cx, |view, _| {
                            let run = TextRun {
                                len: 1,
                                font: mono.clone(),
                                color: default_fg,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let cell_width = window
                                .text_system()
                                .shape_line("M".into(), font_size, &[run], None)
                                .width
                                .max(px(1.));
                            let cell = size(cell_width, (font_size * 1.5).ceil());
                            view.bounds = bounds;
                            view.cell = cell;
                            if let Some(session) = &mut view.session {
                                session.resize(GridSize {
                                    cols: (f32::from(bounds.size.width) / f32::from(cell.width))
                                        .floor()
                                        .clamp(2., 500.)
                                        as u16,
                                    rows: (f32::from(bounds.size.height) / f32::from(cell.height))
                                        .floor()
                                        .clamp(1., 200.)
                                        as u16,
                                });
                            }
                            (
                                view.session.as_ref().map(|session| session.term.clone()),
                                cell,
                                mono,
                            )
                        })
                    },
                    move |bounds, (term, cell_size, mono), window, cx| {
                        let Some(term) = term else {
                            return;
                        };
                        let term = term.lock().unwrap();
                        let content = term.renderable_content();
                        for indexed in content.display_iter {
                            let row = indexed.point.line.0 + content.display_offset as i32;
                            if row < 0 {
                                continue;
                            }
                            let cell = indexed.cell;
                            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                                continue;
                            }
                            let mut fg = color(cell.fg, content.colors, default_fg, default_bg);
                            let mut bg = color(cell.bg, content.colors, default_fg, default_bg);
                            if cell.flags.contains(Flags::INVERSE) {
                                std::mem::swap(&mut fg, &mut bg);
                            }
                            if content
                                .selection
                                .as_ref()
                                .is_some_and(|selection| selection.contains(indexed.point))
                            {
                                bg = selection_bg;
                            }
                            let origin = point(
                                bounds.origin.x + cell_size.width * indexed.point.column.0 as f32,
                                bounds.origin.y + cell_size.height * row as f32,
                            );
                            let width = if cell.flags.contains(Flags::WIDE_CHAR) {
                                cell_size.width * 2.
                            } else {
                                cell_size.width
                            };
                            if bg != default_bg {
                                window.paint_quad(fill(
                                    Bounds::new(origin, size(width, cell_size.height)),
                                    bg,
                                ));
                            }
                            if cell.c == ' ' || cell.flags.contains(Flags::HIDDEN) {
                                continue;
                            }
                            let mut text = cell.c.to_string();
                            if let Some(extra) = cell.zerowidth() {
                                text.extend(extra);
                            }
                            let mut face = mono.clone();
                            if cell.flags.contains(Flags::BOLD) {
                                face.weight = FontWeight::BOLD;
                            }
                            if cell.flags.contains(Flags::ITALIC) {
                                face.style = FontStyle::Italic;
                            }
                            if cell.flags.contains(Flags::DIM) {
                                fg = fg.opacity(0.7);
                            }
                            let run = TextRun {
                                len: text.len(),
                                font: face,
                                color: fg,
                                background_color: None,
                                underline: cell.flags.intersects(Flags::ALL_UNDERLINES).then_some(
                                    gpui::UnderlineStyle {
                                        thickness: px(1.),
                                        color: Some(fg),
                                        wavy: cell.flags.contains(Flags::UNDERCURL),
                                    },
                                ),
                                strikethrough: cell.flags.contains(Flags::STRIKEOUT).then_some(
                                    gpui::StrikethroughStyle {
                                        thickness: px(1.),
                                        color: Some(fg),
                                    },
                                ),
                            };
                            let line = window.text_system().shape_line(
                                SharedString::from(text),
                                font_size,
                                &[run],
                                Some(cell_size.width),
                            );
                            let _ = line.paint(
                                origin,
                                cell_size.height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                        let cursor = content.cursor;
                        let row = cursor.point.line.0 + content.display_offset as i32;
                        if cursor.shape != CursorShape::Hidden
                            && row >= 0
                            && row < term.screen_lines() as i32
                        {
                            let origin = point(
                                bounds.origin.x + cell_size.width * cursor.point.column.0 as f32,
                                bounds.origin.y + cell_size.height * row as f32,
                            );
                            window.paint_quad(fill(
                                Bounds::new(origin, size(px(2.), cell_size.height)),
                                default_fg,
                            ));
                        }
                    },
                )
                .w_full()
                .flex_1()
                .min_h_0(),
            )
    }
}

fn color(
    color: Color,
    overrides: &alacritty_terminal::term::color::Colors,
    fg: Hsla,
    bg: Hsla,
) -> Hsla {
    let index = match color {
        Color::Spec(color) => {
            return rgb((u32::from(color.r) << 16)
                | (u32::from(color.g) << 8)
                | u32::from(color.b))
            .into();
        }
        Color::Indexed(index) => usize::from(index),
        Color::Named(name) => name as usize,
    };
    if let Some(color) = overrides[index] {
        return rgb((u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b))
            .into();
    }
    if index == NamedColor::Background as usize {
        return bg;
    }
    if index >= 256 {
        return fg;
    }
    const ANSI: [u32; 16] = [
        0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5, 0x7f7f7f,
        0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    if index < 16 {
        return rgb(ANSI[index]).into();
    }
    if index >= 232 {
        let v = 8 + (index - 232) as u32 * 10;
        return rgb((v << 16) | (v << 8) | v).into();
    }
    let value = index - 16;
    let component = |n| if n == 0 { 0 } else { 55 + 40 * n as u32 };
    rgb((component(value / 36) << 16) | (component(value / 6 % 6) << 8) | component(value % 6))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn text(session: &PtySession) -> String {
        let term = session.term.lock().unwrap();
        term.grid().display_iter().map(|cell| cell.cell.c).collect()
    }

    fn until(session: &PtySession, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let actual = text(session);
            if actual.contains(expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "terminal never printed {expected:?}: {actual:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn pty_starts_in_its_folder_and_reports_the_resized_grid() {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().canonicalize().unwrap();
        let (mut session, _) = PtySession::open(&cwd, Path::new("/bin/sh"), &["-c", "printf 'CWD='; pwd; read line; stty size; printf '\\033[31mREADY\\033[0m'; read line"]).unwrap();
        until(&session, &format!("CWD={}", cwd.display()));
        session.resize(GridSize { cols: 91, rows: 21 });
        session.write(b"\n".to_vec());
        until(&session, "21 91");
        until(&session, "READY");
        assert!(
            session
                .term
                .lock()
                .unwrap()
                .grid()
                .display_iter()
                .any(|cell| cell.cell.c == 'R' && cell.cell.fg == Color::Named(NamedColor::Red))
        );
        session.shutdown();
    }

    #[test]
    fn the_shell_gets_the_login_paths_without_session_variables() {
        let directory = tempfile::tempdir().unwrap();
        let expected = newjen_core::resolved_path().to_string_lossy();
        let (mut session, _) = PtySession::open(
            directory.path(),
            Path::new("/bin/sh"),
            &[
                "-c",
                "[ \"$PATH\" = \"$1\" ] && printf 'PATH=login '; printf 'NESTED=[%s]' \"${CLAUDECODE-}${CODEX_THREAD_ID-}\"; read line",
                "sh",
                &expected,
            ],
        )
        .unwrap();
        until(&session, "PATH=login NESTED=[]");
        session.shutdown();
    }

    #[test]
    fn dropping_a_terminal_stops_its_shell_and_background_job() {
        let directory = tempfile::tempdir().unwrap();
        let (session, _) = PtySession::open(
            directory.path(),
            Path::new("/bin/sh"),
            &[
                "-c",
                "sleep 60 & echo $! > child.pid; printf 'STARTED'; wait",
            ],
        )
        .unwrap();
        until(&session, "STARTED");
        let shell = session.pid;
        let child: i32 = std::fs::read_to_string(directory.path().join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(child, 0) }, 0);
        drop(session);
        let deadline = Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(shell, 0) == 0 || libc::kill(child, 0) == 0 } {
            assert!(
                Instant::now() < deadline,
                "terminal process survived shutdown"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shutdown_stops_a_job_that_outlived_its_shell() {
        let directory = tempfile::tempdir().unwrap();
        let (mut session, _) = PtySession::open(
            directory.path(),
            Path::new("/bin/sh"),
            &[
                "-c",
                "trap '' HUP; sleep 60 & echo $! > child.pid; printf 'STARTED'; exit",
            ],
        )
        .unwrap();
        until(&session, "STARTED");
        let child: i32 = std::fs::read_to_string(directory.path().join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.alive.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "shell did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(child, 0) }, 0);
        session.shutdown();
        while unsafe { libc::kill(child, 0) == 0 } {
            assert!(
                Instant::now() < deadline,
                "background job survived terminal shutdown"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn terminal_keys_preserve_control_and_cursor_modes() {
        let control = Keystroke::parse("ctrl-c").unwrap();
        assert_eq!(key_bytes(&control, TermMode::empty()), Some(vec![3]));
        let up = Keystroke::parse("up").unwrap();
        assert_eq!(key_bytes(&up, TermMode::empty()), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            key_bytes(&up, TermMode::APP_CURSOR),
            Some(b"\x1bOA".to_vec())
        );
        let shifted = Keystroke::parse("ctrl-right").unwrap();
        assert_eq!(
            key_bytes(&shifted, TermMode::APP_CURSOR),
            Some(b"\x1b[1;5C".to_vec())
        );
    }

    #[test]
    fn shutdown_stops_a_redirected_job_after_shell_exit_and_pty_eof() {
        let directory = tempfile::tempdir().unwrap();
        let (mut session, _) = PtySession::open(
            directory.path(),
            Path::new("/bin/sh"),
            &["-c", "(trap '' HUP; echo ready > ready; exec sleep 60) </dev/null >/dev/null 2>&1 & echo $! > child.pid; while [ ! -f ready ]; do sleep .01; done; printf 'STARTED'; exit"],
        ).unwrap();
        until(&session, "STARTED");
        let child: i32 = std::fs::read_to_string(directory.path().join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.alive.load(Ordering::Acquire) || session.output_open.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "shell and PTY did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(session.master.is_some());
        assert_eq!(unsafe { libc::kill(child, 0) }, 0);
        session.shutdown();
        while unsafe { libc::kill(child, 0) == 0 } {
            assert!(
                Instant::now() < deadline,
                "redirected job survived terminal shutdown"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
