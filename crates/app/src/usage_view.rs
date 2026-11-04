//! The Usage view: both providers' plan windows with their pace, the threads that used the
//! current windows, and a week of tokens. Everything comes from what the CLIs reported and
//! from what Newjen's own threads saved; nothing here asks a CLI or an API for anything.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px, relative,
};
use gpui_kit::base::{Selectable as _, h_flex, v_flex};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use newjen_core::picker::{self, reset_time};
use newjen_core::providers::{Availability, Billing, Provider, ProviderStatus};
use newjen_core::store::{DailyTokens, ThreadWindowUsage, local_utc_offset_seconds};
use newjen_core::usage::{self, Pace, ProviderUsage, UsageCredits, UsageStatus, UsageWindow};
use newjen_core::{Status, Store};

use crate::format::{as_of, compact_tokens, dollars, model_name, span, thousands};
use crate::keys::OpenSettings;
use crate::sidebar::{OnPress, compact_time};
use crate::theme::material;
use crate::ui;

const PROVIDERS: [Provider; 2] = [Provider::Claude, Provider::Codex];
const WARNING_FRACTION: f32 = 0.9;
const WEEK_DAYS: u32 = 7;
const THREAD_ROWS: usize = 8;
const CHART_HEIGHT: f32 = 130.0;
const TABLE_BAR: f32 = 64.0;
const TABLE_AGENT: f32 = 112.0;
const LIMIT_REACHED: &str = "You choose when to continue; Newjen never resumes on its own.";
const NOTHING_REPORTED: &str = "They show here after this account's next reply.";

/// How a window reads: amber from 90% or when the pace empties it before the reset, red at
/// the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Normal,
    Warning,
    Limited,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowModel {
    pub key: String,
    pub label: String,
    /// "resets 4:10 pm", or "reset" once the time has passed.
    pub reset: Option<String>,
    /// The big figure and the bar's fill. None past the reset, or while the CLI gave none.
    pub used: Option<(String, f32)>,
    /// Where even spending would be by now, 0.0 to 1.0.
    pub mark: Option<f32>,
    pub line: Option<String>,
    pub tone: Tone,
}

/// How a window reads: amber from 90% or when the pace empties it before the reset, red at
/// the limit, plain once it has reset.
fn window_tone(window: &UsageWindow, now: i64) -> Tone {
    if window.expired(now) {
        return Tone::Normal;
    }
    let used = window.used_fraction.unwrap_or(0.0);
    let runs_out = matches!(
        window.pace(now),
        Some(Pace::Ahead {
            runs_out_at: Some(_)
        })
    );
    match window.status {
        UsageStatus::Limited => Tone::Limited,
        UsageStatus::Warning => Tone::Warning,
        UsageStatus::Ok if used >= WARNING_FRACTION || runs_out => Tone::Warning,
        UsageStatus::Ok => Tone::Normal,
    }
}

pub fn window_model(provider: Provider, window: &UsageWindow, now: i64) -> WindowModel {
    let label = provider.presentation().label;
    let expired = window.expired(now);
    let pace = window.pace(now);
    let tone = window_tone(window, now);
    let remaining = window
        .resets_at_unix_seconds
        .filter(|_| window.length_seconds.is_some_and(|length| length <= 86_400))
        .map(|reset| format!(" {} left in this window.", span(reset - now)))
        .unwrap_or_default();
    let line = if expired {
        Some(
            "Reset since the last report, so no percentage. The next reply brings a new reading."
                .to_string(),
        )
    } else if tone == Tone::Limited {
        Some(format!("{label}'s limit is reached. {LIMIT_REACHED}"))
    } else {
        match pace {
            Some(Pace::Under) => Some(format!("Under pace.{remaining}")),
            Some(Pace::On) => Some(format!("On pace.{remaining}")),
            Some(Pace::Ahead {
                runs_out_at: Some(out),
            }) => {
                let reset = window.resets_at_unix_seconds.unwrap_or(out);
                Some(format!(
                    "At this pace it runs out around {}, {} before it resets.",
                    reset_time(out, now),
                    span(reset - out)
                ))
            }
            Some(Pace::Ahead { runs_out_at: None }) => Some("Ahead of an even pace.".to_string()),
            None => None,
        }
    };
    WindowModel {
        key: window.key.clone(),
        label: window.label.clone(),
        reset: reset_line(window, now),
        used: window
            .used_fraction
            .filter(|_| !expired)
            .map(|used| (format!("{}%", (used * 100.0).round() as i64), used)),
        mark: pace.and(window.elapsed_fraction(now)),
        line,
        tone,
    }
}

/// "resets 4:10pm", or "reset" once the time has passed; nothing when none was reported.
pub fn reset_line(window: &UsageWindow, now: i64) -> Option<String> {
    if window.expired(now) {
        return Some("reset".to_string());
    }
    window
        .resets_at_unix_seconds
        .map(|reset| format!("resets {}", reset_time(reset, now)))
}

/// The credits line as a label and a value: from 100 up a balance is rounded to whole credits
/// with thousands separators ("66,389"), below it keeps its cents ("12.50").
pub fn credits_line(credits: &UsageCredits) -> (String, Option<String>) {
    match credits.balance {
        Some(balance) if !credits.unlimited && balance >= 100.0 => {
            ("Credits".into(), Some(thousands(balance.round() as usize)))
        }
        _ => {
            let summary = credits.summary();
            match summary.strip_prefix("Credits ") {
                Some(value) => ("Credits".to_string(), Some(value.to_string())),
                None => (summary, None),
            }
        }
    }
}

/// What the footer ring shows: the tightest limit among the providers in use.
#[derive(Debug, Clone, PartialEq)]
pub struct Gauge {
    pub provider: Provider,
    /// How full the window is, 0.0 to 1.0.
    pub fraction: f32,
    /// Where even spending would be by now, when the pace is known.
    pub pace: Option<f32>,
    pub tone: Tone,
    /// Beside the ring, only when a limit is near: "Limit by 3:40pm" or "Resets 4:10pm".
    pub label: Option<String>,
}

/// The tightest live window of the providers in `in_use`: the worst tone first, then the
/// fullest. A provider that is not in use never raises it, nor does one billed through an API
/// key, whose plan windows say nothing. A window past its reset says nothing either.
pub fn gauge(usages: &[ProviderUsage], in_use: &[Provider], now: i64) -> Option<Gauge> {
    usages
        .iter()
        .filter(|usage| in_use.contains(&usage.provider) && usage.billing != Billing::ApiKey)
        .flat_map(|usage| {
            usage
                .windows
                .iter()
                .map(move |window| (usage.provider, window))
        })
        .filter(|(_, window)| !window.expired(now))
        .filter_map(|(provider, window)| {
            let tone = window_tone(window, now);
            let fraction = match tone {
                Tone::Limited => 1.0,
                _ => window
                    .used_fraction
                    .or((tone == Tone::Warning).then_some(WARNING_FRACTION))?,
            };
            Some((tone, fraction, provider, window))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)))
        .map(|(tone, fraction, provider, window)| Gauge {
            provider,
            fraction,
            pace: window.pace(now).and(window.elapsed_fraction(now)),
            tone,
            label: gauge_label(window, tone, now),
        })
}

fn gauge_label(window: &UsageWindow, tone: Tone, now: i64) -> Option<String> {
    match tone {
        Tone::Normal => None,
        Tone::Limited => Some(match window.resets_at_unix_seconds {
            Some(reset) => format!("Resets {}", reset_time(reset, now)),
            None => "Limit reached".to_string(),
        }),
        Tone::Warning => match window.pace(now) {
            Some(Pace::Ahead {
                runs_out_at: Some(out),
            }) => Some(format!("Limit by {}", reset_time(out, now))),
            _ => window
                .used_fraction
                .map(|used| format!("{}% used", (used * 100.0).round() as i64)),
        },
    }
}

/// The providers whose limits can raise the ring: one with a working thread, the one of the
/// thread on screen, and one that Newjen's own turns used in its current window. `with_turns`
/// is given the first two kinds and says which others the turns used.
pub fn providers_in_use(
    threads: impl IntoIterator<Item = (Provider, Status)>,
    on_screen: Option<Provider>,
    with_turns: impl FnOnce(&[Provider]) -> Vec<Provider>,
) -> Vec<Provider> {
    let working = threads
        .into_iter()
        .filter(|(_, status)| status.is_busy())
        .map(|(provider, _)| provider);
    let mut providers: Vec<Provider> = Vec::new();
    for provider in working.chain(on_screen) {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    for provider in with_turns(&providers) {
        if !providers.contains(&provider) {
            providers.push(provider);
        }
    }
    providers
}

/// The providers with turn usage in the window the Usage view counts from (`current_window`).
/// `used_since` says whether Newjen's turns used a provider from a Unix-second instant on.
pub fn providers_with_turns(
    usages: &[ProviderUsage],
    now: i64,
    used_since: impl Fn(Provider, i64) -> bool,
) -> Vec<Provider> {
    usages
        .iter()
        .filter(|usage| {
            current_window(usage, now)
                .is_some_and(|(_, started)| used_since(usage.provider, started))
        })
        .map(|usage| usage.provider)
        .collect()
}

/// The reports on the board, for every provider that has one.
pub fn board_usages() -> Vec<ProviderUsage> {
    PROVIDERS.into_iter().filter_map(usage::latest).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Charged {
    pub week: String,
    pub today: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CardBody {
    Checking,
    Unavailable(String),
    SignedOut {
        command: &'static str,
    },
    /// Signed in, with no limit reported yet.
    Quiet,
    ApiKey {
        variable: &'static str,
        charged: Option<Charged>,
    },
    Plan {
        windows: Vec<WindowModel>,
        credits: Option<(String, Option<String>)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CardModel {
    pub provider: Provider,
    pub plan: Option<String>,
    /// Beside the name, at the right: "Subscription · as of 2 min ago".
    pub meta: Option<String>,
    pub meta_warning: bool,
    pub body: CardBody,
}

pub fn card_model(
    provider: Provider,
    status: Option<&ProviderStatus>,
    usage: Option<&ProviderUsage>,
    charged: Option<(f64, f64)>,
    now: i64,
) -> CardModel {
    let label = provider.presentation().label;
    let card = |plan, meta, body| CardModel {
        provider,
        plan,
        meta,
        meta_warning: false,
        body,
    };
    match status.map(|status| &status.availability) {
        Some(Availability::SignedOut { .. }) => {
            return card(
                None,
                Some("Not signed in".into()),
                CardBody::SignedOut {
                    command: match provider {
                        Provider::Claude => "claude",
                        Provider::Codex => "codex login",
                    },
                },
            );
        }
        Some(Availability::NotInstalled { .. } | Availability::Broken { .. }) => {
            let detail = status
                .and_then(|status| picker::account_view(status).unavailable)
                .unwrap_or_else(|| format!("Couldn't check {label}"));
            return card(None, None, CardBody::Unavailable(detail));
        }
        Some(Availability::Ready { .. }) | None => {}
    }
    let Some(usage) = usage else {
        return card(None, None, CardBody::Checking);
    };
    let age = as_of(usage.updated_at_unix_seconds, now);
    if usage.billing == Billing::ApiKey {
        let variable = provider.api_key_var();
        return CardModel {
            meta: Some(format!("API key · {variable}")),
            meta_warning: true,
            ..card(
                None,
                None,
                CardBody::ApiKey {
                    variable,
                    charged: charged
                        .filter(|_| provider.reports_price())
                        .map(|(week, today)| Charged {
                            week: dollars(week),
                            today: dollars(today),
                        }),
                },
            )
        };
    }
    let meta = [
        (usage.billing == Billing::Subscription).then(|| "Subscription".to_string()),
        age,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let meta = (!meta.is_empty()).then(|| meta.join(" · "));
    let body = if usage.windows.is_empty() && usage.credits.is_none() {
        CardBody::Quiet
    } else {
        CardBody::Plan {
            windows: usage
                .windows
                .iter()
                .map(|window| window_model(provider, window, now))
                .collect(),
            credits: usage.credits.as_ref().map(credits_line),
        }
    };
    card(usage.plan_label(), meta, body)
}

/// The shortest window with a known length, and when to count its threads from: its start,
/// or a whole length back from now once it has reset without a newer report.
pub fn current_window(usage: &ProviderUsage, now: i64) -> Option<(&UsageWindow, i64)> {
    let window = usage
        .windows
        .iter()
        .filter(|window| window.started_at().is_some())
        .min_by_key(|window| window.length_seconds)?;
    let started = if window.expired(now) {
        now - window.length_seconds?
    } else {
        window.started_at()?
    };
    Some((window, started))
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadLine {
    pub thread_id: String,
    pub project_id: i64,
    pub title: String,
    pub archived: bool,
    pub provider: Provider,
    /// The model that used the most tokens.
    pub model: String,
    pub tokens: u64,
    pub last_active_ms: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayBar {
    pub label: String,
    pub claude: u64,
    pub codex: u64,
}

impl DayBar {
    fn total(&self) -> u64 {
        self.claude + self.codex
    }
}

/// What the store says, read when the view opens and now and then while it stays open.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub threads: Vec<ThreadLine>,
    /// Which windows the table counts, "5-hour" when all are.
    pub window_name: Option<String>,
    pub days: Vec<DayBar>,
    pub busiest: Option<String>,
    pub charged: HashMap<Provider, (f64, f64)>,
}

impl Snapshot {
    pub fn load(store: &Store, now_ms: i64) -> Self {
        let now = now_ms / 1000;
        let mut snapshot = Self::default();
        let days = store
            .daily_tokens(now_ms, WEEK_DAYS, local_utc_offset_seconds)
            .unwrap_or_default();
        let week_start = days
            .first()
            .map_or(now - 7 * 86_400, |d| d.day_start_unix_seconds)
            * 1000;
        let today_start = days
            .last()
            .map_or(now - 86_400, |d| d.day_start_unix_seconds)
            * 1000;
        snapshot.days = day_bars(&days);
        let mut week: HashMap<String, (String, u64)> = HashMap::new();
        let mut names = Vec::new();
        for provider in PROVIDERS {
            for thread in store
                .threads_with_usage_since(provider, week_start)
                .unwrap_or_default()
            {
                let entry = week
                    .entry(thread.thread_id.clone())
                    .or_insert((thread.title.clone(), 0));
                entry.1 += thread.tokens.total();
            }
            let usage = usage::latest(provider);
            if provider.reports_price()
                && usage.as_ref().is_some_and(|u| u.billing == Billing::ApiKey)
            {
                let charged = |since| store.charged_since(provider, since).unwrap_or_default();
                snapshot
                    .charged
                    .insert(provider, (charged(week_start), charged(today_start)));
            }
            let Some((window, started)) =
                usage.as_ref().and_then(|usage| current_window(usage, now))
            else {
                continue;
            };
            names.push(window.label.clone());
            snapshot.threads.extend(
                store
                    .threads_with_usage_since(provider, started * 1000)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|thread| thread_line(provider, thread)),
            );
        }
        snapshot.threads.sort_by(|a, b| {
            b.tokens
                .cmp(&a.tokens)
                .then_with(|| b.last_active_ms.cmp(&a.last_active_ms))
        });
        snapshot.threads.truncate(THREAD_ROWS);
        snapshot.window_name = (!names.is_empty() && names.iter().all(|name| name == "5 hours"))
            .then(|| "5-hour".to_string());
        snapshot.busiest = week
            .into_values()
            .filter(|(_, tokens)| *tokens > 0)
            .max_by_key(|(_, tokens)| *tokens)
            .map(|(title, _)| title);
        snapshot
    }

    pub fn week_tokens(&self) -> u64 {
        self.days.iter().map(DayBar::total).sum()
    }
}

fn thread_line(provider: Provider, thread: ThreadWindowUsage) -> ThreadLine {
    ThreadLine {
        thread_id: thread.thread_id,
        project_id: thread.project_id,
        title: thread.title,
        archived: thread.archived,
        provider,
        model: thread
            .models
            .first()
            .map(|m| model_name(m))
            .unwrap_or_default(),
        tokens: thread.tokens.total(),
        last_active_ms: thread.last_active_ms,
    }
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The weekday ("Tue") of the local day that began at `day_start` Unix seconds.
fn weekday(day_start: i64) -> &'static str {
    let local = day_start + i64::from(local_utc_offset_seconds(day_start));
    WEEKDAYS[(local.div_euclid(86_400) + 4).rem_euclid(7) as usize]
}

/// One bar per local day, oldest first, the last one "Today".
pub fn day_bars(days: &[DailyTokens]) -> Vec<DayBar> {
    let mut bars: Vec<(i64, DayBar)> = Vec::new();
    for day in days {
        let at = match bars
            .iter()
            .position(|(start, _)| *start == day.day_start_unix_seconds)
        {
            Some(at) => at,
            None => {
                bars.push((
                    day.day_start_unix_seconds,
                    DayBar {
                        label: weekday(day.day_start_unix_seconds).to_string(),
                        claude: 0,
                        codex: 0,
                    },
                ));
                bars.len() - 1
            }
        };
        let total = day.tokens.total();
        match day.provider {
            Provider::Claude => bars[at].1.claude += total,
            Provider::Codex => bars[at].1.codex += total,
        }
    }
    let mut bars: Vec<DayBar> = bars.into_iter().map(|(_, bar)| bar).collect();
    if let Some(last) = bars.last_mut() {
        last.label = "Today".into();
    }
    bars
}

pub type OnOpenThread = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// What the view draws.
pub struct Input<'a> {
    pub snapshot: &'a Snapshot,
    pub statuses: &'a [ProviderStatus],
    pub now: i64,
    pub now_ms: i64,
    /// Rows whose thread is in the project on screen: they open it.
    pub openable: &'a [String],
    pub on_open: OnOpenThread,
}

pub fn render(input: &Input, cx: &App) -> AnyElement {
    let cards = PROVIDERS.map(|provider| {
        let model = card_model(
            provider,
            input.statuses.iter().find(|s| s.provider == provider),
            usage::latest(provider).as_ref(),
            input.snapshot.charged.get(&provider).copied(),
            input.now,
        );
        card_view(&model, cx)
    });
    let [claude, codex] = cards;
    v_flex()
        .id("usage-scroll")
        .debug_selector(|| "usage-view".to_string())
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_4()
        .gap_4()
        .child(
            h_flex()
                .flex_wrap()
                .items_stretch()
                .gap_4()
                .child(claude)
                .child(codex),
        )
        .child(
            h_flex()
                .flex_wrap()
                .items_start()
                .gap_4()
                .child(threads_card(input, cx))
                .child(week_card(input.snapshot, cx)),
        )
        .into_any_element()
}

fn panel(cx: &App) -> gpui::Div {
    let material = material(cx);
    div()
        .min_w(px(320.))
        .flex_1()
        .overflow_hidden()
        .p_4()
        .rounded_xl()
        .border_1()
        .border_color(material.card_stroke)
        .bg(material.glass_hover)
}

fn tone_color(tone: Tone, cx: &App) -> Hsla {
    let colors = cx.theme().colors;
    match tone {
        Tone::Normal => colors.foreground,
        Tone::Warning => colors.warning,
        Tone::Limited => colors.danger,
    }
}

fn card_view(model: &CardModel, cx: &App) -> AnyElement {
    let muted = material(cx).glass_muted;
    let colors = cx.theme().colors;
    let provider = model.provider;
    let header = h_flex()
        .items_center()
        .gap_2()
        .min_w_0()
        .child(ui::provider_icon(provider, cx).size_5().flex_none())
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .text_lg()
                .child(provider.presentation().label),
        )
        .children(
            model
                .plan
                .clone()
                .map(|plan| ui::status_badge(plan, ui::Tone::Neutral)),
        )
        .child(div().flex_1())
        .children(model.meta.clone().map(|meta| {
            div()
                .min_w_0()
                .text_ellipsis()
                .text_sm()
                .text_color(if model.meta_warning {
                    colors.warning
                } else {
                    muted
                })
                .child(meta)
        }));
    let note = |text: String| div().text_sm().text_color(muted).child(text);
    let mut card = panel(cx)
        .id(SharedString::from(format!("usage-card-{}", provider.id())))
        .debug_selector(move || format!("usage-card-{}", provider.id()))
        .child(header);
    let body = v_flex().mt_3().gap_3();
    card = card.child(match &model.body {
        CardBody::Checking => {
            body.child(note(format!("Checking {}…", provider.presentation().label)))
        }
        CardBody::Unavailable(detail) => body.child(note(detail.clone())),
        CardBody::SignedOut { command } => body
            .child(note(format!(
                "Run {command} in your own terminal to sign in. Newjen never handles your login."
            )))
            .child(
                div()
                    .self_start()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(material(cx).glass_selected)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_sm()
                    .child(command.to_string()),
            ),
        CardBody::Quiet => body.child(note(format!(
            "{} reports its limits during a turn. {NOTHING_REPORTED}",
            provider.presentation().label
        ))),
        CardBody::ApiKey { variable, charged } => {
            let line = |label: &str, value: String| {
                h_flex()
                    .justify_between()
                    .child(div().text_color(muted).child(label.to_string()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(value))
            };
            body.child(
                div().text_sm().text_color(colors.warning).child(
                    "Threads bill the API per token, not your plan. Plan limits do not apply.",
                ),
            )
            .children(
                charged
                    .as_ref()
                    .map(|c| line("Charged in the last 7 days", c.week.clone())),
            )
            .children(charged.as_ref().map(|c| line("Today", c.today.clone())))
            .children((charged.is_none() && !provider.reports_price()).then(|| {
                note(format!(
                    "{} does not report a price. Charged to your API key ({variable}).",
                    provider.presentation().label
                ))
            }))
        }
        CardBody::Plan { windows, credits } => body
            .children(
                windows
                    .iter()
                    .map(|window| window_view(provider, window, cx)),
            )
            .children(credits.as_ref().map(|(label, value)| {
                h_flex()
                    .justify_between()
                    .pt_3()
                    .border_t_1()
                    .border_color(material(cx).card_stroke)
                    .child(div().text_color(muted).child(label.clone()))
                    .children(value.clone().map(|value| div().child(value)))
            })),
    });
    card.into_any_element()
}

fn window_view(provider: Provider, window: &WindowModel, cx: &App) -> AnyElement {
    let muted = material(cx).glass_muted;
    let color = tone_color(window.tone, cx);
    let reset_color = if window.tone == Tone::Limited {
        color
    } else {
        muted
    };
    let bar = window.used.as_ref().map(|(_, used)| {
        div()
            .relative()
            .w_full()
            .child(
                Progress::new(format!("usage-{}-{}", provider.id(), window.key))
                    .value(used * 100.0)
                    .color(color)
                    .with_size(gpui_kit::component::Size::Size(px(6.))),
            )
            .children(window.mark.map(|mark| {
                div()
                    .absolute()
                    .top(px(-2.))
                    .left(relative(mark))
                    .w(px(2.))
                    .h(px(10.))
                    .rounded_full()
                    .bg(muted)
            }))
    });
    v_flex()
        .gap_1p5()
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .text_sm()
                .text_color(muted)
                .child(div().min_w_0().child(window.label.clone()))
                .children(
                    window
                        .reset
                        .clone()
                        .map(|reset| div().flex_none().text_color(reset_color).child(reset)),
                ),
        )
        .children(window.used.as_ref().map(|(percent, _)| {
            h_flex()
                .items_baseline()
                .gap_1p5()
                .child(
                    div()
                        .text_color(color)
                        .text_size(px(26.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(percent.clone()),
                )
                .child(div().text_color(muted).child("used"))
        }))
        .children(bar)
        .children(window.line.clone().map(|line| {
            div()
                .text_sm()
                .text_color(if window.tone == Tone::Normal {
                    muted
                } else {
                    color
                })
                .child(line)
        }))
        .into_any_element()
}

fn threads_card(input: &Input, cx: &App) -> AnyElement {
    let muted = material(cx).glass_muted;
    let stroke = material(cx).card_stroke;
    let snapshot = input.snapshot;
    let title = match &snapshot.window_name {
        Some(name) => format!("Threads in the current {name} windows"),
        None => "Threads in the current windows".to_string(),
    };
    let top = snapshot
        .threads
        .iter()
        .map(|t| t.tokens)
        .max()
        .unwrap_or(1)
        .max(1);
    let head = |text: &'static str| div().text_xs().text_color(muted).child(text);
    let table_row = |thread: &ThreadLine| {
        let openable = input.openable.contains(&thread.thread_id);
        let id = thread.thread_id.clone();
        let on_open = input.on_open.clone();
        let fill = (thread.tokens as f32 / top as f32).clamp(0.04, 1.0);
        h_flex()
            .id(SharedString::from(format!("usage-thread-{id}")))
            .items_center()
            .gap_3()
            .py_2()
            .border_t_1()
            .border_color(stroke)
            .text_sm()
            .when_some(openable.then_some(()), |row, _| {
                row.cursor_pointer()
                    .hover(|row| row.bg(material(cx).glass_hover))
                    .on_click(move |_, window, cx| on_open(&id, window, cx))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .text_color(if thread.archived {
                        muted
                    } else {
                        cx.theme().colors.foreground
                    })
                    .child(if thread.archived {
                        format!("{} (archived)", thread.title)
                    } else {
                        thread.title.clone()
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .w(px(TABLE_AGENT))
                    .gap_1p5()
                    .items_center()
                    .text_color(muted)
                    .child(ui::provider_icon(thread.provider, cx).size_3p5())
                    .child(div().min_w_0().text_ellipsis().child(thread.model.clone())),
            )
            .child(
                h_flex()
                    .flex_none()
                    .w(px(TABLE_BAR + 48.))
                    .gap_2()
                    .items_center()
                    .child(
                        div().w(px(TABLE_BAR)).h(px(4.)).rounded_full().child(
                            div()
                                .h_full()
                                .w(relative(fill))
                                .rounded_full()
                                .bg(ui::provider_color(thread.provider, cx)),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_right()
                            .child(compact_tokens(thread.tokens)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(44.))
                    .text_right()
                    .text_color(muted)
                    .child(compact_time(thread.last_active_ms, input.now_ms)),
            )
    };
    let mut card = panel(cx)
        .flex_grow(1.0)
        .min_w(px(460.))
        .child(
            h_flex()
                .justify_between()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_sm().text_color(muted).child("tokens used")),
        )
        .child(
            h_flex()
                .mt_3()
                .pb_1p5()
                .gap_3()
                .child(head("Thread").flex_1())
                .child(head("Agent").flex_none().w(px(TABLE_AGENT)))
                .child(head("Tokens").flex_none().w(px(TABLE_BAR + 48.)))
                .child(head("Active").flex_none().w(px(44.)).text_right()),
        );
    if snapshot.threads.is_empty() {
        card = card.child(
            div()
                .pt_3()
                .border_t_1()
                .border_color(stroke)
                .text_sm()
                .text_color(muted)
                .child("No Newjen thread has used the current windows yet."),
        );
    }
    card.children(snapshot.threads.iter().map(table_row))
        .child(
            div()
                .pt_2()
                .border_t_1()
                .border_color(stroke)
                .text_xs()
                .text_color(muted)
                .child("Counts what Newjen's threads used. Other apps on the same account also count toward the limits."),
        )
        .into_any_element()
}

fn week_card(snapshot: &Snapshot, cx: &App) -> AnyElement {
    let muted = material(cx).glass_muted;
    let top = snapshot
        .days
        .iter()
        .map(DayBar::total)
        .max()
        .unwrap_or(0)
        .max(1);
    let legend = |provider: Provider| {
        h_flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(muted)
            .child(
                div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(ui::provider_color(provider, cx)),
            )
            .child(provider.presentation().label)
    };
    let height = |tokens: u64| {
        if tokens == 0 {
            0.0
        } else {
            (CHART_HEIGHT * tokens as f32 / top as f32).max(2.0)
        }
    };
    let bars = snapshot.days.iter().map(|day| {
        v_flex()
            .flex_1()
            .items_center()
            .gap_1p5()
            .child(
                v_flex()
                    .h(px(CHART_HEIGHT))
                    .w_full()
                    .max_w(px(40.))
                    .justify_end()
                    .gap(px(1.))
                    .child(
                        div()
                            .w_full()
                            .h(px(height(day.codex)))
                            .rounded_t_sm()
                            .bg(ui::provider_color(Provider::Codex, cx).opacity(0.85)),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(height(day.claude)))
                            .rounded_t_sm()
                            .bg(ui::provider_color(Provider::Claude, cx)),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(day.label.clone()))
    });
    let line = |label: &str, value: String| {
        h_flex()
            .justify_between()
            .gap_3()
            .text_sm()
            .child(div().flex_none().text_color(muted).child(label.to_string()))
            .child(
                div()
                    .min_w_0()
                    .text_ellipsis()
                    .font_weight(FontWeight::MEDIUM)
                    .child(value),
            )
    };
    panel(cx)
        .min_w(px(280.))
        .max_w(px(320.))
        .child(
            h_flex()
                .justify_between()
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Last 7 days"))
                .child(
                    h_flex()
                        .gap_3()
                        .child(legend(Provider::Claude))
                        .child(legend(Provider::Codex)),
                ),
        )
        .child(h_flex().mt_4().gap_1().items_end().children(bars))
        .child(
            v_flex()
                .mt_3()
                .gap_1p5()
                .child(line(
                    "Tokens this week",
                    compact_tokens(snapshot.week_tokens()),
                ))
                .child(line(
                    "Busiest thread",
                    snapshot
                        .busiest
                        .clone()
                        .unwrap_or_else(|| "None yet".into()),
                )),
        )
        .into_any_element()
}

const ICON_BUTTON: f32 = 26.0;
const TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

fn tone_ink(tone: Tone, cx: &App) -> Hsla {
    let colors = cx.theme().colors;
    match tone {
        Tone::Normal => colors.foreground.opacity(0.72),
        Tone::Warning => colors.warning,
        Tone::Limited => colors.danger,
    }
}

/// The row at the foot of the sidebar: the settings gear at the left, the usage ring at the
/// right. The gear opens Settings and the ring the Usage view.
pub fn footer(
    gauge: Option<&Gauge>,
    selected: bool,
    on_usage: OnPress,
    (settings_open, on_settings): (bool, OnPress),
    cx: &App,
) -> AnyElement {
    let gear = ui::icon_button("settings", ui::IconName::Settings, "Settings", cx)
        .tooltip_with_action("Settings", &OpenSettings, Some(crate::keys::CONTEXT))
        .tooltip_show_delay(TOOLTIP_DELAY)
        .debug_selector(|| "settings".to_string())
        .size(px(ICON_BUTTON))
        .selected(settings_open)
        .on_click(move |_, window, cx| on_settings(window, cx));
    div()
        .flex_none()
        .px_2()
        .pb_2()
        .child(
            h_flex()
                .h(px(32.))
                .px(px(2.))
                .items_center()
                .justify_between()
                .child(gear)
                .child(cluster(gauge, selected, on_usage, cx)),
        )
        .into_any_element()
}

fn cluster(gauge: Option<&Gauge>, selected: bool, on_usage: OnPress, cx: &App) -> AnyElement {
    let material = material(cx);
    let colors = cx.theme().colors;
    let look = ui::GaugeLook {
        fraction: gauge.map_or(0.0, |gauge| gauge.fraction),
        tick: gauge.and_then(|gauge| gauge.pace),
        ink: tone_ink(gauge.map_or(Tone::Normal, |gauge| gauge.tone), cx),
        track: colors.foreground.opacity(0.12),
        tick_ink: colors.foreground.opacity(0.55),
    };
    let label = gauge.and_then(|gauge| {
        Some(ui::GaugeLabel {
            provider: gauge.provider,
            text: gauge.label.clone()?.into(),
        })
    });
    h_flex()
        .id("usage-row")
        .debug_selector(|| "usage-row".to_string())
        .h(px(ICON_BUTTON))
        .px(px(5.))
        .items_center()
        .rounded(px(6.))
        .when(selected, |row| row.bg(material.glass_selected))
        .hover(|row| row.bg(material.glass_hover))
        .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Usage").build(window, cx))
        .tooltip_show_delay(TOOLTIP_DELAY)
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
            window.prevent_default()
        })
        .on_click(move |_, window, cx| on_usage(window, cx))
        .child(ui::UsageGauge::new("usage-gauge", look, label))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use newjen_core::agent::TokenCounts;
    use newjen_core::providers::{Account, CatalogSource, ModelCatalog};

    const NOW: i64 = 1_000_000;

    fn window(
        key: &str,
        used: Option<f32>,
        resets: Option<i64>,
        length: Option<i64>,
        status: UsageStatus,
    ) -> UsageWindow {
        UsageWindow {
            key: key.into(),
            label: key.into(),
            used_fraction: used,
            resets_at_unix_seconds: resets,
            length_seconds: length,
            status,
        }
    }

    fn usage(windows: Vec<UsageWindow>) -> ProviderUsage {
        ProviderUsage {
            windows,
            billing: Billing::Subscription,
            plan: Some("max".into()),
            updated_at_unix_seconds: Some(NOW - 120),
            ..ProviderUsage::empty(Provider::Claude)
        }
    }

    fn ready(provider: Provider, billing: Billing) -> ProviderStatus {
        ProviderStatus {
            provider,
            availability: Availability::Ready {
                binary: "cli".into(),
                version: "1.0.0".into(),
                account: Account {
                    billing,
                    plan: None,
                    email: None,
                },
                catalog: ModelCatalog {
                    source: CatalogSource::CliAliases,
                    models: Vec::new(),
                },
                rate_limits: None,
                warnings: Vec::new(),
            },
        }
    }

    fn five_hours(used: f32, left: i64, status: UsageStatus) -> UsageWindow {
        window(
            "five_hour",
            Some(used),
            Some(NOW + left),
            Some(18_000),
            status,
        )
    }

    fn codex(windows: Vec<UsageWindow>) -> ProviderUsage {
        ProviderUsage {
            provider: Provider::Codex,
            ..usage(windows)
        }
    }

    const BOTH: [Provider; 2] = [Provider::Claude, Provider::Codex];

    #[test]
    fn the_ring_is_plain_and_unlabelled_while_a_limit_is_comfortable() {
        let claude = usage(vec![five_hours(0.32, 12_600, UsageStatus::Ok)]);
        let ring = gauge(&[claude], &BOTH, NOW).unwrap();
        assert_eq!(ring.provider, Provider::Claude);
        assert_eq!(ring.tone, Tone::Normal);
        assert_eq!(ring.label, None);
        assert!((ring.fraction - 0.32).abs() < 1e-6);
        assert!((ring.pace.unwrap() - 0.3).abs() < 1e-6);
    }

    #[test]
    fn the_ring_turns_amber_and_says_when_the_pace_hits_the_limit() {
        let claude = usage(vec![five_hours(0.74, 9_000, UsageStatus::Ok)]);
        let ring = gauge(&[claude], &BOTH, NOW).unwrap();
        assert_eq!(ring.tone, Tone::Warning);
        let out = NOW + 9_000 - 18_000 + (9_000.0_f64 / 0.74).round() as i64;
        assert_eq!(
            ring.label,
            Some(format!("Limit by {}", reset_time(out, NOW)))
        );
        assert!((ring.fraction - 0.74).abs() < 1e-6);
    }

    #[test]
    fn near_the_limit_with_no_pace_to_project_the_ring_names_the_share_used() {
        let codex = codex(vec![window(
            "codex:primary",
            Some(0.93),
            Some(NOW + 600),
            None,
            UsageStatus::Ok,
        )]);
        let ring = gauge(&[codex], &BOTH, NOW).unwrap();
        assert_eq!(ring.tone, Tone::Warning);
        assert_eq!(ring.label.as_deref(), Some("93% used"));
        assert_eq!(ring.pace, None);
    }

    #[test]
    fn at_the_limit_the_ring_is_full_red_and_says_when_it_resets() {
        let claude = usage(vec![five_hours(1.0, 3_600, UsageStatus::Limited)]);
        let ring = gauge(&[claude], &BOTH, NOW).unwrap();
        assert_eq!(ring.tone, Tone::Limited);
        assert_eq!(ring.fraction, 1.0);
        assert_eq!(
            ring.label,
            Some(format!("Resets {}", reset_time(NOW + 3_600, NOW)))
        );
        let rejected = usage(vec![window(
            "usage",
            None,
            Some(NOW + 3_600),
            None,
            UsageStatus::Limited,
        )]);
        let ring = gauge(&[rejected], &BOTH, NOW).unwrap();
        assert_eq!((ring.tone, ring.fraction), (Tone::Limited, 1.0));
        let unknown = usage(vec![window(
            "usage",
            None,
            None,
            None,
            UsageStatus::Limited,
        )]);
        assert_eq!(
            gauge(&[unknown], &BOTH, NOW).unwrap().label.as_deref(),
            Some("Limit reached")
        );
    }

    #[test]
    fn the_tightest_limit_of_the_providers_in_use_wins() {
        let claude = usage(vec![five_hours(0.32, 12_600, UsageStatus::Ok)]);
        let codex = codex(vec![window(
            "codex:primary",
            Some(0.97),
            Some(NOW + 600),
            Some(18_000),
            UsageStatus::Warning,
        )]);
        let usages = [claude, codex];
        let both = gauge(&usages, &BOTH, NOW).unwrap();
        assert_eq!((both.provider, both.tone), (Provider::Codex, Tone::Warning));
        let calm_but_fuller = usage(vec![
            five_hours(0.2, 12_600, UsageStatus::Ok),
            window(
                "week",
                Some(0.6),
                Some(NOW + 300_000),
                None,
                UsageStatus::Ok,
            ),
        ]);
        assert!((gauge(&[calm_but_fuller], &BOTH, NOW).unwrap().fraction - 0.6).abs() < 1e-6);
    }

    #[test]
    fn a_provider_not_in_use_never_raises_the_ring() {
        let claude = usage(vec![five_hours(0.32, 12_600, UsageStatus::Ok)]);
        let codex = codex(vec![window(
            "codex:primary",
            Some(1.0),
            Some(NOW + 600),
            Some(18_000),
            UsageStatus::Limited,
        )]);
        let usages = [claude, codex];
        let ring = gauge(&usages, &[Provider::Claude], NOW).unwrap();
        assert_eq!((ring.provider, ring.tone), (Provider::Claude, Tone::Normal));
        assert_eq!(gauge(&usages, &[], NOW), None);
    }

    #[test]
    fn windows_that_say_nothing_are_left_out() {
        let past = usage(vec![five_hours(0.99, -1, UsageStatus::Warning)]);
        assert_eq!(gauge(&[past], &BOTH, NOW), None);
        let bare = usage(vec![window("5h", None, None, None, UsageStatus::Ok)]);
        assert_eq!(gauge(&[bare], &BOTH, NOW), None);
        let paid = ProviderUsage {
            billing: Billing::ApiKey,
            ..usage(vec![five_hours(1.0, 600, UsageStatus::Limited)])
        };
        assert_eq!(gauge(&[paid], &BOTH, NOW), None);
        assert_eq!(gauge(&[], &BOTH, NOW), None);
    }

    #[test]
    fn a_provider_is_in_use_with_a_working_thread_on_screen_or_with_recent_turns() {
        use Provider::{Claude, Codex};
        let threads = [
            (Claude, Status::Done),
            (Codex, Status::Working),
            (Codex, Status::Waiting),
            (Claude, Status::Idle),
        ];
        let none = |_: &[Provider]| Vec::new();
        assert_eq!(providers_in_use(threads, None, none), vec![Codex]);
        assert_eq!(providers_in_use([], Some(Claude), none), vec![Claude]);
        assert_eq!(
            providers_in_use(threads, Some(Claude), |_| vec![Claude]),
            vec![Codex, Claude]
        );
        assert_eq!(
            providers_in_use(threads, None, |known| {
                assert_eq!(known, [Codex]);
                vec![Claude, Codex]
            }),
            vec![Codex, Claude]
        );
        assert!(providers_in_use([(Claude, Status::Failed)], None, none).is_empty());
    }

    #[test]
    fn turn_usage_counts_from_the_start_of_the_current_window() {
        let claude = usage(vec![five_hours(0.4, 3_600, UsageStatus::Ok)]);
        let codex = codex(vec![window(
            "x",
            Some(0.1),
            Some(NOW + 5),
            None,
            UsageStatus::Ok,
        )]);
        let started = NOW + 3_600 - 18_000;
        let used =
            providers_with_turns(&[claude.clone(), codex.clone()], NOW, |provider, since| {
                provider == Provider::Claude && since == started
            });
        assert_eq!(used, vec![Provider::Claude]);
        let unmeasured = providers_with_turns(&[claude, codex], NOW, |_, _| true);
        assert_eq!(unmeasured, vec![Provider::Claude]);
    }

    struct Harness {
        gauge: Option<Gauge>,
        opened: Rc<std::cell::Cell<u32>>,
        settings: Rc<std::cell::Cell<u32>>,
    }

    impl gpui::Render for Harness {
        fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
            let opened = self.opened.clone();
            let settings = self.settings.clone();
            footer(
                self.gauge.as_ref(),
                false,
                Rc::new(move |_, _| opened.set(opened.get() + 1)),
                (false, Rc::new(move |_, _| settings.set(settings.get() + 1))),
                cx,
            )
        }
    }

    fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    }

    #[gpui::test]
    fn the_footer_has_a_gear_for_settings_and_a_ring_that_opens_usage(
        cx: &mut gpui::TestAppContext,
    ) {
        let settings = Rc::new(std::cell::Cell::new(0));
        cx.update(gpui_kit::init);
        let opened = Rc::new(std::cell::Cell::new(0));
        let (_, cx) = cx.add_window_view(|_, _| Harness {
            gauge: None,
            opened: opened.clone(),
            settings: settings.clone(),
        });
        assert!(cx.debug_bounds("usage-label").is_none());
        click(cx, "settings");
        assert_eq!((settings.get(), opened.get()), (1, 0));
        click(cx, "usage-row");
        assert_eq!((settings.get(), opened.get()), (1, 1));
    }

    #[gpui::test]
    fn the_footer_names_the_limit_only_when_one_is_near(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let near = gauge(
            &[usage(vec![five_hours(1.0, 3_600, UsageStatus::Limited)])],
            &BOTH,
            NOW,
        );
        assert!(near.as_ref().unwrap().label.is_some());
        let (_, cx) = cx.add_window_view(|_, _| Harness {
            gauge: near,
            opened: Rc::default(),
            settings: Rc::default(),
        });
        assert!(cx.debug_bounds("usage-label").is_some());
        let calm = gauge(
            &[usage(vec![five_hours(0.32, 12_600, UsageStatus::Ok)])],
            &BOTH,
            NOW,
        );
        let (_, cx) = cx.add_window_view(|_, _| Harness {
            gauge: calm,
            opened: Rc::default(),
            settings: Rc::default(),
        });
        assert!(cx.debug_bounds("usage-label").is_none());
    }

    fn show(view: &gpui::Entity<Harness>, gauge: Option<Gauge>, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, cx| {
            view.update(cx, |harness, cx| {
                harness.gauge = gauge;
                cx.notify();
            })
        });
        cx.run_until_parked();
    }

    fn label_width(cx: &mut gpui::VisualTestContext) -> Option<f32> {
        cx.debug_bounds("usage-label")
            .map(|bounds| f32::from(bounds.size.width))
    }

    fn pass(cx: &mut gpui::VisualTestContext, millis: u64) {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(millis));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }

    fn near() -> Option<Gauge> {
        gauge(
            &[usage(vec![five_hours(1.0, 3_600, UsageStatus::Limited)])],
            &BOTH,
            NOW,
        )
    }

    #[gpui::test]
    fn the_label_slides_out_of_the_ring_and_back(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_kit::init);
        let (view, cx) = cx.add_window_view(|_, _| Harness {
            gauge: None,
            opened: Rc::default(),
            settings: Rc::default(),
        });
        assert_eq!(label_width(cx), None);
        show(&view, near(), cx);
        let starting = label_width(cx).unwrap();
        pass(cx, 100);
        let sliding = label_width(cx).unwrap();
        pass(cx, 300);
        let full = label_width(cx).unwrap();
        assert!(
            starting < sliding && sliding < full,
            "{starting} {sliding} {full}"
        );
        show(&view, None, cx);
        pass(cx, 100);
        assert!(label_width(cx).unwrap() < full);
        pass(cx, 300);
        assert_eq!(label_width(cx), None);
    }

    #[gpui::test]
    fn with_reduce_motion_the_label_keeps_its_width_and_only_fades(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
        let (view, cx) = cx.add_window_view(|_, _| Harness {
            gauge: None,
            opened: Rc::default(),
            settings: Rc::default(),
        });
        show(&view, near(), cx);
        pass(cx, 100);
        let full = label_width(cx).unwrap();
        pass(cx, 300);
        assert_eq!(label_width(cx), Some(full));
        show(&view, None, cx);
        pass(cx, 100);
        assert_eq!(label_width(cx), Some(full));
        pass(cx, 300);
        assert_eq!(label_width(cx), None);
    }

    #[test]
    fn a_window_says_when_it_resets_or_that_it_did() {
        let live = window("5h", Some(0.5), Some(NOW + 3600), None, UsageStatus::Ok);
        assert!(reset_line(&live, NOW).unwrap().starts_with("resets "));
        let past = window("5h", Some(0.5), Some(NOW - 5), None, UsageStatus::Ok);
        assert_eq!(reset_line(&past, NOW).as_deref(), Some("reset"));
        let unknown = window("5h", None, None, None, UsageStatus::Ok);
        assert_eq!(reset_line(&unknown, NOW), None);
    }

    #[test]
    fn a_large_credit_balance_reads_in_whole_credits_with_separators() {
        let credits = |balance| UsageCredits {
            has_credits: true,
            unlimited: false,
            balance,
        };
        let line = |balance| credits_line(&credits(balance));
        assert_eq!(
            line(Some(66388.61)),
            ("Credits".into(), Some("66,389".into()))
        );
        assert_eq!(line(Some(100.0)), ("Credits".into(), Some("100".into())));
        assert_eq!(line(Some(12.5)), ("Credits".into(), Some("12.50".into())));
        assert_eq!(line(Some(120.0)), ("Credits".into(), Some("120".into())));
        assert_eq!(line(None), ("Credits".into(), Some("available".into())));
        let none = UsageCredits {
            has_credits: false,
            unlimited: false,
            balance: None,
        };
        assert_eq!(credits_line(&none), ("No credits".into(), None));
    }

    #[test]
    fn under_pace_says_how_long_is_left_in_a_short_window() {
        let model = window_model(
            Provider::Claude,
            &five_hours(0.1, 110 * 60, UsageStatus::Ok),
            NOW,
        );
        assert_eq!(model.tone, Tone::Normal);
        assert_eq!(
            model.line.as_deref(),
            Some("Under pace. 1 h 50 min left in this window.")
        );
        assert_eq!(model.used, Some(("10%".into(), 0.1)));
        assert!(model.mark.unwrap() > 0.6);
        let week = window_model(
            Provider::Claude,
            &window(
                "seven_day",
                Some(0.12),
                Some(NOW + 400_000),
                Some(604_800),
                UsageStatus::Ok,
            ),
            NOW,
        );
        assert_eq!(week.line.as_deref(), Some("Under pace."));
    }

    #[test]
    fn running_ahead_turns_amber_and_names_when_it_runs_out() {
        let model = window_model(
            Provider::Codex,
            &five_hours(0.82, 3 * 3600, UsageStatus::Ok),
            NOW,
        );
        assert_eq!(model.tone, Tone::Warning);
        let line = model.line.unwrap();
        assert!(
            line.starts_with("At this pace it runs out around "),
            "{line}"
        );
        assert!(line.ends_with(" before it resets."), "{line}");
    }

    #[test]
    fn a_fresh_window_ahead_of_pace_stays_plain_until_ninety_percent() {
        let fresh = |used| {
            window(
                "seven_day",
                Some(used),
                Some(NOW + 604_800),
                Some(604_800),
                UsageStatus::Ok,
            )
        };
        let model = window_model(Provider::Claude, &fresh(0.5), NOW);
        assert_eq!(model.tone, Tone::Normal);
        assert_eq!(model.line.as_deref(), Some("Ahead of an even pace."));
        let nearly = window_model(Provider::Claude, &fresh(0.9), NOW);
        assert_eq!(nearly.tone, Tone::Warning);
    }

    #[test]
    fn a_limit_is_red_and_never_resumes_on_its_own() {
        let model = window_model(
            Provider::Codex,
            &five_hours(1.0, 3600, UsageStatus::Limited),
            NOW,
        );
        assert_eq!(model.tone, Tone::Limited);
        assert_eq!(model.used.as_ref().unwrap().0, "100%");
        assert_eq!(
            model.line.as_deref(),
            Some(
                "Codex's limit is reached. You choose when to continue; Newjen never resumes on its own."
            )
        );
    }

    #[test]
    fn a_window_past_its_reset_drops_its_percentage() {
        let model = window_model(
            Provider::Claude,
            &five_hours(0.9, -10, UsageStatus::Warning),
            NOW,
        );
        assert_eq!(model.used, None);
        assert_eq!(model.mark, None);
        assert_eq!(model.tone, Tone::Normal);
        assert_eq!(model.reset.as_deref(), Some("reset"));
        assert!(
            model
                .line
                .unwrap()
                .starts_with("Reset since the last report")
        );
    }

    #[test]
    fn a_window_with_no_utilisation_has_no_bar_or_pace() {
        let model = window_model(
            Provider::Claude,
            &window("usage", None, Some(NOW + 60), None, UsageStatus::Ok),
            NOW,
        );
        assert_eq!(model.used, None);
        assert_eq!(model.mark, None);
        assert_eq!(model.line, None);
    }

    #[test]
    fn the_card_follows_what_the_cli_and_the_board_know() {
        let signed_out = ProviderStatus {
            provider: Provider::Claude,
            availability: Availability::SignedOut {
                binary: "cli".into(),
                version: "1".into(),
                hint: String::new(),
            },
        };
        let card = card_model(Provider::Claude, Some(&signed_out), None, None, NOW);
        assert_eq!(card.body, CardBody::SignedOut { command: "claude" });
        assert_eq!(card.meta.as_deref(), Some("Not signed in"));
        let card = card_model(Provider::Codex, Some(&signed_out), None, None, NOW);
        assert_eq!(
            card.body,
            CardBody::SignedOut {
                command: "codex login"
            }
        );

        let card = card_model(Provider::Claude, None, None, None, NOW);
        assert_eq!(card.body, CardBody::Checking);

        let quiet = ProviderUsage {
            windows: Vec::new(),
            ..usage(Vec::new())
        };
        let card = card_model(
            Provider::Claude,
            Some(&ready(Provider::Claude, Billing::Subscription)),
            Some(&quiet),
            None,
            NOW,
        );
        assert_eq!(card.body, CardBody::Quiet);
        assert_eq!(card.plan.as_deref(), Some("Max"));
        assert_eq!(card.meta.as_deref(), Some("Subscription · as of 2 min ago"));

        let keyed = ProviderUsage {
            billing: Billing::ApiKey,
            plan: None,
            ..usage(vec![five_hours(0.5, 100, UsageStatus::Ok)])
        };
        let card = card_model(
            Provider::Claude,
            Some(&ready(Provider::Claude, Billing::ApiKey)),
            Some(&keyed),
            Some((48.2, 6.75)),
            NOW,
        );
        assert!(card.meta_warning);
        assert_eq!(card.meta.as_deref(), Some("API key · ANTHROPIC_API_KEY"));
        assert_eq!(
            card.body,
            CardBody::ApiKey {
                variable: "ANTHROPIC_API_KEY",
                charged: Some(Charged {
                    week: "$48.20".into(),
                    today: "$6.75".into()
                })
            }
        );

        let codex_keyed = ProviderUsage {
            provider: Provider::Codex,
            ..keyed.clone()
        };
        let card = card_model(
            Provider::Codex,
            Some(&ready(Provider::Codex, Billing::ApiKey)),
            Some(&codex_keyed),
            Some((0.0, 0.0)),
            NOW,
        );
        assert_eq!(
            card.body,
            CardBody::ApiKey {
                variable: "OPENAI_API_KEY",
                charged: None
            }
        );

        let planned = usage(vec![five_hours(0.38, 6600, UsageStatus::Ok)]);
        let card = card_model(Provider::Claude, None, Some(&planned), None, NOW);
        let CardBody::Plan { windows, credits } = card.body else {
            panic!("a plan card")
        };
        assert_eq!(windows.len(), 1);
        assert_eq!(credits, None);
    }

    #[test]
    fn the_table_counts_from_the_shortest_window_and_a_reset_one_counts_a_length_back() {
        let mixed = usage(vec![
            window(
                "seven_day",
                Some(0.1),
                Some(NOW + 300_000),
                Some(604_800),
                UsageStatus::Ok,
            ),
            five_hours(0.4, 3600, UsageStatus::Ok),
        ]);
        let (picked, started) = current_window(&mixed, NOW).unwrap();
        assert_eq!(picked.key, "five_hour");
        assert_eq!(started, NOW + 3600 - 18_000);

        let reset = usage(vec![
            window(
                "seven_day",
                Some(0.1),
                Some(NOW + 300_000),
                Some(604_800),
                UsageStatus::Ok,
            ),
            five_hours(0.4, -5, UsageStatus::Ok),
        ]);
        let (picked, started) = current_window(&reset, NOW).unwrap();
        assert_eq!(picked.key, "five_hour");
        assert_eq!(started, NOW - 18_000);
        let unmeasured = usage(vec![window(
            "x",
            Some(0.1),
            Some(NOW + 5),
            None,
            UsageStatus::Ok,
        )]);
        assert!(current_window(&unmeasured, NOW).is_none());
    }

    #[test]
    fn days_group_both_providers_and_the_last_is_today() {
        let counts = |total: u64| TokenCounts {
            input: total,
            ..TokenCounts::default()
        };
        let day = |start: i64, provider, total| DailyTokens {
            day_start_unix_seconds: start,
            provider,
            tokens: counts(total),
        };
        let bars = day_bars(&[
            day(0, Provider::Claude, 100),
            day(0, Provider::Codex, 40),
            day(86_400, Provider::Claude, 0),
            day(86_400, Provider::Codex, 7),
        ]);
        assert_eq!(bars.len(), 2);
        assert_eq!((bars[0].claude, bars[0].codex), (100, 40));
        assert_eq!((bars[1].claude, bars[1].codex), (0, 7));
        assert_eq!(bars[1].label, "Today");
        assert!(WEEKDAYS.contains(&bars[0].label.as_str()));
        assert_eq!(
            Snapshot {
                days: bars,
                ..Snapshot::default()
            }
            .week_tokens(),
            147
        );
    }

    #[test]
    fn weekdays_follow_the_local_calendar() {
        let midnight = 1_793_664_000;
        let offset = i64::from(local_utc_offset_seconds(midnight));
        let local_midnight = midnight - offset;
        let names: Vec<_> = (0..7)
            .map(|d| weekday(local_midnight + d * 86_400))
            .collect();
        let first = WEEKDAYS.iter().position(|d| *d == names[0]).unwrap();
        for (ix, name) in names.iter().enumerate() {
            assert_eq!(*name, WEEKDAYS[(first + ix) % 7]);
        }
    }
}
