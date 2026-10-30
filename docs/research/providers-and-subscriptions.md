# Decision memo: agent providers and subscription safety (2026-10-30)

Research for the next step after the MVP slices: Newjen detects which agent CLIs are installed (Claude Code `claude`, OpenAI `codex`) and drives whichever the thread uses. It also answers the owner's hard requirement: **people using their Claude or ChatGPT subscriptions in Newjen must never risk their accounts.**

Sources were checked on 2026-10-30 against claude 2.1.314, codex-cli 0.166.0, T3 Code `main` at commit `6575ce3` and T3 Code Nightly `0.1.6-nightly.20261030` (which bundles `@anthropic-ai/claude-agent-sdk` 0.3.276). Provider policy changes often: Anthropic changed course four times in 2026. **Re-read the two policy pages in §2 whenever we bump the supported CLI versions.** Claims marked UNVERIFIED were not confirmed from a primary source.

## 1. How T3 Code does it

T3 Code is the reference product here. File paths are in https://github.com/pingdotgg/t3code.

### Detection

- **PATH.** At startup the server rebuilds `PATH` from the user's login shell (`$SHELL -ilc` then `printenv PATH`, falling back to `launchctl getenv PATH`), so a Finder-launched app finds CLIs installed by Homebrew, npm or the official installers (`apps/server/src/os-jank.ts`, `fixPath`). Each provider also has a settable binary path, config home (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`) and extra launch arguments.
- **Claude.** It runs `claude --version`, then an Agent SDK `query()` that never sends a prompt, with hooks, tools and MCP servers turned off. It reads sign-in state from `initializationResult().account`: token source, subscription type and API provider (`ClaudeProvider.ts`).
- **Codex.** It starts `codex app-server`, sends `initialize` (the version comes from `userAgent`), then calls `account/read`, `model/list`, `skills/list` and `config/read` within a 20 s budget (`CodexProvider.ts`).
- **No probe spends usage.** It checks at startup, then every 5 minutes, and again from a Refresh button in Settings → Providers.
- **Version ranges** come from a model manifest downloaded at runtime, with a bundled fallback:
  - Claude: supported ≥ 2.1.280, limited from 2.1.111 to 2.1.280.
  - Codex: supported ≥ 0.156.0, recommended ≥ 0.159.0.
- **States shown:** ready, warning, error or disabled, plus whether the CLI is installed and whether it is signed in.
- **Messages** include "Claude Agent CLI (`claude`) was not found on PATH.", "Run `claude auth login` and try again." and "Installing ChatGPT or Codex desktop may not add codex to PATH." The install hints are the official installers.

### Launch

- **Claude** runs through the TypeScript Agent SDK, which spawns the user's `claude` with the same stream-json flags Newjen uses.
  - It sets no `settingSources`, so the user's settings, CLAUDE.md, hooks and MCP servers all load. Only the health probe turns them off.
  - It adds its own HTTP MCP server for orchestration. That server is sent over stdin with `setMcpServers`, so its token never shows up in `ps`.
  - Permission modes map from T3's run modes (approval-required → `default`, and so on).
  - **T3 does not identify itself to Anthropic**: it never sets `CLAUDE_AGENT_SDK_CLIENT_APP`.
- **Codex** runs as one `codex app-server` per provider session, spoken to over stdio (`CodexAdapterV2.ts`).
  - `initialize` sends `clientInfo {name:"T3 Code", title:"T3 Code", version}`, and the env sets `CODEX_INTERNAL_ORIGINATOR_OVERRIDE`.
  - The user's `~/.codex/config.toml` and MCP servers load. T3 overrides only its own `mcp_servers.t3-code` entry.
  - T3 does **not** use the shared Codex daemon (`codex app-server --managed-daemon`, under `~/.codex/packages/app-server-daemon/`). That daemon serves Codex's own remote clients and updates itself every hour, and an update "may interrupt running work".
- **Codex approvals.** T3 maps its run modes to `approvalPolicy` and sandbox values:

  | T3 run mode | `approvalPolicy` | Sandbox |
  |---|---|---|
  | approval-required | `untrusted` | read-only |
  | auto-accept-edits | `on-request` | workspace-write |
  | full-access | `never` | danger-full-access |

  It answers `item/commandExecution/requestApproval` and `item/fileChange/requestApproval`. **Its "accept for session" is broader than it looks.** After one accept-for-session, T3 auto-approves every later request of the same kind (any command, or any file change) in that thread, although Codex's own `acceptForSession` covers only that exact command or patch. Newjen should not copy this.

### Threads and models

- Each thread stores its provider instance, model choice and run mode.
- Switching provider mid-thread is supported. It starts a new provider conversation and hands over a bounded selection of earlier messages, but no tool state or approvals.
- Several instances of one provider are allowed, for example two Claude logins with different config folders.
- Claude models come from the manifest, filtered by CLI version. Codex models come from the live `model/list`.

## 2. Subscription safety

### What the providers allow (as of 2026-10-30)

- **Anthropic, [Legal and compliance](https://code.claude.com/docs/en/legal-and-compliance):**
  - Developers may not "collect, store, or intermediate Claude.ai credentials or session tokens".
  - They may not offer Claude.ai login, or route requests through Free, Pro or Max credentials on behalf of users.
  - The page also says this does not "prevent an end user from signing in to the unmodified Claude Code binary with their own Claude subscription".
  - Usage limits "assume ordinary, individual usage of Claude Code and the Agent SDK."
  - In-app labels may not call the product "Claude Code"; plain prose may say it "runs Claude Code".
  - UNVERIFIED: whether the "running Claude Code in your products" clause covers a free, local, open-source app. Newjen meets every condition listed there anyway: unmodified binary, built-in sign-in left alone, and each user paying for their own usage.
- **Anthropic, [support article 15036540](https://support.claude.com/en/articles/15036540)** (updated 2026-10-07): "Claude Agent SDK, `claude -p`, and third-party app usage still draw from your subscription limits." A planned separate "Agent SDK credit" was announced on 2026-05-13 and then paused.
- **Anthropic staff, 2026-02-18:** "We want to encourage local development and experimentation with the Agent SDK and claude -p. If you're building a business on top of the Agent SDK, you should use an API key instead."
- **OpenAI, [app-server docs](https://learn.chatgpt.com/docs/app-server):** "If you've built a local or open-source application using Codex app-server authentication, you can continue using it". It is "never … permitted for commercial or hosted services". OpenAI recommends "Sign in with ChatGPT" (a preview), where the app stores OAuth tokens itself. That conflicts with Newjen's "never touch credentials" rule, so not now.

### What got people banned or blocked

- **January 2026.** OpenCode and similar tools took the subscription OAuth token and called Anthropic's API directly, sending headers that pretended to be Claude Code. Accounts were banned for tripping abuse filters (the bans were later reversed), and the server now rejects such calls with "This credential is only authorized for use with Claude Code".
- **April 2026.** OpenClaw usage was moved to pay-as-you-go "extra usage" rather than banned. How it signed in is UNVERIFIED.
- **No case found** of a tool being told to stop for spawning the user's own, unmodified official CLI.

### Verdict

Newjen's approach is the path both providers allow: spawn the user's own logged-in CLI, strip the nested-session environment variables, and never touch credentials. T3 Code, Conductor and Vibe Kanban do the same. The remaining risk is that policy changes.

### Newjen rules

These are copied into `CLAUDE.md` as standing rules.

**Do:**
- Spawn only the user's installed, unmodified `claude` and `codex`, and let them handle sign-in.
- **Identify Newjen honestly.**
  - Claude: set `CLAUDE_AGENT_SDK_CLIENT_APP=newjen/<version>` on every spawn, replacing any inherited value. The claude binary reads it and adds `client-app/<value>` to its User-Agent. `claude -p` already labels itself `sdk-cli`, so we neither hide nor fake what we are.
  - Codex: send `clientInfo {name:"newjen", title:"Newjen", version}`.
- Show the account and plan in use, read from the CLI's own reports:
  - Claude: `claude auth status --json` returns `loggedIn`, `authMethod` and `subscriptionType`, plus email and organization, which should be shown only on request.
  - Codex: `account/read`.
- Warn when `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` is set, because the CLI will then bill the API instead of the plan.
- Respect usage limits:
  - Show `rate_limit_event` warnings.
  - On `rejected`, stop sending and show the reset time.
  - Never retry, auto-resume at reset, or auto-continue a turn.
- Start every turn from a user action. Cap the number of threads running at once by default.
- If the user isn't logged in, tell them to run `claude` or `codex login` in their own terminal.

**Don't:**
- Read the Keychain, `~/.claude/.credentials.json` or `~/.codex/auth.json`, or pass tokens with `CLAUDE_CODE_OAUTH_TOKEN` or `claude setup-token`.
- Call model APIs directly, or fake the entrypoint, headers or system prompt.
- Build a login screen or proxy sign-in.
- Share or rotate accounts.
- Ship a hosted or paid version that runs on subscription logins.
- Use "Claude Code" as a product name or in-app label.

## 3. Choices for Newjen's provider step

1. **Detection costs no usage.**
   - Claude: `claude --version` plus `claude auth status --json`. This is simpler than T3's SDK probe and starts no MCP servers.
   - Codex: `codex app-server` with `initialize` plus `account/read`.
   - Resolve `PATH` from the login shell the way T3 does, then fall back to the known install paths already in `find_claude()`. Without this, a Finder launch misses CLIs, and the agent's own Bash tool misses `node`, `git` and other tools.
2. **One provider per thread,** chosen from the installed CLIs when the thread is created. Switching mid-thread with a handoff (T3) can come later.
3. **A provider-neutral core.** One event model (`Transcript` items, approvals, turn end, errors), with a Claude adapter and a Codex adapter behind it, so the UI doesn't branch on the provider.
4. **Codex approvals keep Newjen's exact-scope rule:**
   - `acceptForSession` covers only the exact command or patch, as Codex intends.
   - No thread-wide auto-approve.
   - Edits stay once-only until slice 4's diff view.
5. **Spawn our own stdio `codex app-server`** per thread, never the shared daemon.
6. **Trust `codex app-server generate-json-schema` over the web docs.** The docs' examples use camelCase values that 0.166.0 rejects. Record real sessions as fixtures, as for Claude.

### Codex app-server protocol (0.166.0, from the generated schema)

- **Framing:** one JSON object per line over stdio, with no `"jsonrpc"` field.
- **Handshake:** `initialize`, then the `initialized` notification.
- **Start a thread:** `thread/start {model, cwd, approvalPolicy, sandbox, config, developerInstructions}`.
- **Send a turn:** `turn/start {threadId, input:[{type:"text", text}], ...}`.
- **Stream events:** `turn/started`, `item/started`, `item/agentMessage/delta`, `item/reasoning/summaryTextDelta`, `item/commandExecution/outputDelta`, `item/completed`, `thread/tokenUsage/updated`, `turn/completed`, `error {willRetry}`.
- **Approvals:** the server sends `item/commandExecution/requestApproval` or `item/fileChange/requestApproval`. Reply with `decision` = `accept` | `acceptForSession` | `decline` | `cancel`. `serverRequest/resolved` follows.
- **Stop:** `turn/interrupt {threadId, turnId}`. **Resume:** `thread/resume`.
- **Account:** `account/read`, `account/rateLimits/read` and `account/rateLimits/updated`.
- **Wire spellings:**
  - `approvalPolicy` is `untrusted | on-request | never | {granular}`.
  - `sandbox` is `read-only | workspace-write | danger-full-access`.
  - `sandboxPolicy` is an object such as `{type:"workspaceWrite"}`.
  - Field casing is mixed (`threadId` but `text_elements`), so one blanket rename rule on the Rust types will break.
