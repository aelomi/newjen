# Contributing to Newjen

Newjen is a native macOS agent development environment (ADE). It runs many Claude Code and Codex agents in parallel, each in the project or in its own copy of it, from a fast, low-memory GPUI app.

The full plan, with the reasons behind each decision, is in [`docs/PLAN.md`](docs/PLAN.md). Read it before making architectural changes. Research behind the bigger decisions is in [`docs/research/`](docs/research/).

## Ground rules

- **Never commit secrets, tokens or personal absolute paths.**
- **Pin every dependency to an exact version** (`=x.y.z`) and commit `Cargo.lock`.
- **macOS only.** Don't add Linux or Windows code paths.
- **`core` never depends on the UI libraries**, so the UI layer can be replaced.
- **Follow the settled decisions below.** Raise a concern once, with evidence, rather than working around one.

## Settled decisions

| Area | Decision |
|---|---|
| Platform | **macOS only**. Don't add Linux or Windows code paths. |
| Language | Rust, in a Cargo workspace. Toolchain 1.99.1 pinned in `rust-toolchain.toml`. |
| UI | GPUI via `gpui = { package = "gpui-pre", version = "=0.3.10" }` and `gpui_platform = { package = "gpui-pre-platform", version = "=0.3.10", features = ["font-kit"] }`. |
| Components | **GPUI Kit** (Longbridge): `gpui-kit = "=0.8.1"`, a plain crates.io dependency built on the same GPUI. **Do not fork or vendor it.** Since 2026-11-01 Newjen restyles Kit to its own look, "panels on glass": a blurred window, the sidebar on the glass, and solid floating panels. The restyle goes through Kit's theme and Newjen's own wrappers, and follows the emil-design-eng and apple-design principles. Kit's components stay the building blocks. Newjen hand-builds only the approval card, tool card and working-dot pulse, from Kit pieces; `crates/app/src/ui/` also holds thin behavior helpers. If a Kit bug blocks work, use a temporary `[patch.crates-io]` entry and report it upstream. |
| Agents | Claude Code CLI over stream-json and Codex CLI (`codex app-server`), both in the MVP (decided 2026-10-31). Newjen detects which CLIs are installed without spending usage, and each thread picks one provider, model, effort and speed when it is created. `core` has one neutral event and approval model (`agent.rs`) with a Claude adapter and a Codex adapter under `providers/`. API-key providers and ACP agents come later. |
| Isolation | **Rift** copy-on-write copies, **opt-in per thread**: by default a thread works in the project folder, and you choose a Rift copy when you want one. When chosen, it must be seamless. No git worktrees for now. |
| Version control | **jj (Jujutsu)**, colocated with git. |
| Hosting | **GitHub** (`github.com/aelomi/newjen`, in the Aelomi organization, CLI `gh`), moved from GitCafe on 2026-11-01 for GitHub Releases, hosted macOS CI and contributor familiarity. |
| Storage | SQLite for app state, migrations by `user_version` in one append-only list (`store.rs`). The CLIs keep their own transcripts; Claude resumes with `--resume <session-id>`, Codex with `thread/resume`. |
| CI | GitHub Actions, workflow in `.github/workflows/ci.yml`, on GitHub's hosted Apple Silicon macOS runners. |


## Layout

```
crates/
  core/   # package `newjen-core`. No UI. Tested headlessly.
          #   agent.rs: the provider-neutral events, approvals and agent config the app sees.
          #   providers/: detection (`detect.rs`), the Claude adapter (stream-json) and the Codex adapter (app-server JSON-RPC).
          #   session.rs, thread.rs, transcript.rs: one process per thread, its lifecycle and the rendered transcript.
          #   rift.rs: Rift copies. landing.rs, edits.rs: jj snapshots, review, landing and edit previews.
          #   store.rs: SQLite. instance.rs: the single-instance lock and local launch handoff. env.rs: the login shell's PATH.
  app/    # package `app`, binary `newjen`. The GPUI and GPUI Kit shell. Renders core state and sends user intents to core.
          #   src/project_view.rs: the window, its loaded projects, the Dock badge, notifications and the quit gate.
          #   src/chat_view.rs (+ chat_view/): one project's threads, composer, approval dock, picker, review pane, terminal tab.
          #   src/sidebar.rs, src/usage_view.rs: the thread list (grouped by what needs you) and its usage popover.
          #   src/terminal.rs: the per-thread terminal panel. src/attention.rs: AppKit badge and notification adapters.
          #   src/theme.rs: the sidebar width, the card inset and radius, and the material tokens. src/ui/: approval card, tool card, pulse, thin Kit helpers and style wrappers (`card`, `glass_title_bar`, `card_title_bar`).
          #   src/assets.rs: extra Kit icons. THIRD-PARTY-NOTICES.md: embedded Lucide icon licenses.
design/   # newjen.pen: pen.dev mocks of every UI state. Mock UI changes here first.
docs/     # PLAN.md, landing.md (how review and landing work) and research memos.
```

## Build order

Build one vertical slice first, and don't start wrappers for later steps before it works end to end.

1. `cargo run` opens a window. You type a prompt, Claude replies over stream-json, and the reply renders in the chat. Approve and deny work. One thread; no Rift, no jj. **Done (2026-10-27).**
2. Thread list, SQLite persistence, resume. **Done (2026-10-31).**
3. Rift copies, chosen per thread (the default stays the project folder). **Done (2026-10-31).**
4. jj snapshots, diff view (GPUI Kit `diff`), and both landing modes (see the plan and [`docs/landing.md`](docs/landing.md)). **Done (2026-10-31).**
5. Parallel threads, an approvals inbox, Dock badge and notifications, a terminal panel (on `alacritty_terminal`), and a quit prompt. **Done (2026-10-31).**
6. Providers: Claude and Codex side by side, with a model, effort and speed picker and usage-limit pauses. **Done (2026-10-31)**, part of the MVP.

## Verified facts (tested on macOS)

### Claude Code stream-json

```
claude -p --verbose --input-format stream-json --output-format stream-json \
       --permission-prompt-tool stdio --permission-mode default \
       --include-partial-messages [--resume <id>]
```

Checked against claude 2.1.310. `crates/core/tests/fixtures/` holds sanitized recordings of each case below.

- **Sending input:** write one JSON object per line to stdin: `{"type":"user","message":{"role":"user","content":"..."}}`. Always produce the line with a JSON encoder: a line that isn't JSON kills the CLI. **Keep stdin open** for the whole session.
- **Closing stdin** makes the CLI exit about a second later, after it finishes the current turn. A pending approval is then rejected with an `AbortError` tool_result, and the model may keep going without the tool.
- **Approval requests** arrive on stdout as `{"type":"control_request","request_id":…,"request":{"subtype":"can_use_tool","tool_name":…,"input":…,"tool_use_id":…,"permission_suggestions":[…]}}`. Read-only commands such as `ls` are approved automatically and never ask, so test approvals with a command that writes, such as `touch`.
- **Answering an approval:** reply on stdin with `{"type":"control_response","response":{"subtype":"success","request_id":…,"response":{"behavior":"allow","updatedInput":<input>}}}`. No MCP server is needed.
  - **Deny** with `"behavior":"deny","message":"…"`. The `message` is required; without it the model retries the tool. Add `"interrupt":true` to also end the turn.
  - **Allow for the session:** add `"updatedPermissions"` with the request's `addRules` suggestions, their `destination` changed to `"session"`. Later identical calls are approved without asking, and nothing is written to the project.
  - **Rules that grant more than they look like:** a session `setMode` `acceptEdits` (the CLI's suggestion for Write/Edit) also approves shell commands such as `rm` and `mkdir`. A whole-tool rule such as `{"toolName":"Write"}` approves writes outside the project. A `*` inside a Bash rule's `ruleContent` is a wildcard. So "Always allow" forwards an `addRules` suggestion only when every rule names the requesting tool (never an edit tool) and carries `ruleContent`, or names exactly one `mcp__server__tool`; otherwise it remembers an exact Bash command without `*`, or allows once. The dock shows which before the click (`session_scope` in `crates/core/src/protocol.rs`).
- **Stop a turn** with `{"type":"control_request","request_id":"<any id>","request":{"subtype":"interrupt"}}`.
- **Other events:** stdout also carries `system` (hooks, and `init` at the start of every turn, not at spawn), `assistant` (one line per finished content block, so several lines share a `message.id`), `stream_event` (token deltas, with `--include-partial-messages`), `user` (`tool_result`), `rate_limit_event` and `result` (end of turn). Background tasks (Bash `run_in_background`, claude 2.1.314) outlive their turn: `system/background_tasks_changed` carries the full list (`[]` when none), and when one finishes claude starts a turn by itself (`init`, reply, `result`) with no user message. A `result` with `queued_turn_count` > 0 means a user message sent meanwhile is still queued and its turn follows (from the CLI's schema text; not recorded).
- **Errors:** branch on `result.is_error`, not `subtype`. A missing login or bad model comes back as `subtype:"success"` with `is_error:true`. `total_cost_usd` is a running total across turns and `--resume`; per-turn cost is in `model_usage_since_last_result`.
- **Environment:** when Newjen runs inside a Claude Code terminal, nested-session variables (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID` and others) leak into the child. `crates/core/src/session.rs` strips them and keeps the user's own config variables.
- **Tests:** record real sessions as fixture files and replay them in `core` tests.

### Rift + jj

- **Use `rift create --copy-all`.** Without it, Rift skips gitignored files (`node_modules`, `target/`), which defeats the purpose.
- **Start the copy's work on a change the main repo no longer edits.** Otherwise the copy and the main repo share a jj change, and fetching the work back creates a divergent change. `crates/core/src/rift.rs` does it in three steps, checked with real Rift on 2026-10-31 (`crates/core/tests/rift.rs`):
  1. In the main repo, snapshot, and run `jj new` only if `@` has changes. The base is then `@-`.
  2. Run `rift create --copy-all` with a fresh `newjen-<id8>-<6 random lowercase letters or digits>` name, saved with the held message before the job starts.
  3. In the copy, run `jj new <base>`. The copy's own copy of `@` is empty, so jj abandons it.
  Landing by fetch then has no divergent change, even after the main repo keeps editing. The older rule (describe, then `jj new` in both repos) also works but leaves two empty changes in the main repo per copy.
- **Don't make a copy while an agent works in the project folder.** Step 1 snapshots the main repo, which would catch that agent's half-written files. Newjen refuses until the project-folder thread finishes.
- **Land only the thread's bookmark.** In the main repo, run `jj git remote add <thread> <copy-path>` and `jj git fetch --remote <thread> --branch newjen/<id8>`. A full fetch brings inherited bookmarks and divergent changes. Then run `jj bookmark track newjen/<id8>@<thread>`. If the base was rewritten in main, run `jj rebase -s 'roots(<base>..<bookmark>)' -d <base change's current commit>` and `jj abandon <old base commit>`. This keeps the thread's work on the current base and leaves `divergent()` empty.
- **Expect a harmless warning:** jj prints "repo appears to have been copied" the first time it runs in a copy.
- **`rift init` once per project.** It writes a `.rift` marker at the root and adds it to `.git/info/exclude`, so jj never tracks it in a colocated repo.
- **Hooks:** `.rift.toml` holds `version = 1` and `[[hooks.postcreate]]` tables with `run = "…"`. Hooks print to stderr, which Newjen shows while the copy is made, and see `RIFT_ID`, `RIFT_SOURCE`, `RIFT_DESTINATION` and `RIFT_PARENT_ID`. A failed `postcreate` leaves the copy registered, so Newjen removes it. Anything a `postcreate` hook leaves running in the background is stopped when `rift create` ends.
- **`rift` on `PATH` is a Node script.** Newjen runs the native binary beside it (`prebuilds/darwin-<arch>/rift`), so apps started from Finder work without Node.
- **Copy ownership:** the unique name proves ownership of the exact destination. Save the Rift id before `jj new` in the copy. Before sending the held message, acknowledge a sent pending row; recovery keeps sent copies even if the thread checkpoint failed. Recovery validates the project copy parent and the thread name.
- **Copy jobs stop on quit and on Cancel:** `rift create` and its hooks run in their own process group, registered for crash recovery. Quit, or ⌘. while the copy is made, sends SIGTERM, then SIGKILL; a running jj finishes. A cancelled copy is removed and the held message comes back to the composer.
- **Hooks get the login shell's PATH.** An app started from Finder has a minimal `PATH`; `Tools::with_env` passes `env::resolved_path()` to Rift and its hooks.
- **Remove copies by path:** `rift remove <copy-path>` (not the name) moves the copy to `.trash` next to it. Newjen then deletes that one trash entry. A different Rift id at the destination is preserved; our own trash entry is still removed. A missing `.rift` marker means an owned half-made or cleaned copy, removed directly after checking its parent and refusing symlinks. Never run `rift gc` from Newjen: it empties every Rift trash on the Mac.
- **UI reads of jj must pass `--ignore-working-copy`.** Without it, each read snapshots an agent's half-written files.
- **jj does not run git hooks.** Run any checks they would have done from the harness.

## Subscription safety

Users run Newjen on their own Claude and ChatGPT subscriptions. **Never do anything that could get their accounts banned.** What got other harnesses banned in 2026 was copying subscription tokens and calling the API while pretending to be Claude Code. Sources and dates are in [`docs/research/providers-and-subscriptions.md`](docs/research/providers-and-subscriptions.md); re-check its two policy links whenever the supported CLI versions change.

- **Only spawn the user's own installed, unmodified `claude` or `codex`**, and let it handle sign-in. If they aren't logged in, tell them to run `claude` or `codex login` in their own terminal.
- **Never touch credentials.** Don't read the Keychain, `~/.claude/.credentials.json` or `~/.codex/auth.json`; don't use `CLAUDE_CODE_OAUTH_TOKEN` or `claude setup-token`; don't build a login screen or proxy sign-in.
- **Never call model APIs directly**, and never fake the entrypoint, headers or system prompt.
- **Don't disguise or relabel the CLI.** `claude -p` already reports itself to Anthropic as a non-interactive (`sdk-cli`) session, so don't change that. Like T3 Code, Newjen doesn't set `CLAUDE_AGENT_SDK_CLIENT_APP` and strips any inherited value. Codex requires `clientInfo`; send the name `newjen`.
- **Respect usage limits.** Every turn starts from a user action. Never auto-retry, auto-resume at a limit reset, or auto-continue a turn. Stop on a rejected rate limit and show when it resets.
- **Say which account pays.** Show the account and plan from the CLI's own report (`claude auth status --json`, Codex `account/read`), and warn when `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` will bill the API instead.
- **Codex runs with `approvalPolicy: untrusted` and a read-only sandbox**, so every write and every non-trivial command asks, as Claude's `--permission-mode default` does. Newjen's own instructions to a thread go through each CLI's official seam: `--append-system-prompt` for Claude, `developerInstructions` on `thread/start` for Codex.
- **No "Claude Code" in Newjen's name or UI labels.** Prose such as the README may say it "runs Claude Code".

## GPUI and GPUI Kit: don't guess APIs

Models often invent GPUI APIs because there is little public documentation and the API changes between versions. Before writing UI code:

- **Read the real GPUI Kit and GPUI source.** After the first `cargo build`, it's in `~/.cargo/registry/src/*/` under `gpui-kit-0.8.1/` (the facade), `gpui-component-0.8.1/` (styled components), `gpui-base-0.8.1/` (text view, input, popover, motion), `gpui-kit-assets-0.8.1/` (icons) and the `gpui-pre-*-0.3.10/` folders.
- **GPUI Kit's guidance:**
  - Its guides: https://gpui-kit.com
  - Its source repository: https://github.com/longbridge/gpui-kit (read only)
- **Setup** (`crates/app/src/main.rs`): call `gpui_kit::init(cx)`, then follow system appearance with `Theme::change(window.appearance(), Some(window), cx)` when opening the window and in its appearance observer. Use Kit's default fonts; no font assets or registration are needed. Open windows with `gpui_kit::open_window`, which wraps the view in Kit's `Root`: without it tooltips never show and menus lose focus tracking. Without the `font-kit` platform feature, no text renders on macOS.
- **Colors come from Kit's theme plus Newjen's material tokens; sizes come from Kit.** Read `cx.theme()` and use Kit variants, sizes, fonts and text styles. `theme.rs` holds the app layout dimensions and the material tokens (`glass_tint`, `glass_hover`, `glass_selected`, `glass_muted` for muted text on the glass, `card`, `card_stroke`, `popover_glass`), each with a dark and a light value. `theme::apply` puts them onto Kit's theme, and `Theme::change` resets that theme, so call it after every `Theme::change`. Kit's `background` color is set to the card fill, so the card's contents share one solid. Kit's window root paints the `background` token, which carries the glass tint, so dialogs and sheets must set their own solid background (`card`). The main window opens with `WindowBackgroundAppearance::Blurred`. macOS Reduce Transparency, re-read every second, switches the window to opaque and makes the tint and popovers opaque. Do not add another palette.
- **Reduce Motion belongs to GPUI.** The app re-reads the system setting every second through GPUI Kit; never call `cx.set_reduce_motion` from the app, or the system setting stops applying.
- **Use Kit's components.** Read a component's source before using it. Buttons use `.label(..)`, `.icon(..)` and `.tooltip(..)`; messages use `MessageScroller`, `Message`/`Bubble` and default `TextView` markdown. Inputs, status tags, overlays and window chrome also come from Kit. Add only the small behavior wrappers needed for Newjen's keys and focus paths.
- **Markdown highlighting:** the pinned Kit dependency enables tree-sitter with Bash, Rust, JavaScript/TypeScript, TSX, Python and TOML; JSON is included by tree-sitter. Mermaid fences remain code. Reply states are app-owned and built lazily. Kit's `test-support` feature currently conflicts with GPUI's taffy dependency; headless tests use GPUI's `test-support` dev-dependency.

## UI work

Mock a change in `design/newjen.pen` with [pen.dev](https://pen.dev) first, including its states (empty, running, needs approval, error, worst-case data), then build to match.

## Commands

Run these commands from the repository. Rustup selects and installs the Rust version pinned in `rust-toolchain.toml`, including Clippy and rustfmt.
Bump Rust by editing `rust-toolchain.toml` in its own commit.

```sh
rustup toolchain install
cargo run -p app [-- <project dir>]   # launch the app; without a folder it shows the project chooser
cargo test --workspace --locked       # all tests; no network or login needed (Rift tests skip without rift and jj)
cargo test -p newjen-core -- --ignored   # live tests against the real claude and codex CLIs (use your logins)
cargo build --release                 # optimized build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

A second launch with the same data folder opens or switches to the requested project in the running window. Without a project, it brings that window to the front. Separate `NEWJEN_HOME` folders run independently. A successful handoff exits quietly with code 0. If the running instance cannot accept the request before the timeout, the launcher exits with code 1 and reports an error. In an app bundle, macOS folder-open and reopen events use the running window. The bundle must declare `public.folder` in `CFBundleDocumentTypes` to accept project folders.

The first build takes about 1–2 minutes and creates a `target/` folder of several GB. Later builds are incremental.

## Build the app

```sh
scripts/bundle-app.sh
scripts/bundle-app.sh 0.1.0-alpha.1
open dist/Newjen.app
```

## Version control

Use **jj**, not raw git, for commits in this repo:

```sh
jj describe -m "message"                         # describe the current change
jj new                                            # start the next change
jj bookmark set master -r @-                      # move master to the finished change
jj git push --bookmark master                     # push to GitHub
```

- The remote is `origin`, at `https://github.com/aelomi/newjen.git`. The default branch is `master`.
- Commit titles use [Conventional Commits](https://www.conventionalcommits.org): `type(scope): subject`, such as `fix(core): keep stdin open after an interrupt`.
- Use `gh` for PRs, issues and CI (`gh pr`, `gh issue`, `gh run`).
- Open pull requests with `gh pr create --head <bookmark> --base master`. jj leaves git without a current branch, so push the bookmark first and always pass `--head`.

## Releasing

The repository doubles as the Homebrew tap. The cask lives in `Casks/newjen.rb`.
The release workflow uses the built-in `GITHUB_TOKEN` with `contents: write` for releases and cask updates.
No separate tap repository or token secret is needed. Repository rules must allow Actions to push a cask update to `master`.

Use tags such as `v0.1.0-alpha.1`, `v0.1.0-beta.1`, `v0.1.0-rc.1`, or `v0.1.0`.
Version numbers have three numeric components without leading zeroes.
The optional suffix is `alpha`, `beta`, or `rc`, optionally followed by a numeric component such as `.1`.
Every `0.x` version and every version with a suffix becomes a GitHub pre-release.
Versions from `1.0.0` without a suffix become stable releases.

After the release changes land, create and push a version tag pointing at the commit to release.
A tag push runs `.github/workflows/release.yml` on GitHub's hosted Apple Silicon macOS runner.
You can also run **Release** from the Actions tab and enter an existing version tag.
Manual dispatch builds that tag, even if you select a different workflow branch.
The tag must contain the release workflow, `scripts/bundle-app.sh`, and `rust-toolchain.toml`.
CI, releases, and local builds use `rust-toolchain.toml` from the checked-out revision.
For a release, that file comes from the selected tag.

The workflow runs the same build, test, format, and Clippy gates as CI.
It calls `scripts/bundle-app.sh <version>` to build an ad-hoc-signed app without Developer ID signing or notarization.
It publishes `Newjen-<version>-arm64.zip` and `Newjen-<version>-arm64.zip.sha256` through `gh release create`.
It then checks out `master`, updates only the cask's version and SHA-256, and commits that file with a bot identity.
If a push fails, it fetches `master`, rebases, and retries once. Conflicts or another rejected push fail with recovery instructions.

The cask commit cannot trigger a release because the release push trigger matches only tags.
GitHub also suppresses push workflows for commits made with `GITHUB_TOKEN`, so the cask commit does not start CI.
CI's `master` push and pull request triggers do not publish releases.

The initial cask has an all-zero checksum until the first successful release updates it.
This placeholder fails checksum verification instead of accepting an unverified download.
Check the release and cask before announcing it. The cask tracks whichever version you release, including pre-releases.
Do not reuse a published tag or replace its assets. Publish a new version to correct a release.
If only the cask update fails, copy the version and checksum from the published asset into `Casks/newjen.rb` and commit that change.
Do not rerun publication for an existing release.
