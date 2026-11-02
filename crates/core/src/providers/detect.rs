//! Usage-free CLI discovery. Callers choose when to refresh; this module never starts a turn.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::env::{self, Cli};
use crate::probe::{self, Probe};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

impl Provider {
    pub(crate) fn cli(self) -> Cli {
        match self {
            Self::Claude => Cli::Claude,
            Self::Codex => Cli::Codex,
        }
    }
    fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            Self::Claude => 5,
            Self::Codex => 15,
        })
    }
    fn versions(self) -> (&'static str, &'static str) {
        match self {
            Self::Claude => ("2.1.280", "2.1.316"),
            Self::Codex => ("0.156.0", "0.166.0"),
        }
    }
    /// The environment variable that makes the CLI bill an API account instead of the plan.
    pub fn api_key_var(self) -> &'static str {
        match self {
            Self::Claude => "ANTHROPIC_API_KEY",
            Self::Codex => "OPENAI_API_KEY",
        }
    }
    fn install_hint(self) -> String {
        match self {
            Self::Claude => "Install the `claude` CLI: https://code.claude.com/docs/en/setup",
            Self::Codex => "Install the Codex CLI: https://developers.openai.com/codex/cli/. Newjen never uses the copy bundled in the ChatGPT desktop app.",
        }.into()
    }
    fn login_hint(self) -> String {
        match self {
            Self::Claude => "Run `claude` in your own terminal to sign in, then refresh.",
            Self::Codex => "Run `codex login` in your own terminal to sign in, then refresh.",
        }
        .into()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStatus {
    pub provider: Provider,
    pub availability: Availability,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Availability {
    NotInstalled {
        hint: String,
    },
    Broken {
        binary: PathBuf,
        error: String,
    },
    SignedOut {
        binary: PathBuf,
        version: String,
        hint: String,
    },
    Ready {
        binary: PathBuf,
        version: String,
        account: Account,
        catalog: ModelCatalog,
        rate_limits: Option<Box<RateLimits>>,
        warnings: Vec<Warning>,
    },
}

/// What pays for the CLI's turns, as the CLI reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    Subscription,
    ApiKey,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub billing: Billing,
    pub plan: Option<String>,
    /// Personal data: only display on request.
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    ApiKeyEnvironment {
        variable: &'static str,
    },
    VersionBelowTested {
        found: String,
        tested: &'static str,
    },
    VersionBelowFloor {
        found: String,
        minimum: &'static str,
    },
    UnrecognizedVersion,
    /// Claude documents syntax, but offers no usage-free per-account capability query.
    ClaudeCapabilitiesUnverified,
    RateLimitsUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ModelSelection {
    /// Omit model/effort/speed overrides and let the CLI choose.
    CliDefault,
    Model {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogSource {
    CliAliases,
    AccountModels,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    pub source: CatalogSource,
    /// `ModelSelection::CliDefault` is always an additional choice, even with no listed models.
    pub models: Vec<ModelOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub description: Option<String>,
    pub id: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub service_tiers: Vec<ServiceTier>,
    pub default_service_tier: Option<String>,
    pub fast: Option<ServiceTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServiceTier {
    pub id: String,
    #[serde(rename = "name")]
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RateLimits {
    #[serde(rename = "rateLimits")]
    pub rate_limits: RateLimitSnapshot,
    #[serde(rename = "rateLimitsByLimitId")]
    pub by_limit_id: Option<BTreeMap<String, RateLimitSnapshot>>,
    #[serde(rename = "ordinaryUsageAllowed")]
    pub ordinary_usage_allowed: Option<bool>,
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
    #[serde(rename = "rateLimitResetCredits")]
    pub reset_credits: Option<ResetCredits>,
    #[serde(rename = "rateLimitUpsell")]
    pub upsell: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
pub struct RateLimitSnapshot {
    #[serde(rename = "limitId")]
    pub limit_id: Option<String>,
    #[serde(rename = "limitName")]
    pub limit_name: Option<String>,
    #[serde(rename = "normalModelSlug")]
    pub normal_model_slug: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    pub credits: Option<Credits>,
    #[serde(rename = "individualLimit")]
    pub individual_limit: Option<SpendLimit>,
    #[serde(rename = "spendControlReached")]
    pub spend_control_reached: Option<bool>,
    #[serde(rename = "planType")]
    pub plan: Option<String>,
    #[serde(rename = "rateLimitReachedType")]
    pub reached_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct RateLimitWindow {
    #[serde(rename = "usedPercent")]
    pub used_percent: i32,
    #[serde(rename = "windowDurationMins")]
    pub window_duration_mins: Option<i64>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct Credits {
    #[serde(rename = "hasCredits")]
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct SpendLimit {
    pub limit: String,
    pub used: String,
    #[serde(rename = "remainingPercent")]
    pub remaining_percent: i32,
    #[serde(rename = "resetsAt")]
    pub resets_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResetCredits {
    #[serde(rename = "availableCount")]
    pub available_count: i64,
    pub credits: Option<Vec<ResetCredit>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResetCredit {
    pub id: String,
    #[serde(rename = "resetType")]
    pub reset_type: String,
    pub status: String,
    #[serde(rename = "grantedAt")]
    pub granted_at: i64,
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<i64>,
    pub title: Option<String>,
    pub description: Option<String>,
}

/// Blocking, bounded probe. Run off the UI thread. PATH resolution has a separate one-time budget.
pub fn detect(provider: Provider) -> ProviderStatus {
    let binary = env::find_cli(provider.cli());
    let api_key_set = std::env::var_os(provider.api_key_var()).is_some_and(|v| !v.is_empty());
    detect_at(provider, binary, provider.timeout(), api_key_set)
}

/// Both probes run concurrently; the result order is always Claude, Codex.
pub fn detect_all() -> Vec<ProviderStatus> {
    thread::scope(|scope| {
        let workers = [Provider::Claude, Provider::Codex].map(|provider| {
            (
                provider,
                thread::Builder::new().spawn_scoped(scope, move || detect(provider)),
            )
        });
        workers
            .into_iter()
            .map(|(provider, task)| {
                let result = match task {
                    Ok(task) => task
                        .join()
                        .map_err(|_| "The CLI detection worker stopped unexpectedly"),
                    Err(_) => Err("Could not start the CLI detection worker"),
                };
                result.unwrap_or_else(|error| ProviderStatus {
                    provider,
                    availability: Availability::Broken {
                        binary: provider.cli().name().into(),
                        error: error.into(),
                    },
                })
            })
            .collect()
    })
}

fn detect_at(
    provider: Provider,
    binary: PathBuf,
    timeout: Duration,
    api_key_set: bool,
) -> ProviderStatus {
    let availability =
        if binary == std::path::Path::new(provider.cli().name()) && !env::is_executable(&binary) {
            Availability::NotInstalled {
                hint: provider.install_hint(),
            }
        } else {
            let deadline = Instant::now() + timeout;
            match probe_provider(provider, &binary, deadline, api_key_set) {
                Ok(availability) => availability,
                Err(error) => Availability::Broken {
                    binary,
                    error: format!("Could not check {}: {error:#}", provider.cli().name()),
                },
            }
        };
    let status = ProviderStatus {
        provider,
        availability,
    };
    crate::usage::record_status(&status);
    status
}

fn command(binary: &PathBuf) -> Command {
    let mut command = Command::new(binary);
    env::configure(&mut command);
    command
}

fn probe_provider(
    provider: Provider,
    binary: &PathBuf,
    deadline: Instant,
    api_key_set: bool,
) -> Result<Availability> {
    let mut warnings = Vec::new();
    if api_key_set {
        warnings.push(Warning::ApiKeyEnvironment {
            variable: provider.api_key_var(),
        });
    }
    match provider {
        Provider::Claude => {
            let (status, bytes) = probe::output(command(binary).arg("--version"), deadline)?;
            if !status.success() {
                bail!("The CLI version check exited unsuccessfully ({status})");
            }
            let version =
                parse_version(std::str::from_utf8(&bytes).context("The CLI version is not text")?)?;
            version_warnings(provider, &version, &mut warnings);
            let (status, bytes) =
                probe::output(command(binary).args(["auth", "status", "--json"]), deadline)?;
            let auth: ClaudeAuth = serde_json::from_slice(&bytes)
                .context("The CLI returned an unreadable sign-in report")?;
            // `auth status` exits 1 for signed-out users, with a valid JSON report.
            if !status.success() && (status.code() != Some(1) || auth.logged_in) {
                bail!("The CLI sign-in check exited unsuccessfully ({status})");
            }
            if !auth.logged_in {
                return Ok(signed_out(provider, binary, version));
            }
            warnings.push(Warning::ClaudeCapabilitiesUnverified);
            Ok(Availability::Ready {
                binary: binary.clone(),
                version,
                account: auth.into(),
                catalog: claude_catalog(),
                rate_limits: None,
                warnings,
            })
        }
        Provider::Codex => {
            let mut rpc = Rpc {
                probe: Probe::spawn(command(binary).arg("app-server"), deadline)?,
                next_id: 0,
            };
            let init: Initialize = rpc.request("initialize", Value::Null)?;
            rpc.probe.send(&super::rpc::initialized())?;
            let version = parse_version(&init.user_agent)?;
            version_warnings(provider, &version, &mut warnings);
            let auth: CodexAuth = rpc.request("account/read", json!({"refreshToken":false}))?;
            let account = match auth.account {
                Some(account) => account.into(),
                None if auth.requires_openai_auth => {
                    return Ok(signed_out(provider, binary, version));
                }
                None => Account {
                    billing: Billing::Unknown,
                    plan: None,
                    email: None,
                },
            };
            let mut models = Vec::new();
            let mut cursor = None;
            let mut seen = Vec::new();
            loop {
                let page: ModelPage = rpc.request("model/list", json!({"cursor":cursor}))?;
                models.extend(
                    page.data
                        .into_iter()
                        .filter(|model| !model.hidden)
                        .map(ModelOption::from),
                );
                let Some(next) = page.next_cursor else {
                    break;
                };
                if seen.contains(&next) || seen.len() >= 100 {
                    bail!("The CLI model list repeated or exceeded its page limit");
                }
                seen.push(next.clone());
                cursor = Some(next);
            }
            let rate_limits = match rpc.request("account/rateLimits/read", json!({})) {
                Ok(limits) => Some(Box::new(limits)),
                Err(_) => {
                    warnings.push(Warning::RateLimitsUnavailable);
                    None
                }
            };
            Ok(Availability::Ready {
                binary: binary.clone(),
                version,
                account,
                catalog: ModelCatalog {
                    source: CatalogSource::AccountModels,
                    models,
                },
                rate_limits,
                warnings,
            })
        }
    }
}

fn signed_out(provider: Provider, binary: &std::path::Path, version: String) -> Availability {
    Availability::SignedOut {
        binary: binary.into(),
        version,
        hint: provider.login_hint(),
    }
}

fn parse_version(text: &str) -> Result<String> {
    text.split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|part| version_tuple(part).is_some())
        .map(str::to_owned)
        .context("The CLI did not report a recognizable version")
}

fn version_tuple(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.');
    let tuple = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(tuple)
}

fn version_warnings(provider: Provider, version: &str, warnings: &mut Vec<Warning>) {
    let (floor, tested) = provider.versions();
    let Some(found) = version_tuple(version) else {
        warnings.push(Warning::UnrecognizedVersion);
        return;
    };
    if Some(found) < version_tuple(tested) {
        warnings.push(Warning::VersionBelowTested {
            found: version.into(),
            tested,
        });
    }
    if Some(found) < version_tuple(floor) {
        warnings.push(Warning::VersionBelowFloor {
            found: version.into(),
            minimum: floor,
        });
    }
}

#[derive(Deserialize)]
struct ClaudeAuth {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
    #[serde(rename = "authMethod")]
    auth_method: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
    email: Option<String>,
}

impl From<ClaudeAuth> for Account {
    fn from(auth: ClaudeAuth) -> Self {
        let billing = match auth.auth_method.as_deref() {
            Some("claude.ai") => Billing::Subscription,
            Some("api_key" | "apiKey" | "api-key") => Billing::ApiKey,
            _ => Billing::Unknown,
        };
        Self {
            billing,
            plan: auth.subscription_type,
            email: auth.email,
        }
    }
}

fn claude_catalog() -> ModelCatalog {
    ModelCatalog {
        source: CatalogSource::CliAliases,
        models: [
            ("fable", "Fable"),
            ("opus", "Opus"),
            ("sonnet", "Sonnet"),
            ("haiku", "Haiku"),
        ]
        .into_iter()
        .map(|(id, label)| ModelOption {
            id: id.into(),
            label: label.into(),
            description: None,
            efforts: ["low", "medium", "high", "xhigh", "max"]
                .map(str::to_owned)
                .into(),
            default_effort: None,
            fast: Some(ServiceTier {
                id: "fast".into(),
                label: "Fast (CLI checks availability)".into(),
                description:
                    "May bill extra usage. The Claude CLI checks whether your account can use it."
                        .into(),
            }),
            service_tiers: Vec::new(),
            default_service_tier: None,
        })
        .collect(),
    }
}

#[derive(Deserialize)]
pub(crate) struct Initialize {
    #[serde(rename = "userAgent")]
    pub(crate) user_agent: String,
}

#[derive(Deserialize)]
pub(crate) struct CodexAuth {
    pub(crate) account: Option<CodexAccount>,
    #[serde(rename = "requiresOpenaiAuth")]
    pub(crate) requires_openai_auth: bool,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
pub(crate) enum CodexAccount {
    #[serde(rename = "apiKey")]
    ApiKey,
    #[serde(rename = "chatgpt")]
    Chatgpt {
        email: Option<String>,
        #[serde(rename = "planType")]
        plan: String,
    },
    #[serde(rename = "amazonBedrock")]
    AmazonBedrock,
    #[serde(rename = "gcp")]
    Gcp,
}

impl From<CodexAccount> for Account {
    fn from(account: CodexAccount) -> Self {
        match account {
            CodexAccount::ApiKey => Self {
                billing: Billing::ApiKey,
                plan: None,
                email: None,
            },
            CodexAccount::Chatgpt { email, plan } => Self {
                billing: Billing::Subscription,
                plan: Some(plan),
                email,
            },
            CodexAccount::AmazonBedrock | CodexAccount::Gcp => Self {
                billing: Billing::Unknown,
                plan: None,
                email: None,
            },
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct ModelPage {
    pub(crate) data: Vec<CodexModel>,
    #[serde(rename = "nextCursor")]
    pub(crate) next_cursor: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CodexModel {
    model: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(rename = "displayName")]
    display_name: String,
    pub(crate) hidden: bool,
    #[serde(rename = "supportedReasoningEfforts")]
    efforts: Vec<Effort>,
    #[serde(rename = "defaultReasoningEffort")]
    default_effort: String,
    #[serde(default, rename = "serviceTiers")]
    service_tiers: Vec<ServiceTier>,
    #[serde(rename = "defaultServiceTier")]
    default_service_tier: Option<String>,
}

#[derive(Deserialize)]
struct Effort {
    #[serde(rename = "reasoningEffort")]
    effort: String,
}

impl From<CodexModel> for ModelOption {
    fn from(model: CodexModel) -> Self {
        let fast = model
            .service_tiers
            .iter()
            .find(|tier| matches!(tier.id.as_str(), "priority" | "fast"))
            .cloned();
        Self {
            id: model.model,
            label: model.display_name,
            description: model.description,
            efforts: model
                .efforts
                .into_iter()
                .map(|effort| effort.effort)
                .collect(),
            default_effort: Some(model.default_effort),
            service_tiers: model.service_tiers,
            default_service_tier: model.default_service_tier,
            fast,
        }
    }
}

struct Rpc {
    probe: Probe,
    next_id: u64,
}

impl Rpc {
    fn request<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<T> {
        self.next_id += 1;
        let id = super::rpc::RpcId::Integer(self.next_id as i64);
        let message = if method == "initialize" {
            super::rpc::initialize(&id)
        } else {
            super::rpc::request(&id, method, params)
        };
        self.probe.send(&message)?;
        loop {
            let message: Value = serde_json::from_slice(&self.probe.line()?)
                .context("The CLI returned unreadable JSON")?;
            if message.get("id") == Some(&json!(self.next_id)) && message.get("method").is_none() {
                if message.get("error").is_some() {
                    bail!("The CLI could not complete {method}");
                }
                return serde_json::from_value(
                    message
                        .get("result")
                        .context("The CLI reply has no result")?
                        .clone(),
                )
                .with_context(|| format!("The CLI returned an unreadable {method} report"));
            }
            if message.get("method").is_some() && message.get("id").is_some() {
                bail!("The CLI requested an unexpected action during read-only detection");
            }
        }
    }
}

#[cfg(test)]
mod tests;

impl Provider {
    pub fn presentation(self) -> crate::agent::AgentPresentation {
        crate::session::saved_presentation(self.id())
    }
    pub fn account_fallback(self) -> &'static str {
        match self {
            Self::Claude => "Claude subscription",
            Self::Codex => "ChatGPT account",
        }
    }
}
impl ProviderStatus {
    pub fn limit_event(&self) -> Option<crate::agent::RateLimitEvent> {
        let Availability::Ready {
            rate_limits: Some(report),
            ..
        } = &self.availability
        else {
            return None;
        };
        Some(super::codex::account_limit(report))
    }
    pub fn clears_limit(&self) -> bool {
        self.provider == Provider::Codex
            && matches!(&self.availability, Availability::Ready { rate_limits: Some(report), .. } if report.ordinary_usage_allowed != Some(false) && report.rate_limits.primary.as_ref().is_some_and(|w| w.used_percent < 100) && report.rate_limits.secondary.as_ref().is_none_or(|w| w.used_percent < 100))
            && matches!(
                self.limit_event(),
                Some(crate::agent::RateLimitEvent::Updated(snapshot)) if snapshot.windows.iter().all(|w| w.used_percent.is_none_or(|p| p < 100.0))
            )
    }
}
