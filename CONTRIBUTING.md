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
| Components | **GPUI Kit** (Longbridge): `gpui-kit = "=0.8.1"`, a plain crates.io dependency built on the same GPUI. **Do not fork or vendor it.** Since 2026-11-01 Newjen restyles Kit to its own look, "panels on glass": a blurred window, the sidebar on the glass, and solid floating panels. The restyle goes through Kit's theme and Newjen's own wrappers, and follows the emil-design-eng and apple-design principles. Kit's components stay the building blocks. Newjen hand-builds only the approval card and tool card, from Kit pieces; `crates/app/src/ui/` also holds thin behavior helpers. If a Kit bug blocks work, use a temporary `[patch.crates-io]` entry and report it upstream. |
| Agents | Claude Code CLI over stream-json and Codex CLI (`codex app-server`), both in the MVP (decided 2026-10-31). Newjen detects which CLIs are installed without spending usage, and each thread picks one provider, model, effort and speed when it is created. `core` has one neutral event and approval model (`agent.rs`) with a Claude adapter and a Codex adapter under `providers/`. API-key providers and ACP agents come later. |
| Isolation | **Git worktrees**, **opt-in per thread**: by default a thread works in the project folder, and you choose its own worktree when you want one. When chosen, it must be seamless. Since 2026-11-02; Rift copies (2026-10-30 to 2026-11-02) may return later as an experiment. |
| Version control | **Plain git**, in this repo and in the projects Newjen reviews and lands. jj was used until 2026-11-02 and may return later as an experiment. |
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
          #   worktree.rs: git worktrees. vcs.rs: checkpoints, diffs and git actions. commit_message.rs: AI commit messages through the user's CLI. changes.rs, edits.rs: per-file change counts and edit previews.
          #   thread_usage.rs: context fill and tokens per model for one thread. usage.rs: each CLI's plan windows, pace and billing.
          #   update.rs (+ update/): in-app updates. The releases Atom feed, curl downloads, Ed25519 signature check, staging, and the detached install script. changelog.rs: the bundled CHANGELOG.md, What's New and the last version seen.
          #   calendar.rs: local days for snoozes and the Snoozed and Settled panels.
          #   store.rs: SQLite. instance.rs: the single-instance lock and local launch handoff. env.rs: the login shell's PATH.
  app/    # package `app`, binary `newjen`. The GPUI and GPUI Kit shell. Renders core state and sends user intents to core.
          #   src/project_view.rs: the window, its loaded projects, the Dock badge, notifications and the quit gate.
          #   src/chat_view.rs (+ chat_view/): one project's threads, composer, approval dock, picker, review pane, terminal panel.
          #   src/sidebar.rs (+ sidebar/dock.rs, sidebar/motion.rs): the thread list (Pinned, a Queue of what waits on you, then the read list) with the In flight, Snoozed and Settled dock under it, and a settings gear and a usage ring at its foot (the ring opens the Usage view). `motion.rs` keeps a row that changes place on screen while it fades out and eases the arrival in.
          #   src/context_chip.rs: the composer's context chip and its "This thread" popover. src/usage_view.rs: the Usage view (plan windows with pace, threads in the current windows, a week of tokens).
          #   src/terminal.rs: the per-thread terminal panel. src/attention.rs: AppKit badge and notification adapters.
          #   src/theme.rs: the sidebar width, the card inset and radius, and the material tokens. src/ui/: approval card, tool card, thin Kit helpers and style wrappers (`card`, `glass_title_bar`, `card_title_bar`). `ui/fox.rs` is the spirit fox runner game (`FoxGame`) that `chat_view.rs` shows under the thread list while it holds no Pinned, Queue or read thread.
          #   src/assets.rs: extra Kit icons. THIRD-PARTY-NOTICES.md: embedded Lucide icon licenses.
design/   # newjen.pen: pen.dev mocks of every UI state. Mock UI changes here first.
docs/     # PLAN.md, git.md (checkpoints, changes and the git button) and research memos.
```

## Build order

Build one vertical slice first, and don't start wrappers for later steps before it works end to end.

1. `cargo run` opens a window. You type a prompt, Claude replies over stream-json, and the reply renders in the chat. Approve and deny work. One thread; no worktrees, no git actions. **Done (2026-10-27).**
2. Thread list, SQLite persistence, resume. **Done (2026-10-31).**
3. Git worktrees, chosen per thread (the default stays the project folder). **Done (2026-10-31, rebuilt on git worktrees 2026-11-02).** The slice first used Rift copies.
4. Checkpoints, diff view (GPUI Kit `diff`) and git actions; agents commit themselves (see the plan and [`docs/git.md`](docs/git.md)). **Done (2026-10-31, rebuilt on git 2026-11-02).** The slice first used jj and two landing modes, then briefly a model where only Newjen committed.
5. Parallel threads, a Queue in the thread list (⌘J jumps to what needs you), Dock badge and notifications, a terminal panel (on `alacritty_terminal`), and a quit prompt. **Done (2026-10-31).**
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
  - **Rules that grant more than they look like:** a session `setMode` `acceptEdits` (the CLI's suggestion for Write/Edit) also approves shell commands such as `rm` and `mkdir`. A whole-tool rule such as `{"toolName":"Write"}` approves writes outside the project. A `*` inside a Bash rule's `ruleContent` is a wildcard. So "Allow in this thread" forwards an `addRules` suggestion only when every rule names the requesting tool (never an edit tool) and carries `ruleContent`, or names exactly one `mcp__server__tool`; otherwise it remembers an exact Bash command without `*`, or allows once. The dock shows which before the click (`session_scope` in `crates/core/src/protocol.rs`).
- **Stop a turn** with `{"type":"control_request","request_id":"<any id>","request":{"subtype":"interrupt"}}`.
- **Other events:** stdout also carries `system` (hooks, and `init` at the start of every turn, not at spawn), `assistant` (one line per finished content block, so several lines share a `message.id`), `stream_event` (token deltas, with `--include-partial-messages`), `user` (`tool_result`), `rate_limit_event` and `result` (end of turn). Background tasks (Bash `run_in_background`, claude 2.1.314) outlive their turn: `system/background_tasks_changed` carries the full list (`[]` when none), and when one finishes claude starts a turn by itself (`init`, reply, `result`) with no user message. A `result` with `queued_turn_count` > 0 means a user message sent meanwhile is still queued and its turn follows (from the CLI's schema text; not recorded).
- **Errors:** branch on `result.is_error`, not `subtype`. A missing login or bad model comes back as `subtype:"success"` with `is_error:true`. `total_cost_usd` is a running total across turns and `--resume`; per-turn cost is in `model_usage_since_last_result`.
- **Environment:** when Newjen runs inside a Claude Code terminal, nested-session variables (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID` and others) leak into the child. `crates/core/src/session.rs` strips them and keeps the user's own config variables.
- **Tests:** record real sessions as fixture files and replay them in `core` tests.

### Git

Rift copies and jj were used until 2026-11-02; `docs/PLAN.md` has the history. [`docs/git.md`](docs/git.md) describes the behaviour. `crates/core/tests/vcs.rs` and `crates/core/tests/worktree.rs` need only git.

- **Agents do version control themselves.** Newjen sends no git instructions. Its own checkpoints are private commits under `refs/newjen/<thread id>/<n>`, built in a separate index (`GIT_INDEX_FILE`) on top of `HEAD`, scoped to the thread's folder with a pathspec, skipping embedded repositories; they never touch the user's index, `HEAD` or branches (`crates/core/src/vcs.rs`). This is the same technique as T3 Code's checkpoints.
- **The git button** (commit, push, `gh pr create`, merge a worktree branch) runs only on a user's click. The commit message comes from the user's own `claude -p --model haiku --tools ""` (`crates/core/src/commit_message.rs`), also only on a click.
- **Worktrees:** `git worktree add -b newjen/<id8>-<6 random lowercase letters or digits> <path> HEAD` in `<parent>/.newjen-worktrees/<repository folder name>/`. Cancel (⌘.) while it is made stops git and removes the half-made worktree; the held message is saved first, so crash recovery restores it unsent. Archiving runs `git worktree remove --force`, `git worktree prune`, then `git branch -d`.
- **Merging a worktree** uses `git merge-tree --write-tree` as a preflight and aborts a failed `git merge --no-ff`.
- **Hooks get the login shell's PATH.** An app started from Finder has a minimal `PATH`; `Tools::with_env` passes `env::resolved_path()` to git.
- **Old data.** Migration 6 marks Rift- and jj-era threads resolved (read-only) and keeps Rift-era drafts' messages.

## Subscription safety

Users run Newjen on their own Claude and ChatGPT subscriptions. **Never do anything that could get their accounts banned.** What got other harnesses banned in 2026 was copying subscription tokens and calling the API while pretending to be Claude Code. Sources and dates are in [`docs/research/providers-and-subscriptions.md`](docs/research/providers-and-subscriptions.md); re-check its two policy links whenever the supported CLI versions change.

- **Only spawn the user's own installed, unmodified `claude` or `codex`**, and let it handle sign-in. If they aren't logged in, tell them to run `claude` or `codex login` in their own terminal.
- **Never touch credentials.** Don't read the Keychain, `~/.claude/.credentials.json` or `~/.codex/auth.json`; don't use `CLAUDE_CODE_OAUTH_TOKEN` or `claude setup-token`; don't build a login screen or proxy sign-in.
- **Never call model APIs directly**, and never fake the entrypoint, headers or system prompt.
- **Don't disguise or relabel the CLI.** `claude -p` already reports itself to Anthropic as a non-interactive (`sdk-cli`) session, so don't change that. Like T3 Code, Newjen doesn't set `CLAUDE_AGENT_SDK_CLIENT_APP` and strips any inherited value. Codex requires `clientInfo`; send the name `newjen`.
- **Respect usage limits.** Every turn starts from a user action. Never auto-retry, auto-resume at a limit reset, or auto-continue a turn. Stop on a rejected rate limit and show when it resets.
- **Say which account pays.** Show the account and plan from the CLI's own report (`claude auth status --json`, Codex `account/read`), and warn when `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` will bill the API instead.
- **Codex runs with `approvalPolicy: untrusted` and a read-only sandbox**, so every write and every non-trivial command asks, as Claude's `--permission-mode default` does. Newjen's own instructions to a thread go through each CLI's official seam: `--append-system-prompt` for Claude, `developerInstructions` on `thread/start` for Codex.
- **Updates never touch the CLIs or credentials, and never start a turn.** The updater fetches only Newjen's own releases from github.com and installs nothing whose Ed25519 signature fails.
- **No "Claude Code" in Newjen's name or UI labels.** Prose such as the README may say it "runs Claude Code".

## GPUI and GPUI Kit: don't guess APIs

Models often invent GPUI APIs because there is little public documentation and the API changes between versions. Before writing UI code:

- **Read the real GPUI Kit and GPUI source.** After the first `cargo build`, it's in `~/.cargo/registry/src/*/` under `gpui-kit-0.8.1/` (the facade), `gpui-component-0.8.1/` (styled components), `gpui-base-0.8.1/` (text view, input, popover, motion), `gpui-kit-assets-0.8.1/` (icons) and the `gpui-pre-*-0.3.10/` folders.
- **GPUI Kit's guidance:**
  - Its guides: https://gpui-kit.com
  - Its source repository: https://github.com/longbridge/gpui-kit (read only)
- **Setup** (`crates/app/src/main.rs`): call `gpui_kit::init(cx)`, then follow system appearance with `Theme::change(window.appearance(), Some(window), cx)` when opening the window and in its appearance observer. Use Kit's default fonts; no font assets or registration are needed. Open windows with `gpui_kit::open_window`, which wraps the view in Kit's `Root`: without it tooltips never show and menus lose focus tracking. Without the `font-kit` platform feature, no text renders on macOS.
- **Colors come from Kit's theme plus Newjen's material tokens; sizes come from Kit.** Read `cx.theme()` and use Kit variants, sizes, fonts and text styles. `theme.rs` holds the app layout dimensions and the material tokens (`glass_tint`, `glass_hover`, `glass_selected`, `glass_muted` for muted text on the glass, `card`, `card_stroke`, `popover`), each with a dark and a light value. `theme::apply` puts them onto Kit's theme, and `Theme::change` resets that theme, so call it after every `Theme::change`. Kit's `background` color is set to the card fill, so the card's contents share one solid. Kit's window root paints the `background` token, which carries the glass tint, so dialogs and sheets must set their own solid background (`card`). The main window opens with `WindowBackgroundAppearance::Blurred`. macOS Reduce Transparency, re-read every second, switches the window to opaque and makes the tint opaque. `popover` is always fully opaque, not translucent like the window: GPUI blurs only what is behind the window, never the card's own text behind a popover, so a see-through popover or tooltip shows that text through it (at 97% a tooltip over text was already illegible). Do not add another palette.
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
cargo test --workspace --locked       # all tests; no network or login needed (worktree and git tests need only git)
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

Use plain git:

```sh
git switch -c <branch>
git add <files>
git commit -m "message"
git push -u origin <branch>
gh pr create --base master
```

- The remote is `origin`, at `https://github.com/aelomi/newjen.git`. The default branch is `master`.
- Commit titles use [Conventional Commits](https://www.conventionalcommits.org): `type(scope): subject`, such as `fix(core): keep stdin open after an interrupt`.
- Use `gh` for PRs, issues and CI (`gh pr`, `gh issue`, `gh run`).

## Releasing

The repository doubles as the Homebrew tap. The cask lives in `Casks/newjen.rb`.
The release workflow uses the built-in `GITHUB_TOKEN` with `contents: write` for releases and cask updates.
No separate tap repository or tap token is needed; the only repository secret is `NEWJEN_SIGNING_KEY` (see In-app updates). Repository rules must allow Actions to push a cask update to `master`.

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
It signs the zip with `newjen-sign`, then publishes `Newjen-<version>-arm64.zip`, its `.sha256` and its `.sig` through `gh release create`, with the version's `CHANGELOG.md` section as the release notes.
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

### Changelog

Every user-visible change adds a line under `## Unreleased` in `CHANGELOG.md`, in one of four groups: `### New features`, `### Improvements`, `### Fixes` or `### Misc`. Write it in plain English for a user, one `- ` bullet per change. A bullet may wrap onto the next line and may use `inline code`.
Releasing renames `## Unreleased` to `## <version> · <date>`, such as `## 0.1.0-alpha.2 · 12 November 2026`, and puts a new empty `## Unreleased` above it. Do this in the release commit, before tagging, and check it with `scripts/release-notes.sh --check <version>` (a pre-tag check: it fails when the version has no dated section with notes or `## Unreleased` still has bullets). The release workflow runs the same check, and CI fails when `CHANGELOG.md` is missing or does not parse.
`CHANGELOG.md` is compiled into the app, so Newjen shows What's New offline after an update. The same section becomes the GitHub release notes (`scripts/release-notes.sh <version>`). The workflow creates the release as a draft, uploads the zip, `.sha256` and `.sig`, and only then publishes it, so the feed never lists a release without its assets.

### In-app updates

Newjen updates itself. Every hour (and 15 seconds after launch) it reads `https://github.com/aelomi/newjen/releases.atom`, never the GitHub API, which rate-limits. For a newer `v<semver>` tag (prereleases only for a copy that itself runs a prerelease) it downloads `Newjen-<version>-arm64.zip` and `Newjen-<version>-arm64.zip.sig` from the release with the system `curl`, checks the signature, unpacks the zip beside the installed app, and swaps it in after Newjen quits. The swap script writes `.Newjen-update-result` beside the app before it relaunches; the next launch reports a failed install once (with the Homebrew command and the releases page), and the quiet hourly check leaves that version alone until the feed offers a newer one. A relaunch after Restart reopens the projects that were open.
`NEWJEN_UPDATE_FEED` replaces the repository URL, such as `file:///tmp/fake-releases` for a local end-to-end test with a bundle made by `scripts/bundle-app.sh`. Builds that did not come from that script have no release version and do not update.

Releases are signed with an Ed25519 key. The public key is `UPDATE_PUBLIC_KEY` in `crates/core/src/update/signature.rs`; the private key is the `NEWJEN_SIGNING_KEY` repository secret, and the release workflow fails without it.

- **Make the key pair once:** `cargo run --release -p newjen-core --bin newjen-sign -- keygen`. It prints the private key once. Store it as the secret and in a password manager, never in the repository, then paste the printed array over `UPDATE_PUBLIC_KEY`. Until you do, every update is refused.
- **Sign by hand:** `NEWJEN_SIGNING_KEY=<hex> cargo run --release -p newjen-core --bin newjen-sign -- sign <zip>` writes `<zip>.sig`. `newjen-sign verify <zip> <public key hex>` checks one.
- **Never lose or swap the key without a bridge release.** An installed app only accepts the key it was built with. To rotate, ship a release that contains the new key but is signed with the old one (`sign --allow-other-key`), then sign later releases with the new one.
