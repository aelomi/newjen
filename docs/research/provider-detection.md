# Headless provider detection (2026-10-31)

`newjen-core` now exposes `env::{Cli, resolved_path, find_cli}` and
`providers::{detect, detect_all, ProviderStatus, Availability}`. Detection starts
no conversation, sends no prompt, and reads no credential files. No dependencies,
UI, storage changes, or Codex session adapter were added.

## Data and caller contract

```rust
pub fn resolved_path() -> &'static std::ffi::OsStr;
pub fn find_cli(cli: Cli) -> std::path::PathBuf;
pub fn detect(provider: Provider) -> ProviderStatus;
pub fn detect_all() -> Vec<ProviderStatus>;
```

`Availability` is `NotInstalled`, `Broken`, `SignedOut`, or `Ready`. Only `Ready`
has an `Account`, `ModelCatalog`, optional boxed Codex `RateLimits`, and warnings.
This avoids contradictory flags such as signed out with a usable catalog.
`Account` keeps billing source, optional plan, and optional email. Email is
personal data for an explicit details request, not a default UI label.

The model picker should always include `ModelSelection::CliDefault`: omit all
model/effort/speed overrides. Catalog models carry CLI identifiers, labels,
efforts, optional default effort, service tiers, default tier, and optional fast
tier. Codex identifiers use the schema's `model` field (the selectable value),
not its separate catalog row `id`. Hidden rows are skipped; pages are followed.

`CatalogSource::AccountModels` comes from Codex's live `model/list` response.
`CatalogSource::CliAliases` describes Claude CLI syntax, **not confirmed account
entitlements**. Its aliases are fable, opus, sonnet and haiku; efforts are low,
medium, high, xhigh and max. No usage-free command confirms which alias/effort
combinations an account can actually use. `ClaudeCapabilitiesUnverified` makes
this limitation explicit; the eventual session must let the CLI validate them.
The help lists fable, opus and sonnet as examples, not an exhaustive list; haiku
is the requested supported alias. No Claude default effort is guessed.

A rate-limit read failure leaves Codex ready with `RateLimitsUnavailable`, rather
than confusing sign-in with quota availability. `ordinary_usage_allowed` is
preserved as an optional authoritative backend flag: callers must not infer
permission to continue from percentages or reset times. Reset credits are read,
never redeemed. Third-party billing reports (Bedrock/GCP or unrecognized Claude
auth methods) are `Billing::Unknown`, never labeled as subscription billing.

Probes block, so call them off the UI thread. `detect_all` runs both providers
concurrently. The Claude budget is 5 seconds total; Codex is 15 seconds total,
including initialization and pagination. Initial PATH resolution adds a separate
one-time budget of up to 5 seconds for the login shell and 1 for launchctl.
All probe pipes use bounded, nonblocking I/O. Dropping a probe kills its own
process group and reaps the child; no app-server stays running. Stderr is discarded
so arbitrary CLI diagnostics cannot copy account data or tokens into errors.
Refresh cadence belongs to the caller.

## Environment

PATH is cached once with `OnceLock`: `$SHELL -ilc` with UUID-delimited PATH output,
then `/bin/launchctl getenv PATH`, then inherited PATH. Known install directories
are appended without duplicates. Binary overrides `NEWJEN_CLAUDE` and
`NEWJEN_CODEX` take precedence. Detection and Claude sessions receive this PATH;
explicit session test/environment overrides still take precedence.

Only PATH is imported from shell initialization; other environment variables are
inherited from Newjen. API-key warnings check that actual child environment, not
credentials or shell dotfiles. Values are never read into provider status.
`ANTHROPIC_API_KEY` and `OPENAI_API_KEY` produce warnings without overriding the
billing source the CLI itself reports.

The native Codex executable behind the npm wrapper was inspected with `strings`
(codex-cli 0.166.0). It contains `CODEX_THREAD_ID` next to `exec_command`/`PATH`,
`CODEX_TOOL_CALL_ID`, `CODEX_SESSION_ID`, and the pair
`CODEX_SANDBOX_NETWORK_DISABLED1CODEX_SANDBOX`. It also contains
`CODEX_INTERNAL_ORIGINATOR_OVERRIDE`. The unified-exec/process-manager strings additionally place
`CODEX_PERMISSION_PROFILE`, `CODEX_VERSION`,
`CODEX_APPLY_PATCH_PRESERVE_LINE_ENDINGS`, and `CODEX_PLUGIN_METRICS_OUTPUT`
next to its child-shell setup (`LC_ALL`, `COLORTERM`, `PAGER`, `GIT_PAGER`).
These ten context variables are stripped
for both providers, alongside existing Claude context variables. The originator
override must not carry another harness's identity into Newjen's handshake.
The inherited `CLAUDE_CODE_OAUTH_TOKEN` is also removed, so Newjen never supplies
that alternate sign-in mechanism. `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, API keys,
cloud configuration, and package-manager variables are preserved. Package-manager
variables `CODEX_MANAGED_BY_*` describe the installation, not a nested session.
`CODEX_EXEC_SERVER_URL` is an executor-selection configuration override and
`CODEX_NON_INTERACTIVE` belongs to the installer; neither is removed. Network
proxy and credential-broker configuration is preserved rather than attempting
to bypass an inherited network policy.
No shell execution or model turn was used to investigate Codex child context.

## Claude fast mode evidence and limit

Inspection of the installed Claude 2.1.316 native binary with `strings` found:

- `new bs("--fast-mode <on|off>")` followed by an argument parser accepting `on`
  or `off`, and `.hideHelp()`. It is a real hidden option; public `--help` does
  not list it. `claude --fast-mode on --help` also exits successfully.
- The settings schema includes `fastMode` with description "When true, fast mode
  is enabled. When absent or false, fast mode is off."
- The public help accepts `--settings <file-or-json>`. Together these establish
  non-interactive configuration via `--settings '{"fastMode":true}'`, without
  editing persistent user settings. `claude --settings '{"fastMode":true}' --help`
  exits successfully too. The hidden option is another activation path.
- Eligibility logic consults model metadata under `fast_mode`, falls back to
  names containing `opus-4-8` or `opus-5`, and checks organization status,
  policy, session opt-in and extra usage. The alias-to-model mapping and account
  entitlement are not available in `auth status`.

Therefore Claude `ModelOption.fast` is `None`: detection cannot verify a per-alias,
per-account fast option. This means **unverified/unsupported in Newjen's current
picker**, not a claim that the CLI lacks fast mode. No turn was run to test it.
A future change can expose the settings mechanism once capability verification
has a safe source. Codex explicitly reports a `priority` tier named `Fast`, with
"2x speed, increased usage", so that tier is exposed directly.

## Recordings and verification

`crates/core/tests/fixtures/providers/` records real `claude --version`,
`claude --help`, `claude auth status --json`, and our own stdio `codex app-server`
responses to `initialize`, `account/read` (`refreshToken:false`), `model/list`,
and `account/rateLimits/read`. The handshake sends `clientInfo` identifying
Newjen with the crate version, followed by `initialized`. No managed daemon is
used. Emails, account/organization identifiers and names, reset-credit IDs,
personal home paths, and credit balance were replaced before checking in.

The selected generated schemas are copied verbatim from codex-cli 0.166.0.
Fixture tests check the consumed fields and structurally validate recordings and
request parameters against these schemas, including mixed casing and tagged
account variants. Unknown harmless fields remain forward compatible; incompatible
consumed fields fail parsing. Schemas/recordings should be refreshed together
when testing a new CLI version; offline tests cannot detect an installed binary
changing until recordings are refreshed. Fake executables test both protocols,
errors, total deadlines, signed-out state, API warnings and pagination.

The only new live test is `live_provider_detection_usage_free`. Run exactly:

```sh
cargo test -p newjen-core live_provider_detection_usage_free -- --ignored --nocapture
```

Other ignored live tests send prompts and are outside this detection work.
