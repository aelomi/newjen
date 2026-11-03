//! What the update flow looks like: the Check for Updates… sheet, What's New and About, and the
//! sidebar's "Restart to update" pill and "Updated to …" card (screens 19a to 19j).
//!
//! The decisions come first as plain functions, tested without a window: which sheet each
//! [`UpdateState`] gets, which buttons it has and what each button does. The elements that
//! draw them come after.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClipboardItem, Div, Entity, FontWeight, Hsla,
    IntoElement, ParentElement, RenderOnce, SharedString, StyleRefinement, Styled, StyledText,
    WeakEntity, Window, div, font, prelude::*, px, relative,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::animation::ease_out_cubic;
use gpui_kit::component::dialog::{Dialog, DialogEntrance};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::list::ListItem;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use newjen_core::changelog::{self, ReleaseNotes};
use newjen_core::update::{InstallFailure, UpdateError, UpdateState};
use semver::Version;

use crate::project_view::ProjectView;
use crate::sidebar::OnPress;
use crate::theme;
use crate::ui::{self, IconName, Look, Size};
use crate::update::{UpdateModel, WhatsNewSheet, short_version};

pub const HOMEBREW_COMMAND: &str = "brew upgrade --cask --greedy newjen";

/// How many notes the pill's popover and the Ready sheet list before "more in What's New".
pub const HEADLINES: usize = 3;

const ENTER: Duration = Duration::from_millis(200);
const RISE: f32 = 4.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Close,
    CancelCheck,
    TryAgain,
    CopyCommand,
    OpenGithub,
    Restart,
}

/// One button of a sheet. The left one answers Esc, the right one Return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: &'static str,
    pub action: Action,
    pub key: bool,
}

const fn choice(label: &'static str, action: Action) -> Choice {
    Choice {
        label,
        action,
        key: true,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lead {
    Spinner,
    Done,
    Update,
    Offline,
    Unsafe,
    Locked,
    Moved,
    Info,
    Problem,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Extra {
    None,
    Progress(Option<f32>),
    Notes(Vec<String>),
    Command(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SheetView {
    pub lead: Lead,
    pub title: String,
    pub body: Option<String>,
    pub extra: Extra,
    pub left: Option<Choice>,
    pub right: Option<Choice>,
}

impl SheetView {
    pub fn escape(&self) -> Action {
        self.left.map_or(Action::Close, |left| left.action)
    }

    pub fn enter(&self) -> Option<Action> {
        self.right.map(|right| right.action)
    }
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / 1_000_000.0
}

fn running(current: Option<&Version>) -> String {
    current.map_or_else(|| "this version".into(), Version::to_string)
}

/// The sheet for `state`. `notes` are the incoming version's first notes, if they are known.
pub fn sheet_view(state: &UpdateState, current: Option<&Version>, notes: &[String]) -> SheetView {
    let plain = |lead, title: &str, body: Option<String>| SheetView {
        lead,
        title: title.into(),
        body,
        extra: Extra::None,
        left: None,
        right: Some(choice("OK", Action::Close)),
    };
    match state {
        UpdateState::Idle | UpdateState::Checking => SheetView {
            lead: Lead::Spinner,
            title: "Checking for updates…".into(),
            body: Some("Asking GitHub for the newest Newjen.".into()),
            extra: Extra::None,
            left: Some(choice("Cancel", Action::CancelCheck)),
            right: None,
        },
        UpdateState::UpToDate { .. } => plain(
            Lead::Done,
            "Newjen is up to date",
            Some(match current {
                Some(current) => format!("{current} is the newest version. Checked just now."),
                None => "This is the newest version. Checked just now.".into(),
            }),
        ),
        UpdateState::Downloading {
            version,
            received,
            total,
        } => SheetView {
            lead: Lead::Update,
            title: format!("Downloading {version}"),
            body: Some(match total {
                Some(total) => format!(
                    "{:.1} of {:.1} MB · You can keep working.",
                    megabytes(*received),
                    megabytes(*total)
                ),
                None => format!("{:.1} MB · You can keep working.", megabytes(*received)),
            }),
            extra: Extra::Progress(
                total
                    .filter(|total| *total > 0)
                    .map(|total| (*received as f64 / total as f64).clamp(0.0, 1.0) as f32),
            ),
            left: Some(choice("Hide", Action::Close)),
            right: None,
        },
        UpdateState::Preparing { version } => SheetView {
            lead: Lead::Update,
            title: format!("Checking {version}"),
            body: Some("Newjen is checking the download's signature. You can keep working.".into()),
            extra: Extra::Progress(Some(1.0)),
            left: Some(choice("Hide", Action::Close)),
            right: None,
        },
        UpdateState::Ready(staged) => SheetView {
            lead: Lead::Update,
            title: format!("Newjen {} is ready", staged.version),
            body: Some(format!("You run {}.", running(current))),
            extra: if notes.is_empty() {
                Extra::None
            } else {
                Extra::Notes(notes.to_vec())
            },
            left: Some(choice("Later", Action::Close)),
            right: Some(choice("Restart to update", Action::Restart)),
        },
        UpdateState::Blocked { reason, .. } | UpdateState::Failed(reason) => {
            failure_view(reason, current)
        }
        UpdateState::Unavailable(unavailable) => plain(
            Lead::Info,
            "Can't check for updates",
            Some(unavailable.to_string()),
        ),
    }
}

fn failure_view(error: &UpdateError, current: Option<&Version>) -> SheetView {
    match error {
        UpdateError::Offline => SheetView {
            lead: Lead::Offline,
            title: "Couldn't check for updates".into(),
            body: Some("Newjen couldn't reach GitHub. It tries again on its own in an hour.".into()),
            extra: Extra::None,
            left: Some(choice("OK", Action::Close)),
            right: Some(choice("Try again", Action::TryAgain)),
        },
        UpdateError::Verification(detail) => SheetView {
            lead: Lead::Unsafe,
            title: "The download didn't check out".into(),
            body: Some(format!(
                "{detail} Newjen deleted it and kept {}, so nothing changed.",
                running(current)
            )),
            extra: Extra::None,
            left: None,
            right: Some(choice("OK", Action::Close)),
        },
        UpdateError::NotSigned => SheetView {
            lead: Lead::Info,
            title: "The update isn't ready yet".into(),
            body: Some(error.to_string()),
            extra: Extra::None,
            left: Some(choice("OK", Action::Close)),
            right: Some(choice("Try again", Action::TryAgain)),
        },
        UpdateError::InstallFailed(failure) => SheetView {
            lead: Lead::Problem,
            title: "The update didn't install".into(),
            body: Some(format!(
                "{failure} Newjen is still {}. Update with Homebrew, or download it from GitHub.",
                running(current)
            )),
            extra: Extra::Command(HOMEBREW_COMMAND),
            left: Some(Choice {
                label: "Copy command",
                action: Action::CopyCommand,
                key: false,
            }),
            right: Some(choice("Open GitHub", Action::OpenGithub)),
        },
        UpdateError::CantWrite { .. } => SheetView {
            lead: Lead::Locked,
            title: "Newjen can't update itself here".into(),
            body: Some(
                "It runs from a folder your account can't change. Update it with Homebrew, or download it from GitHub."
                    .into(),
            ),
            extra: Extra::Command(HOMEBREW_COMMAND),
            left: Some(Choice {
                label: "Copy command",
                action: Action::CopyCommand,
                key: false,
            }),
            right: Some(choice("Open GitHub", Action::OpenGithub)),
        },
        UpdateError::Translocated => SheetView {
            lead: Lead::Moved,
            title: "Move Newjen to Applications first".into(),
            body: Some(
                "macOS runs it from a read-only copy, so it can't replace itself. Drag Newjen.app into Applications and open it again."
                    .into(),
            ),
            extra: Extra::None,
            left: None,
            right: Some(choice("OK", Action::Close)),
        },
        UpdateError::Other(detail) => SheetView {
            lead: Lead::Problem,
            title: "Couldn't update Newjen".into(),
            body: Some(detail.clone()),
            extra: Extra::None,
            left: Some(choice("OK", Action::Close)),
            right: Some(choice("Try again", Action::TryAgain)),
        },
    }
}

/// The first `limit` notes of a release, across its groups, and how many more there are.
pub fn headlines(notes: &ReleaseNotes, limit: usize) -> (Vec<String>, usize) {
    let all: Vec<&String> = notes
        .groups
        .iter()
        .flat_map(|group| group.items.iter())
        .collect();
    let more = all.len().saturating_sub(limit);
    (all.into_iter().take(limit).cloned().collect(), more)
}

/// "a", "a and b", "a, b and c".
fn list_versions(versions: &[Version]) -> String {
    match versions {
        [] => String::new(),
        [only] => only.to_string(),
        [rest @ .., last] => format!(
            "{} and {last}",
            rest.iter()
                .map(Version::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// What the buttons and keys of the open sheets do.
#[derive(Clone)]
pub struct Sheets {
    pub model: Entity<UpdateModel>,
    pub shell: WeakEntity<ProjectView>,
}

impl Sheets {
    /// Runs `action`; true when the sheet should close.
    fn perform(&self, action: Action, window: &mut Window, cx: &mut App) -> bool {
        match action {
            Action::Close => true,
            Action::CancelCheck => {
                self.model.update(cx, |model, _| model.cancel());
                true
            }
            Action::TryAgain => {
                self.model.update(cx, |model, cx| model.check_now(cx));
                false
            }
            Action::CopyCommand => {
                cx.write_to_clipboard(ClipboardItem::new_string(HOMEBREW_COMMAND.into()));
                false
            }
            Action::OpenGithub => {
                let url = self.model.read(cx).releases_page();
                cx.open_url(&url);
                true
            }
            Action::Restart => {
                window.close_dialog(cx);
                let _ = self
                    .shell
                    .update(cx, |view, cx| view.request_restart(window, cx));
                false
            }
        }
    }

    fn click(&self, action: Action, window: &mut Window, cx: &mut App) {
        if self.perform(action, window, cx) {
            window.close_dialog(cx);
        }
    }
}

fn entrance(pointer: bool) -> DialogEntrance {
    if pointer {
        DialogEntrance::Fade
    } else {
        DialogEntrance::None
    }
}

fn scaffold(
    dialog: Dialog,
    width: f32,
    pointer: bool,
    title: impl IntoElement,
    body: impl IntoElement,
    footer: impl IntoElement,
    cx: &App,
) -> Dialog {
    dialog
        .centered()
        .width(px(width))
        .bg(theme::material(cx).card)
        .entrance(entrance(pointer))
        .close_button(false)
        .overlay_closable(false)
        .title(title)
        .child(body)
        .footer(footer)
}

fn lead_icon(lead: Lead, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let small = gpui_kit::component::Size::Small;
    match lead {
        Lead::Spinner => Spinner::new().small().color(theme.info).into_any_element(),
        Lead::Done => ui::icon(IconName::CircleCheck, small, theme.success).into_any_element(),
        Lead::Update => ui::icon(IconName::CircleArrowUp, small, theme.info).into_any_element(),
        Lead::Offline => {
            ui::icon(IconName::WifiOff, small, theme.muted_foreground).into_any_element()
        }
        Lead::Unsafe => ui::icon(IconName::ShieldAlert, small, theme.danger).into_any_element(),
        Lead::Locked => ui::icon(IconName::FolderLock, small, theme.warning).into_any_element(),
        Lead::Moved => ui::icon(IconName::FolderInput, small, theme.warning).into_any_element(),
        Lead::Info => ui::icon(IconName::Info, small, theme.info).into_any_element(),
        Lead::Problem => ui::icon(IconName::CircleAlert, small, theme.danger).into_any_element(),
    }
}

fn sheet_button(
    id: &'static str,
    choice: Choice,
    key: &'static str,
    look: Look,
    sheets: &Sheets,
    cx: &App,
) -> AnyElement {
    let sheets = sheets.clone();
    let action = choice.action;
    let content = h_flex()
        .gap_2()
        .items_center()
        .child(choice.label)
        .when(choice.key, |row| row.child(ui::key_caps(key, cx)));
    div()
        .debug_selector(move || id.into())
        .child(
            ui::button(id, "", look, Size::Md, cx)
                .child(content)
                .on_click(move |_, window, cx| sheets.click(action, window, cx)),
        )
        .into_any_element()
}

fn footer_row(left: Option<Choice>, right: Option<Choice>, sheets: &Sheets, cx: &App) -> Div {
    h_flex()
        .gap_2()
        .justify_end()
        .children(
            left.map(|left| {
                sheet_button("update-left", left, "escape", Look::Secondary, sheets, cx)
            }),
        )
        .children(
            right.map(|right| {
                sheet_button("update-right", right, "enter", Look::Primary, sheets, cx)
            }),
        )
}

fn bullet_list(items: &[String], cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    v_flex().gap_1p5().children(items.iter().map(|item| {
        h_flex()
            .items_start()
            .gap_2()
            .child(div().flex_none().w(px(10.)).text_color(muted).child("•"))
            .child(div().flex_1().min_w_0().child(InlineText(item.clone())))
    }))
}

/// A bullet's text, with its `code` spans set in the monospace font on a chip.
#[derive(IntoElement)]
struct InlineText(String);

impl RenderOnce for InlineText {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let spans = changelog::inline_spans(&self.0);
        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        let base = window.text_style();
        let mono = font(cx.theme().mono_font_family.clone());
        let chip = theme::material(cx).glass_selected;
        let runs = spans
            .iter()
            .map(|span| {
                let mut run = base.to_run(span.text.len());
                if span.code {
                    run.font = mono.clone();
                    run.background_color = Some(chip);
                }
                run
            })
            .collect();
        StyledText::new(text).with_runs(runs)
    }
}

fn progress_bar(fraction: Option<f32>, cx: &App) -> AnyElement {
    match fraction {
        Some(fraction) => div()
            .debug_selector(|| "update-progress".into())
            .h(px(6.))
            .w_full()
            .rounded_full()
            .overflow_hidden()
            .bg(theme::material(cx).glass_selected)
            .child(
                div()
                    .h_full()
                    .w(relative(fraction))
                    .rounded_full()
                    .bg(cx.theme().info),
            )
            .into_any_element(),
        None => Progress::new("update-progress-unknown")
            .loading(true)
            .color(cx.theme().info)
            .into_any_element(),
    }
}

/// The Check for Updates… sheet. It reads the update state each time the dialog layer is
/// built, so it follows a check from start to finish.
pub fn check_sheet(dialog: Dialog, sheets: &Sheets, pointer: bool, cx: &mut App) -> Dialog {
    let (state, current, notes) = {
        let model = sheets.model.read(cx);
        let notes = model
            .ready_notes()
            .map(|notes| headlines(notes, HEADLINES).0)
            .unwrap_or_default();
        (
            model.state().clone(),
            model.current_version().cloned(),
            notes,
        )
    };
    let view = sheet_view(&state, current.as_ref(), &notes);
    sheet_dialog(dialog, view, sheets, pointer, cx)
}

/// The sheet for an install that did not finish: why, and the ways to update anyway.
pub fn install_failed_sheet(
    dialog: Dialog,
    sheets: &Sheets,
    failure: &InstallFailure,
    pointer: bool,
    cx: &mut App,
) -> Dialog {
    let current = sheets.model.read(cx).current_version().cloned();
    let view = failure_view(
        &UpdateError::InstallFailed(failure.clone()),
        current.as_ref(),
    );
    sheet_dialog(dialog, view, sheets, pointer, cx)
}

fn sheet_dialog(
    dialog: Dialog,
    view: SheetView,
    sheets: &Sheets,
    pointer: bool,
    cx: &mut App,
) -> Dialog {
    let muted = cx.theme().muted_foreground;
    let material = theme::material(cx);
    let title = h_flex()
        .debug_selector(|| "update-title".into())
        .gap_2()
        .items_center()
        .child(lead_icon(view.lead, cx))
        .child(view.title.clone());
    let body = v_flex()
        .gap_3()
        .children(
            view.body
                .clone()
                .map(|body| div().text_color(muted).child(body)),
        )
        .map(|column| match &view.extra {
            Extra::None => column,
            Extra::Progress(fraction) => column.child(progress_bar(*fraction, cx)),
            Extra::Notes(items) => column.child(
                div()
                    .p_3()
                    .rounded_lg()
                    .bg(material.glass_selected)
                    .child(bullet_list(items, cx)),
            ),
            Extra::Command(command) => column.child(
                div()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .bg(material.glass_selected)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(*command),
            ),
        });
    let footer = footer_row(view.left, view.right, sheets, cx);
    let (on_ok, on_cancel) = (sheets.clone(), sheets.clone());
    let (enter, escape) = (view.enter(), view.escape());
    scaffold(dialog, 440., pointer, title, body, footer, cx)
        .on_ok(move |_, window, cx| enter.is_some_and(|action| on_ok.perform(action, window, cx)))
        .on_cancel(move |_, window, cx| on_cancel.perform(escape, window, cx))
}

/// What's New: the notes of one release, in their groups.
pub fn whats_new_sheet(
    dialog: Dialog,
    sheets: &Sheets,
    news: WhatsNewSheet,
    mark_seen: bool,
    pointer: bool,
    cx: &mut App,
) -> Dialog {
    let muted = cx.theme().muted_foreground;
    let info = cx.theme().info;
    let material = theme::material(cx);
    let subtitle = news.notes.as_ref().map(|notes| match &notes.date {
        Some(date) => format!("{} · {date}", notes.label),
        None => notes.label.clone(),
    });
    let title = v_flex()
        .debug_selector(|| "whats-new-title".into())
        .gap_0p5()
        .child("What's new in Newjen")
        .children(subtitle.map(|subtitle| {
            div()
                .text_sm()
                .font_weight(FontWeight::NORMAL)
                .text_color(muted)
                .child(subtitle)
        }));
    let releases = news.releases_url.clone();
    let notice = (!news.skipped.is_empty()).then(|| {
        let count = news.skipped.len();
        let label = news
            .notes
            .as_ref()
            .map_or_else(|| "this version".to_string(), |notes| notes.label.clone());
        let url = news.releases_url.clone();
        h_flex()
            .debug_selector(|| "whats-new-skipped".into())
            .items_start()
            .gap_2()
            .p_3()
            .rounded_lg()
            .bg(info.opacity(0.12))
            .border_1()
            .border_color(info.opacity(0.3))
            .child(div().flex_none().pt_0p5().child(ui::icon(
                IconName::RotateCcwClock,
                gpui_kit::component::Size::Small,
                info,
            )))
            .child(
                v_flex()
                    .gap_1()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                        "You skipped {count} update{}",
                        if count == 1 { "" } else { "s" }
                    )))
                    .child(div().text_sm().text_color(muted).child(format!(
                        "These are the notes for {label}. What changed in {} is on GitHub.",
                        list_versions(&news.skipped)
                    )))
                    .child(link(
                        "skipped-releases",
                        "See the releases you missed",
                        info,
                        move |_, cx| cx.open_url(&url),
                    )),
            )
    });
    let body = match &news.notes {
        Some(notes) => v_flex()
            .gap_4()
            .children(notice)
            .children(
                notes
                    .summary
                    .clone()
                    .map(|summary| div().text_color(muted).child(summary)),
            )
            .children(notes.groups.iter().map(|group| {
                v_flex()
                    .gap_1p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(muted)
                            .child(group.group.title()),
                    )
                    .child(bullet_list(&group.items, cx))
            })),
        None => v_flex().gap_4().children(notice).child(
            div()
                .text_color(muted)
                .child("There are no release notes for this version yet."),
        ),
    };
    let model = sheets.model.clone();
    let done = {
        let model = model.clone();
        move |cx: &mut App| {
            if mark_seen {
                model.update(cx, |model, cx| model.dismiss_card(cx));
            }
        }
    };
    let done_button = {
        let done = done.clone();
        div().debug_selector(|| "whats-new-done".into()).child(
            ui::button("whats-new-done", "", Look::Primary, Size::Md, cx)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child("Done")
                        .child(ui::key_caps("enter", cx)),
                )
                .on_click(move |_, window, cx| {
                    done(cx);
                    window.close_dialog(cx);
                }),
        )
    };
    let footer = h_flex()
        .items_center()
        .justify_between()
        .pt_3()
        .border_t_1()
        .border_color(material.card_stroke)
        .child(link(
            "all-release-notes",
            "All release notes on GitHub",
            info,
            move |_, cx| cx.open_url(&releases),
        ))
        .child(done_button);
    let (on_ok, on_cancel) = (done.clone(), done);
    scaffold(dialog, 560., pointer, title, body, footer, cx)
        .on_ok(move |_, _, cx| {
            on_ok(cx);
            true
        })
        .on_cancel(move |_, _, cx| {
            on_cancel(cx);
            true
        })
}

/// About Newjen: the version, and that is all.
pub fn about_sheet(
    dialog: Dialog,
    current: Option<&Version>,
    pointer: bool,
    cx: &mut App,
) -> Dialog {
    let muted = cx.theme().muted_foreground;
    let line = match current {
        Some(version) => format!("Version {version}"),
        None => "Development build".to_string(),
    };
    let ok = div().debug_selector(|| "about-ok".into()).child(
        ui::button("about-ok", "", Look::Primary, Size::Md, cx)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child("OK")
                    .child(ui::key_caps("enter", cx)),
            )
            .on_click(|_, window, cx| window.close_dialog(cx)),
    );
    scaffold(
        dialog,
        360.,
        pointer,
        div()
            .debug_selector(|| "about-title".into())
            .child("Newjen"),
        div().text_color(muted).child(line),
        h_flex().justify_end().child(ok),
        cx,
    )
    .on_ok(|_, _, _| true)
    .on_cancel(|_, _, _| true)
}

/// A blue text link with an arrow, as in the mocks.
fn link(
    id: &'static str,
    label: impl Into<SharedString>,
    color: gpui::Hsla,
    on_press: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    h_flex()
        .id(id)
        .debug_selector(move || id.into())
        .gap_1()
        .items_center()
        .text_sm()
        .text_color(color)
        .cursor_pointer()
        .hover(|row| row.opacity(0.8))
        .child(label.into())
        .child(ui::icon(
            IconName::ArrowUpRight,
            gpui_kit::component::Size::XSmall,
            color,
        ))
        .on_click(move |_, window, cx| on_press(window, cx))
        .into_any_element()
}

// -------------------------------------------------------------------------------------------
// The sidebar
// -------------------------------------------------------------------------------------------

/// What the sidebar's pill does when it is clicked or its popover's link is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateIntent {
    Restart,
    WhatsNew,
    IncomingNotes,
    InstallFailure,
}

fn rises(row: gpui::Stateful<Div>, id: &'static str) -> impl IntoElement {
    row.relative().with_animation(
        id,
        Animation::new(ENTER).with_easing(ease_out_cubic),
        |row, t| row.top(px(RISE * (t - 1.))).opacity(t),
    )
}

/// "Restart to update" with the new version's suffix, above Usage. Hover shows what is in it.
/// The popover lists the first notes, and its link opens them all.
pub fn update_pill(
    version: &Version,
    current: Option<&Version>,
    notes: Option<&ReleaseNotes>,
    on_restart: OnPress,
    on_notes: OnPress,
    cx: &App,
) -> AnyElement {
    let material = theme::material(cx);
    let muted = material.glass_muted;
    let info = cx.theme().info;
    let (items, more) = notes.map_or((Vec::new(), 0), |notes| headlines(notes, HEADLINES));
    let heading = format!("Newjen {version}");
    let subtitle = match current {
        Some(current) => format!("Downloaded and checked. You run {current}."),
        None => "Downloaded and checked.".to_string(),
    };
    let row = ListItem::new("update-pill-item")
        .w_full()
        .px_2p5()
        .py_1p5()
        .rounded_lg()
        .bg(material.glass_hover)
        .accessibility_label("Restart to update")
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default()
        })
        .on_click({
            let on_restart = on_restart.clone();
            move |_, window, cx| on_restart(window, cx)
        })
        .debug_selector(|| "update-pill".to_string())
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(ui::icon(
                    IconName::CircleArrowUp,
                    gpui_kit::component::Size::Small,
                    info,
                ))
                .child(div().flex_1().child("Restart to update"))
                .child(
                    div()
                        .flex_none()
                        .text_sm()
                        .text_color(muted)
                        .child(short_version(version)),
                ),
        );
    let card = HoverCard::new("update-pill-card")
        .anchor(gpui::Anchor::BottomLeft)
        .open_delay(Duration::from_millis(300))
        .close_delay(Duration::from_millis(150))
        .appearance(false)
        .trigger_style(StyleRefinement::default().w_full())
        .trigger(row)
        .content(move |_, _, cx| {
            let on_notes = on_notes.clone();
            let body = v_flex()
                .debug_selector(|| "update-pill-popover".into())
                .w(px(300.))
                .gap_2()
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(heading.clone()),
                        )
                        .child(div().text_sm().text_color(muted).child(subtitle.clone())),
                )
                .when(!items.is_empty(), |column| {
                    column
                        .child(div().text_sm().child(bullet_list(&items, cx)))
                        .when(more > 0, |column| {
                            column.child(link(
                                "update-more",
                                format!("+ {more} more in What's New"),
                                info,
                                move |window, cx| on_notes(window, cx),
                            ))
                        })
                })
                .child(
                    div()
                        .pt_2()
                        .border_t_1()
                        .border_color(material.card_stroke)
                        .text_sm()
                        .text_color(muted)
                        .child("Click to restart now. Otherwise it installs when you quit."),
                );
            ui::solid_popover_surface("update-pill-enter", div().p_3().child(body), true, cx)
        });
    div()
        .flex_none()
        .pb_1()
        .child(rises(
            div().id("update-pill-row").w_full().child(card),
            "update-pill-rise",
        ))
        .into_any_element()
}

/// The ids a sidebar card is found by.
struct CardIds {
    card: &'static str,
    open: &'static str,
    dismiss: &'static str,
    rise: &'static str,
    row: &'static str,
    icon: IconName,
    tint: Hsla,
    link_text: &'static str,
}

/// A card above Usage: an icon, a line, a link, and a button that puts it away.
fn notice_card(
    ids: CardIds,
    line: String,
    on_open: OnPress,
    on_dismiss: OnPress,
    cx: &App,
) -> AnyElement {
    let (icon, tint, link_text) = (ids.icon, ids.tint, ids.link_text);
    let card = h_flex()
        .id(ids.card)
        .debug_selector(move || ids.card.to_string())
        .w_full()
        .items_start()
        .gap_2()
        .pl_2p5()
        .pr_1()
        .py_1p5()
        .rounded_lg()
        .bg(tint.opacity(0.1))
        .child(div().flex_none().pt_0p5().child(ui::icon(
            icon,
            gpui_kit::component::Size::Small,
            tint,
        )))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(ui::ellipsis(line).text_sm())
                .child(
                    div()
                        .id(ids.open)
                        .debug_selector(move || ids.open.to_string())
                        .text_sm()
                        .text_color(tint)
                        .cursor_pointer()
                        .on_click(move |_, window, cx| on_open(window, cx))
                        .child(link_text),
                ),
        )
        .child(
            ui::icon_button(ids.dismiss, IconName::Close, "Dismiss", cx)
                .debug_selector(move || ids.dismiss.to_string())
                .on_click(move |_, window, cx| on_dismiss(window, cx)),
        );
    div()
        .flex_none()
        .pb_1()
        .child(rises(div().id(ids.row).w_full().child(card), ids.rise))
        .into_any_element()
}

/// "Updated to 0.1.0-alpha.2 · See what's new", with a button that puts it away.
pub fn updated_card(
    version: String,
    on_open: OnPress,
    on_dismiss: OnPress,
    cx: &App,
) -> AnyElement {
    notice_card(
        CardIds {
            card: "updated-card",
            open: "updated-card-open",
            dismiss: "updated-card-dismiss",
            rise: "updated-card-rise",
            row: "updated-card-row",
            icon: IconName::Sparkles,
            tint: cx.theme().info,
            link_text: "See what's new",
        },
        format!("Updated to {version}"),
        on_open,
        on_dismiss,
        cx,
    )
}

/// "Couldn't install 0.1.0-alpha.3 · Other ways to update", until it is put away.
pub fn install_failed_card(
    failure: &InstallFailure,
    on_open: OnPress,
    on_dismiss: OnPress,
    cx: &App,
) -> AnyElement {
    notice_card(
        CardIds {
            card: "install-failed-card",
            open: "install-failed-card-open",
            dismiss: "install-failed-card-dismiss",
            rise: "install-failed-card-rise",
            row: "install-failed-card-row",
            icon: IconName::CircleAlert,
            tint: cx.theme().warning,
            link_text: "Other ways to update",
        },
        format!("Couldn't install {}", failure.version),
        on_open,
        on_dismiss,
        cx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::scripted::staged;
    use newjen_core::update::Unavailable;

    fn state_view(state: UpdateState) -> SheetView {
        sheet_view(&state, Some(&"0.1.0-alpha.2".parse().unwrap()), &[])
    }

    #[test]
    fn every_state_has_a_sheet_with_the_buttons_the_mock_shows() {
        let checking = state_view(UpdateState::Checking);
        assert_eq!(checking.title, "Checking for updates…");
        assert_eq!(checking.left, Some(choice("Cancel", Action::CancelCheck)));
        assert_eq!(checking.right, None);
        assert_eq!(checking.enter(), None);
        assert_eq!(checking.escape(), Action::CancelCheck);

        let current = state_view(UpdateState::UpToDate { checked_at: 0 });
        assert_eq!(current.title, "Newjen is up to date");
        assert_eq!(current.right, Some(choice("OK", Action::Close)));
        assert_eq!(current.escape(), Action::Close);

        let downloading = state_view(UpdateState::Downloading {
            version: "0.1.0-alpha.3".parse().unwrap(),
            received: 6_200_000,
            total: Some(14_800_000),
        });
        assert_eq!(downloading.title, "Downloading 0.1.0-alpha.3");
        assert_eq!(
            downloading.body.as_deref(),
            Some("6.2 of 14.8 MB · You can keep working.")
        );
        assert_eq!(downloading.left, Some(choice("Hide", Action::Close)));
        let Extra::Progress(Some(fraction)) = downloading.extra else {
            panic!("a progress bar");
        };
        assert!((fraction - 6.2 / 14.8).abs() < 0.001);

        let ready = state_view(UpdateState::Ready(staged("0.1.0-alpha.3")));
        assert_eq!(ready.title, "Newjen 0.1.0-alpha.3 is ready");
        assert_eq!(ready.body.as_deref(), Some("You run 0.1.0-alpha.2."));
        assert_eq!(ready.left, Some(choice("Later", Action::Close)));
        assert_eq!(
            ready.right,
            Some(choice("Restart to update", Action::Restart))
        );
        assert_eq!(ready.enter(), Some(Action::Restart));
    }

    #[test]
    fn failures_and_blocks_reach_the_same_sheets() {
        let version: Version = "0.1.0-alpha.3".parse().unwrap();
        let cant_write = UpdateError::CantWrite {
            folder: "/Applications".into(),
        };
        let blocked = state_view(UpdateState::Blocked {
            version: version.clone(),
            reason: cant_write.clone(),
        });
        assert_eq!(blocked, state_view(UpdateState::Failed(cant_write)));
        assert_eq!(blocked.extra, Extra::Command(HOMEBREW_COMMAND));
        assert_eq!(blocked.left.unwrap().action, Action::CopyCommand);
        assert!(!blocked.left.unwrap().key);
        assert_eq!(
            blocked.right,
            Some(choice("Open GitHub", Action::OpenGithub))
        );

        let offline = state_view(UpdateState::Failed(UpdateError::Offline));
        assert_eq!(offline.title, "Couldn't check for updates");
        assert_eq!(offline.left, Some(choice("OK", Action::Close)));
        assert_eq!(offline.right, Some(choice("Try again", Action::TryAgain)));

        let unsafe_download = state_view(UpdateState::Failed(UpdateError::Verification(
            "The update's signature doesn't match its contents.".into(),
        )));
        assert_eq!(unsafe_download.title, "The download didn't check out");
        assert_eq!(unsafe_download.lead, Lead::Unsafe);
        assert_eq!(unsafe_download.right, Some(choice("OK", Action::Close)));

        let moved = state_view(UpdateState::Blocked {
            version,
            reason: UpdateError::Translocated,
        });
        assert_eq!(moved.title, "Move Newjen to Applications first");

        for unavailable in [Unavailable::DevBuild, Unavailable::NotBundled] {
            let view = state_view(UpdateState::Unavailable(unavailable));
            assert_eq!(view.body, Some(unavailable.to_string()));
            assert!(
                view.body
                    .unwrap()
                    .starts_with("Updates work in the released app")
            );
        }
    }

    #[test]
    fn an_unsigned_release_and_a_failed_install_have_their_own_words() {
        let unsigned = state_view(UpdateState::Failed(UpdateError::NotSigned));
        assert_eq!(
            unsigned.body.as_deref(),
            Some("The release isn't signed yet. Newjen will try again later.")
        );
        assert_eq!(unsigned.left, Some(choice("OK", Action::Close)));

        let failure = InstallFailure {
            version: "0.1.0-alpha.3".parse().unwrap(),
            reason: "Couldn't put the new version in place.".into(),
        };
        let failed = failure_view(&UpdateError::InstallFailed(failure), None);
        assert!(
            failed.body.as_deref().unwrap().starts_with(
                "Couldn't install 0.1.0-alpha.3: Couldn't put the new version in place."
            )
        );
        assert_eq!(failed.extra, Extra::Command(HOMEBREW_COMMAND));
        assert_eq!(
            failed.right,
            Some(choice("Open GitHub", Action::OpenGithub))
        );
    }

    #[test]
    fn headlines_take_the_first_notes_across_groups() {
        let changelog = changelog::Changelog::parse(
            "## 1.0.0 · 1 May 2026\n\n### New features\n\n- One\n- Two\n\n### Fixes\n\n- Three\n- Four\n- Five\n",
        );
        let notes = changelog.releases()[0].clone();
        assert_eq!(
            headlines(&notes, 3),
            (vec!["One".into(), "Two".into(), "Three".into()], 2)
        );
        assert_eq!(headlines(&notes, 9).1, 0);
    }

    #[test]
    fn skipped_versions_read_as_a_list() {
        let versions = |names: &[&str]| -> Vec<Version> {
            names.iter().map(|name| name.parse().unwrap()).collect()
        };
        assert_eq!(list_versions(&versions(&["1.0.0"])), "1.0.0");
        assert_eq!(
            list_versions(&versions(&["1.0.0", "1.1.0"])),
            "1.0.0 and 1.1.0"
        );
        assert_eq!(
            list_versions(&versions(&["1.0.0", "1.1.0", "1.2.0"])),
            "1.0.0, 1.1.0 and 1.2.0"
        );
    }
}
