use crate::agent::{AgentConfig, EffortSelection, Speed};
use crate::providers::{Availability, ModelOption, ModelSelection, ProviderStatus};

pub fn selected_model<'a>(
    config: &AgentConfig,
    status: &'a ProviderStatus,
) -> Option<&'a ModelOption> {
    let (ModelSelection::Model { id }, Availability::Ready { catalog, .. }) =
        (&config.model, &status.availability)
    else {
        return None;
    };
    catalog.models.iter().find(|model| &model.id == id)
}

pub fn agent_label(config: &AgentConfig, status: Option<&ProviderStatus>) -> String {
    let label = config.provider.presentation().label;
    match &config.model {
        ModelSelection::CliDefault => label.into(),
        ModelSelection::Model { id } => format!(
            "{label} · {}",
            status
                .and_then(|s| selected_model(config, s))
                .map_or(id.as_str(), |m| m.label.as_str())
        ),
    }
}

pub fn effort_label(effort: &EffortSelection) -> String {
    match effort {
        EffortSelection::CliDefault => "Default".into(),
        EffortSelection::Chosen(value) => match value.as_str() {
            "xhigh" => "XHigh".into(),
            _ => {
                let mut chars = value.chars();
                chars
                    .next()
                    .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            }
        },
    }
}

pub fn frozen_label(config: Option<&AgentConfig>, status: Option<&ProviderStatus>) -> String {
    let Some(config) = config else {
        return "Unavailable provider".into();
    };
    let mut label = agent_label(config, status);
    if config.effort != EffortSelection::CliDefault {
        label.push_str(&format!(" · {}", effort_label(&config.effort)));
    }
    label
}

pub fn restore_choice(
    saved: Option<AgentConfig>,
    statuses: &[ProviderStatus],
) -> Option<AgentConfig> {
    if let Some(config) = saved
        && statuses.iter().any(|s| config.validate(s).is_ok())
    {
        return Some(config);
    }
    statuses
        .iter()
        .find(|s| matches!(s.availability, Availability::Ready { .. }))
        .map(|s| AgentConfig::cli_default(s.provider))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRow {
    pub selection: ModelSelection,
    pub label: String,
    pub detail: Option<String>,
}

pub fn model_rows(status: &ProviderStatus) -> Vec<ModelRow> {
    let Availability::Ready { catalog, .. } = &status.availability else {
        return vec![];
    };
    std::iter::once(ModelRow {
        selection: ModelSelection::CliDefault,
        label: "Default".into(),
        detail: Some(format!(
            "Whatever your {} settings pick",
            status.provider.presentation().label
        )),
    })
    .chain(catalog.models.iter().map(|m| ModelRow {
        selection: ModelSelection::Model { id: m.id.clone() },
        label: m.label.clone(),
        detail: m.description.clone(),
    }))
    .collect()
}

pub fn effort_rows(
    config: &AgentConfig,
    status: &ProviderStatus,
) -> Vec<(EffortSelection, Option<String>)> {
    let Some(model) = selected_model(config, status) else {
        return vec![(
            EffortSelection::CliDefault,
            Some("Pick a model to choose its effort and speed".into()),
        )];
    };
    std::iter::once((
        EffortSelection::CliDefault,
        model.default_effort.as_ref().map(|e| {
            format!(
                "{} · {}'s default for this model",
                effort_label(&EffortSelection::Chosen(e.clone())),
                config.provider.presentation().label
            )
        }),
    ))
    .chain(
        model
            .efforts
            .iter()
            .map(|e| (EffortSelection::Chosen(e.clone()), None)),
    )
    .collect()
}

pub fn fast_detail<'a>(config: &AgentConfig, status: &'a ProviderStatus) -> Option<&'a str> {
    selected_model(config, status)?
        .fast
        .as_ref()
        .map(|f| f.description.as_str())
}

pub fn is_fast(config: Option<&AgentConfig>) -> bool {
    config.is_some_and(|c| c.speed == Speed::Fast)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::*;
    fn status() -> ProviderStatus {
        ProviderStatus {
            provider: Provider::Codex,
            availability: Availability::Ready {
                binary: "codex".into(),
                version: "0.166.0".into(),
                account: Account {
                    billing: Billing::Subscription,
                    plan: None,
                    email: None,
                },
                catalog: ModelCatalog {
                    source: CatalogSource::AccountModels,
                    models: vec![ModelOption {
                        id: "sol".into(),
                        label: "GPT-6.1-Sol".into(),
                        description: Some("Workhorse".into()),
                        efforts: vec!["high".into(), "xhigh".into(), "minimal".into()],
                        default_effort: Some("high".into()),
                        service_tiers: vec![],
                        default_service_tier: None,
                        fast: Some(ServiceTier {
                            id: "fast".into(),
                            label: "Fast".into(),
                            description: "2x speed, increased usage".into(),
                        }),
                    }],
                },
                rate_limits: None,
                warnings: vec![],
            },
        }
    }
    #[test]
    fn labels_and_rows_follow_the_cli_catalog() {
        let status = status();
        let mut config = AgentConfig::cli_default(status.provider);
        assert_eq!(agent_label(&config, Some(&status)), "Codex");
        assert_eq!(effort_rows(&config, &status).len(), 1);
        let rows = model_rows(&status);
        assert_eq!(rows[0].label, "Default");
        assert_eq!(rows[1].detail.as_deref(), Some("Workhorse"));
        config.model = rows[1].selection.clone();
        config.effort = EffortSelection::Chosen("xhigh".into());
        config.speed = Speed::Fast;
        assert_eq!(agent_label(&config, Some(&status)), "Codex · GPT-6.1-Sol");
        assert_eq!(
            frozen_label(Some(&config), Some(&status)),
            "Codex · GPT-6.1-Sol · XHigh"
        );
        assert_eq!(frozen_label(None, None), "Unavailable provider");
        assert_eq!(effort_rows(&config, &status).len(), 4);
        assert_eq!(
            effort_label(&EffortSelection::Chosen("minimal".into())),
            "Minimal"
        );
        assert_eq!(
            fast_detail(&config, &status),
            Some("2x speed, increased usage")
        );
    }
    #[test]
    fn restores_only_choices_the_current_detection_can_run() {
        let status = status();
        let default = AgentConfig::cli_default(status.provider);
        assert_eq!(
            restore_choice(None, std::slice::from_ref(&status)),
            Some(default.clone())
        );
        let mut chosen = default.clone();
        chosen.model = ModelSelection::Model { id: "sol".into() };
        assert_eq!(
            restore_choice(Some(chosen.clone()), std::slice::from_ref(&status)),
            Some(chosen)
        );
        let mut missing = default.clone();
        missing.model = ModelSelection::Model {
            id: "removed".into(),
        };
        assert_eq!(restore_choice(Some(missing), &[status]), Some(default));
        assert_eq!(restore_choice(None, &[]), None);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Ready,
    Warning,
    Disabled,
    Danger,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountView {
    pub health: Health,
    pub plan: String,
    pub email: Option<String>,
    pub version: Option<String>,
    pub api_billing: bool,
    pub callouts: Vec<String>,
    pub unavailable: Option<String>,
    pub error: Option<String>,
}

pub fn account_view(status: &ProviderStatus) -> AccountView {
    use crate::providers::{Billing, Warning};
    let label = status.provider.presentation().label;
    let mut view = AccountView {
        health: Health::Disabled,
        plan: String::new(),
        email: None,
        version: None,
        api_billing: false,
        callouts: vec![],
        unavailable: None,
        error: None,
    };
    match &status.availability {
        Availability::NotInstalled { hint } => {
            view.unavailable = Some(format!("{label} isn't installed. {hint}"))
        }
        Availability::SignedOut { version, hint, .. } => {
            view.version = Some(version.clone());
            view.unavailable = Some(format!("Signed out of {label}. {hint}"));
        }
        Availability::Broken { error, .. } => {
            view.health = Health::Danger;
            view.unavailable = Some(format!("Couldn't check {label}"));
            view.error = Some(error.clone());
        }
        Availability::Ready {
            version,
            account,
            warnings,
            ..
        } => {
            view.health = if warnings.is_empty() {
                Health::Ready
            } else {
                Health::Warning
            };
            view.version = Some(version.clone());
            view.email = account.email.clone();
            view.api_billing = account.billing == Billing::ApiKey
                || warnings
                    .iter()
                    .any(|w| matches!(w, Warning::ApiKeyEnvironment { .. }));
            view.plan = if view.api_billing {
                "API key · billed per token".into()
            } else {
                account
                    .plan
                    .as_ref()
                    .filter(|p| !p.trim().is_empty())
                    .map(|p| {
                        format!(
                            "{} plan · your subscription",
                            effort_label(&EffortSelection::Chosen(p.clone()))
                        )
                    })
                    .unwrap_or_else(|| status.provider.account_fallback().into())
            };
            for warning in warnings {
                match warning {
                    Warning::ApiKeyEnvironment { variable } => view.callouts.push(format!("{variable} is set. {label} bills your API account. Unset it and Refresh to use your plan.")),
                    Warning::VersionBelowTested { found, tested } => view.callouts.push(format!("{label} {found} is older than Newjen is tested with. Some steps may not show. Update to {tested} or newer.")),
                    Warning::VersionBelowFloor { found, minimum } => view.callouts.push(format!("{label} {found} is below the supported version. Update to {minimum} or newer.")),
                    Warning::UnrecognizedVersion => view.callouts.push("Couldn't recognize the CLI version.".into()),
                    Warning::ClaudeCapabilitiesUnverified | Warning::RateLimitsUnavailable => {},
                }
            }
            if let Some(crate::agent::RateLimitEvent::Warning(snapshot)) = status.limit_event() {
                let mut detail =
                    "Plan limit reached. Credits on this account now pay for turns.".to_owned();
                if let Some(reset) = limit_reset(&snapshot) {
                    detail.push_str(&format!(
                        " Plan resets {}.",
                        reset_time(reset, crate::store::now_ms() / 1000)
                    ));
                }
                if let Availability::Ready {
                    rate_limits: Some(report),
                    ..
                } = &status.availability
                    && let Some(weekly) = &report.rate_limits.secondary
                {
                    detail.push_str(&format!(" Weekly {}% used.", weekly.used_percent));
                }
                view.callouts.push(detail);
                view.health = Health::Warning;
            }
        }
    }
    view
}

pub fn limit_reset(snapshot: &crate::agent::LimitSnapshot) -> Option<i64> {
    snapshot
        .windows
        .iter()
        .filter(|w| w.used_percent.is_none_or(|p| p >= 100.0))
        .filter_map(|w| w.resets_at_unix_seconds)
        .max()
}

pub fn paused_until(event: Option<&crate::agent::RateLimitEvent>, now: i64) -> Option<i64> {
    let Some(crate::agent::RateLimitEvent::Rejected(snapshot)) = event else {
        return None;
    };
    limit_reset(snapshot).filter(|reset| *reset > now)
}

pub fn reset_time(reset: i64, now: i64) -> String {
    fn local(seconds: i64) -> Option<libc::tm> {
        let seconds: libc::time_t = seconds;
        let mut value = std::mem::MaybeUninit::uninit();
        if unsafe { libc::localtime_r(&seconds, value.as_mut_ptr()) }.is_null() {
            None
        } else {
            Some(unsafe { value.assume_init() })
        }
    }
    let (Some(reset), Some(now)) = (local(reset), local(now)) else {
        return "the reported reset time".into();
    };
    let day = if reset.tm_year == now.tm_year {
        reset.tm_yday - now.tm_yday
    } else if reset.tm_year == now.tm_year + 1
        && reset.tm_yday == 0
        && now.tm_mon == 11
        && now.tm_mday == 31
    {
        1
    } else {
        2
    };
    let weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .get(usize::try_from(reset.tm_wday).unwrap_or(0))
        .unwrap_or(&"");
    format_reset(day, weekday, reset.tm_hour, reset.tm_min)
}
fn format_reset(day: i32, weekday: &str, hour: i32, minute: i32) -> String {
    let clock = format!(
        "{}:{minute:02}{}",
        if hour % 12 == 0 { 12 } else { hour % 12 },
        if hour < 12 { "am" } else { "pm" }
    );
    match day {
        0 => clock,
        1 => format!("tomorrow {clock}"),
        _ => format!("{weekday} {clock}"),
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use crate::agent::{LimitSnapshot, LimitWindow, RateLimitEvent};
    #[test]
    fn rejected_known_resets_pause_until_reset_but_warnings_and_unknown_resets_do_not() {
        let mut snapshot = LimitSnapshot {
            bucket: "plan".into(),
            windows: vec![
                LimitWindow {
                    label: "5-hour".into(),
                    used_percent: Some(100.),
                    resets_at_unix_seconds: Some(120),
                },
                LimitWindow {
                    label: "weekly".into(),
                    used_percent: Some(23.),
                    resets_at_unix_seconds: Some(9999),
                },
            ],
            explanation: String::new(),
        };
        assert_eq!(
            paused_until(Some(&RateLimitEvent::Rejected(snapshot.clone())), 100),
            Some(120)
        );
        assert_eq!(
            paused_until(Some(&RateLimitEvent::Rejected(snapshot.clone())), 120),
            None
        );
        assert_eq!(
            paused_until(Some(&RateLimitEvent::Warning(snapshot.clone())), 100),
            None
        );
        snapshot.windows[0].resets_at_unix_seconds = None;
        assert_eq!(
            paused_until(Some(&RateLimitEvent::Rejected(snapshot.clone())), 100),
            None,
            "an unused weekly window cannot supply a refused primary window's missing reset"
        );
        for window in &mut snapshot.windows {
            window.resets_at_unix_seconds = None;
        }
        assert_eq!(
            paused_until(Some(&RateLimitEvent::Rejected(snapshot)), 100),
            None
        );
    }
    #[test]
    fn reset_labels_are_local_calendar_times_without_countdowns() {
        assert_eq!(format_reset(0, "Thu", 2, 20), "2:20am");
        assert_eq!(format_reset(1, "Thu", 2, 20), "tomorrow 2:20am");
        assert_eq!(format_reset(3, "Thu", 9, 14), "Thu 9:14am");
        assert_eq!(format_reset(0, "Thu", 0, 0), "12:00am");
        assert_eq!(format_reset(0, "Thu", 12, 0), "12:00pm");
    }
}
