//! What each CLI reports about plan usage, in one provider-neutral shape.
//!
//! Nothing here asks a CLI or an API for anything: it only reshapes what the CLIs already
//! printed (Claude's `rate_limit_event`, Codex's `account/rateLimits/*`). A bar and a percentage
//! exist only where the CLI reported a utilisation.

use std::sync::{Mutex, OnceLock, PoisonError};

use serde_json::Value;

use crate::providers::{
    Availability, Billing, Provider, ProviderStatus, RateLimitSnapshot, RateLimits,
};

const WARNING_FRACTION: f32 = 0.9;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum UsageStatus {
    Ok,
    Warning,
    Limited,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageWindow {
    /// Stable identity used to replace a window with its newer report.
    pub key: String,
    pub label: String,
    /// 0.0 to 1.0, only when the CLI reported a utilisation.
    pub used_fraction: Option<f32>,
    pub resets_at_unix_seconds: Option<i64>,
    /// How long the window lasts, when the CLI says (or the window's name does).
    #[serde(default)]
    pub length_seconds: Option<i64>,
    pub status: UsageStatus,
}

/// Whether use is running ahead of or behind an even spend of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    Under,
    On,
    /// `runs_out_at` is when the window empties at the current rate, Unix seconds, only if
    /// that comes before it resets.
    Ahead {
        runs_out_at: Option<i64>,
    },
}

/// Use within this many points of the elapsed share of the window counts as on pace.
const PACE_BAND: f64 = 0.05;
const PACE_EPSILON: f64 = 1e-4;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderUsage {
    pub provider: Provider,
    /// As the CLI names it (`max`, `plus`); see [`ProviderUsage::plan_label`].
    pub plan: Option<String>,
    /// What pays for this provider's turns. A plan's windows mean little under `ApiKey`.
    #[serde(default)]
    pub billing: Billing,
    pub windows: Vec<UsageWindow>,
    pub credits: Option<UsageCredits>,
    /// Set by [`UsageBoard`] when it stores the report; adapters leave it `None`.
    pub updated_at_unix_seconds: Option<i64>,
}

impl UsageWindow {
    pub fn expired(&self, now: i64) -> bool {
        self.resets_at_unix_seconds
            .is_some_and(|reset| reset <= now)
    }

    /// When the window began, Unix seconds.
    pub fn started_at(&self) -> Option<i64> {
        Some(self.resets_at_unix_seconds? - self.length_seconds.filter(|length| *length > 0)?)
    }

    /// How much of the window has passed, 0.0 to 1.0.
    pub fn elapsed_fraction(&self, now: i64) -> Option<f32> {
        let length = self.length_seconds.filter(|length| *length > 0)?;
        let start = self.started_at()?;
        Some(((now - start) as f64 / length as f64).clamp(0.0, 1.0) as f32)
    }

    /// Use against the share of the window that has passed. `None` when either is unknown or the
    /// window has already reset.
    pub fn pace(&self, now: i64) -> Option<Pace> {
        if self.expired(now) {
            return None;
        }
        let used = f64::from(self.used_fraction?);
        let elapsed = f64::from(self.elapsed_fraction(now)?);
        let gap = used - elapsed;
        Some(if gap < -(PACE_BAND + PACE_EPSILON) {
            Pace::Under
        } else if gap <= PACE_BAND + PACE_EPSILON {
            Pace::On
        } else {
            Pace::Ahead {
                runs_out_at: self.runs_out_at(now, used),
            }
        })
    }

    fn runs_out_at(&self, now: i64, used: f64) -> Option<i64> {
        let start = self.started_at()?;
        let reset = self.resets_at_unix_seconds?;
        let elapsed = now - start;
        if used <= 0.0 || elapsed <= 0 {
            return None;
        }
        let empty = start + (elapsed as f64 / used).round() as i64;
        (empty < reset).then_some(empty)
    }

    /// `5 hours · 38% · resets 4:10pm`. A window past its reset time drops the old percentage.
    pub fn summary(&self, now: i64) -> String {
        let mut parts = vec![self.label.clone()];
        if self.expired(now) {
            parts.push("reset".into());
        } else {
            if let Some(fraction) = self.used_fraction {
                parts.push(format!("{}%", (fraction * 100.0).round() as i64));
            }
            if let Some(reset) = self.resets_at_unix_seconds {
                parts.push(format!("resets {}", crate::picker::reset_time(reset, now)));
            }
        }
        parts.join(" · ")
    }
}

impl UsageCredits {
    pub fn summary(&self) -> String {
        if self.unlimited {
            "Credits unlimited".into()
        } else if let Some(balance) = self.balance {
            if balance.fract() == 0.0 {
                format!("Credits {balance:.0}")
            } else {
                format!("Credits {balance:.2}")
            }
        } else if self.has_credits {
            "Credits available".into()
        } else {
            "No credits".into()
        }
    }
}

impl ProviderUsage {
    pub fn empty(provider: Provider) -> Self {
        Self {
            provider,
            plan: None,
            billing: Billing::Unknown,
            windows: Vec::new(),
            credits: None,
            updated_at_unix_seconds: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.plan.is_none() && self.windows.is_empty() && self.credits.is_none()
    }

    pub fn plan_label(&self) -> Option<String> {
        let plan = self.plan.as_deref()?;
        let mut chars = plan.chars();
        let first = chars.next()?;
        Some(first.to_uppercase().chain(chars).collect())
    }

    /// The most severe window status.
    pub fn status(&self) -> UsageStatus {
        self.windows
            .iter()
            .map(|w| w.status)
            .max()
            .unwrap_or(UsageStatus::Ok)
    }

    /// Newer values win: same-key windows are replaced, new ones appended, and a missing plan
    /// or credits report never erases a known one.
    pub fn absorb(&mut self, newer: &ProviderUsage) {
        if newer.plan.is_some() {
            self.plan = newer.plan.clone();
        }
        if newer.credits.is_some() {
            self.credits = newer.credits.clone();
        }
        if newer.billing != Billing::Unknown {
            self.billing = newer.billing;
        }
        if newer.updated_at_unix_seconds.is_some() {
            self.updated_at_unix_seconds = newer.updated_at_unix_seconds;
        }
        for window in &newer.windows {
            match self.windows.iter_mut().find(|w| w.key == window.key) {
                Some(existing) => *existing = window.clone(),
                None => self.windows.push(window.clone()),
            }
        }
    }

    /// From the `rate_limit_info` object of a Claude `rate_limit_event`.
    ///
    /// Recorded: `unifiedWindows.{five_hour,seven_day}.{utilization,resetsAt,status}` with
    /// status `allowed`, and a bare `{status:"rejected", rateLimitType, resetsAt}`. Other keys
    /// and statuses are handled generically but have never been recorded.
    pub fn from_claude_rate_limit(info: &Value) -> Option<Self> {
        let top_status = info.get("status").and_then(Value::as_str);
        top_status?;
        let top_status = top_status.and_then(claude_status);
        let top_type = info.get("rateLimitType").and_then(Value::as_str);
        let mut windows = Vec::new();
        if let Some(unified) = info.get("unifiedWindows").and_then(Value::as_object) {
            for (key, window) in unified {
                windows.push(window_from(
                    key,
                    claude_label(key),
                    WindowReport {
                        utilization: window["utilization"].as_f64(),
                        resets_at: window["resetsAt"].as_i64(),
                        length_seconds: claude_length(key),
                        status: window["status"].as_str().and_then(claude_status),
                    },
                ));
            }
        }
        if windows.is_empty() {
            let key = top_type.unwrap_or("usage");
            windows.push(window_from(
                key,
                claude_label(key),
                WindowReport {
                    utilization: None,
                    resets_at: info["resetsAt"].as_i64(),
                    length_seconds: claude_length(key),
                    status: top_status,
                },
            ));
        } else if let (Some(status), Some(kind)) = (top_status, top_type)
            && let Some(window) = windows.iter_mut().find(|w| w.key == kind)
        {
            window.status = window.status.max(status);
        }
        let mut usage = Self::empty(Provider::Claude);
        usage.windows = windows;
        Some(usage)
    }

    /// From one Codex rate-limit bucket (`account/rateLimits/read` or `.../updated`).
    pub fn from_codex_snapshot(bucket: &str, snapshot: &RateLimitSnapshot) -> Self {
        let main = matches!(bucket, "codex" | "default");
        let prefix = || {
            snapshot
                .limit_name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or(bucket)
                .to_owned()
        };
        let named = |label: String| {
            if main {
                label
            } else {
                format!("{} · {label}", prefix())
            }
        };
        let mut usage = Self::empty(Provider::Codex);
        for (slot, fallback, window) in [
            ("primary", "Primary limit", &snapshot.primary),
            ("secondary", "Secondary limit", &snapshot.secondary),
        ] {
            if let Some(window) = window {
                let minutes = window.window_duration_mins.filter(|m| *m > 0);
                usage.windows.push(window_from(
                    &format!("{bucket}:{slot}"),
                    named(
                        minutes
                            .map(duration_label)
                            .unwrap_or_else(|| fallback.into()),
                    ),
                    WindowReport {
                        utilization: Some(f64::from(window.used_percent) / 100.0),
                        resets_at: window.resets_at,
                        length_seconds: minutes.map(|m| m * 60),
                        status: None,
                    },
                ));
            }
        }
        if let Some(spend) = &snapshot.individual_limit {
            usage.windows.push(window_from(
                &format!("{bucket}:spend"),
                named("Spend".into()),
                WindowReport {
                    utilization: Some(f64::from(100 - spend.remaining_percent) / 100.0),
                    resets_at: Some(spend.resets_at),
                    length_seconds: None,
                    status: None,
                },
            ));
        }
        usage.credits = snapshot.credits.as_ref().map(|credits| UsageCredits {
            has_credits: credits.has_credits,
            unlimited: credits.unlimited,
            balance: credits
                .balance
                .as_deref()
                .and_then(|b| b.trim().parse::<f64>().ok())
                .filter(|b| b.is_finite()),
        });
        usage.plan = snapshot.plan.clone().filter(|p| !p.trim().is_empty());
        usage
    }

    /// From a whole `account/rateLimits/read` result.
    pub fn from_codex_report(report: &RateLimits) -> Self {
        let main = report.rate_limits.limit_id.as_deref().unwrap_or("default");
        let mut usage = Self::from_codex_snapshot(main, &report.rate_limits);
        for (id, snapshot) in report.by_limit_id.iter().flatten() {
            let other = Self::from_codex_snapshot(id, snapshot);
            let credits = usage.credits.clone();
            usage.absorb(&other);
            if credits.is_some() {
                usage.credits = credits;
            }
        }
        usage
    }
}

/// What a CLI reported about one window.
struct WindowReport {
    utilization: Option<f64>,
    resets_at: Option<i64>,
    length_seconds: Option<i64>,
    status: Option<UsageStatus>,
}

fn window_from(key: &str, label: String, report: WindowReport) -> UsageWindow {
    let WindowReport {
        utilization,
        resets_at,
        length_seconds,
        status: reported,
    } = report;
    let used_fraction = utilization
        .filter(|n| n.is_finite())
        .map(|n| n.clamp(0.0, 1.0) as f32);
    let derived = match used_fraction {
        Some(f) if f >= 1.0 => UsageStatus::Limited,
        Some(f) if f >= WARNING_FRACTION => UsageStatus::Warning,
        _ => UsageStatus::Ok,
    };
    UsageWindow {
        key: key.into(),
        label,
        used_fraction,
        resets_at_unix_seconds: resets_at.filter(|n| *n >= 0),
        length_seconds: length_seconds.filter(|n| *n > 0),
        status: derived.max(reported.unwrap_or(UsageStatus::Ok)),
    }
}

fn claude_status(status: &str) -> Option<UsageStatus> {
    match status {
        "allowed" => Some(UsageStatus::Ok),
        "allowed_warning" | "warning" => Some(UsageStatus::Warning),
        "rejected" => Some(UsageStatus::Limited),
        _ => None,
    }
}

fn claude_length(key: &str) -> Option<i64> {
    match key {
        "five_hour" => Some(5 * 3600),
        "seven_day" => Some(7 * 86_400),
        _ => None,
    }
}

fn claude_label(key: &str) -> String {
    match key {
        "five_hour" => "5 hours".into(),
        "seven_day" => "Week".into(),
        other => {
            let text = other.replace('_', " ");
            let mut chars = text.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_else(|| "Usage".into())
        }
    }
}

fn duration_label(minutes: i64) -> String {
    match minutes {
        300 => "5 hours".into(),
        1440 => "Day".into(),
        10080 => "Week".into(),
        60 => "1 hour".into(),
        m if m % 10080 == 0 => format!("{} weeks", m / 10080),
        m if m % 1440 == 0 => format!("{} days", m / 1440),
        m if m % 60 == 0 => format!("{} hours", m / 60),
        m => format!("{m} minutes"),
    }
}

#[derive(Debug, Default)]
struct Entry {
    usage: Option<ProviderUsage>,
    account: Option<String>,
}

/// The latest report per provider.
#[derive(Debug, Default)]
pub struct UsageBoard {
    claude: Entry,
    codex: Entry,
}

impl UsageBoard {
    pub fn new() -> Self {
        Self::default()
    }

    fn entry(&mut self, provider: Provider) -> &mut Entry {
        match provider {
            Provider::Claude => &mut self.claude,
            Provider::Codex => &mut self.codex,
        }
    }

    pub fn get(&self, provider: Provider) -> Option<&ProviderUsage> {
        match provider {
            Provider::Claude => self.claude.usage.as_ref(),
            Provider::Codex => self.codex.usage.as_ref(),
        }
        .filter(|usage| !usage.is_empty())
    }

    pub fn record(&mut self, usage: &ProviderUsage, now: i64) {
        let entry = self.entry(usage.provider);
        let stored = entry
            .usage
            .get_or_insert_with(|| ProviderUsage::empty(usage.provider));
        stored.absorb(usage);
        stored.updated_at_unix_seconds = Some(now);
    }

    /// Plan and account from a detection result, plus Codex's usage-free limit read.
    /// A signed-out or missing CLI clears the provider; a broken probe keeps what is known.
    pub fn record_status(&mut self, status: &ProviderStatus, now: i64) {
        match &status.availability {
            Availability::Ready {
                account,
                rate_limits,
                ..
            } => {
                let entry = self.entry(status.provider);
                if entry.account.is_some() && entry.account != account.email {
                    entry.usage = None;
                }
                entry.account = account.email.clone();
                let stored = entry
                    .usage
                    .get_or_insert_with(|| ProviderUsage::empty(status.provider));
                stored.billing = account.billing;
                stored.plan = match account.billing {
                    Billing::ApiKey => None,
                    _ => account
                        .plan
                        .clone()
                        .filter(|p| !p.trim().is_empty() && p != "unknown"),
                };
                stored.updated_at_unix_seconds = Some(now);
                if let Some(report) = rate_limits {
                    stored.absorb(&ProviderUsage::from_codex_report(report));
                }
            }
            Availability::NotInstalled { .. } | Availability::SignedOut { .. } => {
                *self.entry(status.provider) = Entry::default();
            }
            Availability::Broken { .. } => {}
        }
    }
}

fn board() -> &'static Mutex<UsageBoard> {
    static BOARD: OnceLock<Mutex<UsageBoard>> = OnceLock::new();
    BOARD.get_or_init(Mutex::default)
}

fn now() -> i64 {
    crate::store::now_ms() / 1000
}

/// The latest report for `provider` seen by this process.
pub fn latest(provider: Provider) -> Option<ProviderUsage> {
    board()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(provider)
        .cloned()
}

pub(crate) fn record(usage: &ProviderUsage) {
    board()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .record(usage, now());
}

pub(crate) fn record_status(status: &ProviderStatus) {
    board()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .record_status(status, now());
}

#[cfg(test)]
mod tests;
