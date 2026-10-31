# Newjen: plan (2026-10-26)

Newjen is a macOS-only agent development environment: run many isolated Claude Code agents in parallel from a fast native app.

- **Location:** `~/Newjen`, with remote `https://git.cafe/ericrasputin/newjen` (GitCafe, public, default branch `master`).
- **Audience:** personal for now, with plans to open it to contributors later. Keep the repo contributor-ready: pick an open-source license early, keep a clear README and CONTRIBUTING.md, and avoid personal paths or secrets in code. Code signing and auto-updates come later.

## v1 goal

Parallel agents and speed. The stack provides both. Workflow/UX and customizability follow from owning the code; they are not v1 features.

**Done when:** you can open a project, start several threads, have each agent work in the project or in its own Rift copy, approve its tool calls, review its diff, and land or discard the work, all in a native window that stays light on memory.

## Stack

| Layer | Choice | Notes |
|---|---|---|
| Language | Rust (Cargo workspace) | Toolchain 1.98.1 installed |
| UI framework | GPUI: `gpui-pre =0.3.10`, `gpui-pre-platform =0.3.10` (`font-kit`) | The version GPUI Kit pins |
| Components | GPUI Kit (Longbridge): `gpui-kit =0.8.1` | Plain pinned dependency, Apache-2.0. Kit components with Kit's default theme, sizes and fonts: title bar, list items, Sheet, Popover, messages/scroller, markdown, input, buttons, tags and menus; diff in slice 4. Newjen hand-builds only the approval card, tool card and working-dot pulse, with thin behavior helpers. |
| Agent | Claude Code CLI (`claude`) over stream-json; Codex CLI (`codex app-server`) next | Newjen detects which CLIs are installed; one provider per thread. API-key providers and ACP later |
| Isolation | Rift (`rift-snapshot`), an APFS copy-on-write copy, opt-in per thread | Installed. By default a thread works in the project folder; no git worktrees for now |
| Version control | jj 0.46.0, colocated with git | Installed |
| Code hosting | GitCafe (git.cafe), a normal git remote; `jj git push` | `cafe` 0.11.0 installed via Bun 1.4.4. Still needed: run `cafe auth login`. Private repos are free during the beta |
| Storage | SQLite (threads, settings); Claude keeps its own transcripts | Resume with `--resume <session-id>` |
| Platform | macOS only | No signing or notarization until it's shared |

## Verified on this Mac (2026-10-26)

- **GPUI builds and initialises.** The first debug build took 1m38s and the `target/` folder reached 3.5 GB. Later builds are incremental.
- **GPUI Kit streams replies as smoothly as Ely did** (2026-10-31). A 7.7 KB Markdown reply streamed in 744 pieces, in release builds: the main thread spent 17% of the time drawing (Ely: 19%) at the same frame rate, with the same CPU and memory. Debug builds draw the same frames but keep the main thread busy; setting `opt-level = 2` for dependencies in the dev profile brings that to 23%, at the cost of a slower first build.
- **Claude approvals work over stdio.** Launch the CLI like this:

  ```
  claude -p --verbose --input-format stream-json --output-format stream-json \
         --permission-prompt-tool stdio --permission-mode default [--resume <id>]
  ```

  - Each tool call that needs approval arrives as `{"type":"control_request","request":{"subtype":"can_use_tool","tool_name":…,"input":…}}`.
  - Answer with `{"type":"control_response","response":{"subtype":"success","request_id":…,"response":{"behavior":"allow","updatedInput":…}}}`, or `"behavior":"deny"`.
  - No MCP server is needed. Keep stdin open for the whole session.
- **Rift + jj work together**, with three rules:
  - **Use `rift create --copy-all`.** By default Rift skips gitignored files such as `node_modules` and build output, which removes the main benefit.
  - **Run `jj new` in the copy before the agent starts.** Otherwise the copy keeps editing the same jj change as the original, and fetching it back creates a *divergent* change. With a new change first, the fetched work shows only the agent's diff, sitting on top of the original's uncommitted state.
  - **Land work with fetch:** run `jj git remote add <thread> <copy path>` and `jj git fetch --remote <thread>` in the main repo, and the thread's bookmark appears as `thread/<id>@<thread>`. This was verified.
- **Harmless jj warning:** jj notes "repo appears to have been copied" the first time it runs in a copy.
- **`rift remove` takes the copy's path, not its name.** Run `rift gc` afterwards.

## Architecture

```
crates/
  core/      # no UI: agent sessions, Rift, jj, SQLite, events. Tested headlessly.
  app/       # GPUI + GPUI Kit shell; Kit's default themes and fonts follow system appearance.
             # Renders core state, sends user intents to core; ui/ holds the two agent cards, pulse and thin helpers.
  cli/       # (optional) small CLI that drives core, used for testing and debugging
```

- **Core owns state.** It runs one actor per thread, which owns the `claude` child process and emits typed events: text, tool use, approval needed, result, error.
- **The app subscribes to those events** and renders them with Kit's own components and default theme (`cx.theme()`). Newjen owns only the approval card, tool card, working-dot pulse and thin key/focus helpers (`crates/app/src/ui/`). Replies keep lazily built `TextViewState` entities; Kit draws markdown and highlighted code blocks, and `MessageScroller` follows the tail. No bundled fonts or separate palette. `diff` comes in slice 4.
- **Thread model:** project → threads. Each thread has its own working folder (the project folder by default, or its own Rift copy when you choose one), one Claude process and one landing mode. A thread always runs from the same folder, because Claude ties `--resume` to the folder a conversation started in.

## Thread lifecycle

1. **New thread:** works in the project folder unless you choose a Rift copy (decided 2026-10-30: Rift is a choice, like worktrees are elsewhere, and must be seamless when chosen). For a Rift copy: snapshot the original (`jj status`), run `rift create --copy-all`, then `jj new` in the copy. Rift `postcreate` hooks run per-copy setup, such as rebuilding venvs or starting services named with `$RIFT_ID`.
2. **Run:** spawn `claude` with the copy as its working directory, stream events, and surface approvals in the UI.
3. **Land work** using one of two modes, chosen per thread with a project default:
   - **Harness-managed:** the agent is told not to commit. At the end of each turn the harness snapshots the copy with jj (`jj describe`/`jj new`). You review the diff in the app, then:
     - **Land:** fetch the copy's change into the main repo as bookmark `thread/<id>`, then rebase or merge.
     - **Discard:** remove the copy.
   - **Agent-managed:** the harness never writes to version control in the copy, and reads only with `--ignore-working-copy`. Landing fetches whatever bookmarks the agent created.
4. **Close:** `rift remove <copy path>`, then `jj git remote remove <thread>` in the main repo, and later `rift gc`.

## Approvals and agent lifecycle

Decided on 2026-10-27 after comparing Claude Code, Codex, T3 Code, MonoCode, Zeron, Paseo, Synara, Zed, Vibe Kanban, Claude Squad and Cursor ([memo](research/approvals-and-lifecycle.md)).

- **Grants live only in the session.** "Always allow" never writes to settings files (each Rift copy would get its own), and it remembers only what it can name exactly: CLI `addRules` suggestions whose rules all carry `ruleContent` (or a single MCP tool), or an exact Bash command without `*`. Everything else, including edits, is allowed once. The dock says which, before the click.
- **Deny and stop are separate.** Esc denies and Claude carries on; ⌘. denies and ends the turn.
- **Claude dies with Newjen.** A watcher kills claude's process group as soon as Newjen exits, and the pid registry reaps anything left at the next launch.
- **Later slices:**
  - Slice 2: grants kept per thread across resume, with a list and revoke, which no other ADE has; allocate `--session-id` before spawning; show approvals left pending by a crash as expired.
  - Slice 4: deny with a note (typed text becomes the deny message); a real diff before approving an edit; a diff too large to show means allow once.
  - Slice 5: one approvals inbox across threads, notifications only when Newjen is in the background, a Dock badge for threads waiting on you, an idle reaper that never stops a thread with a pending approval, and asking before quitting with turns running.

## Known gotchas

- **Copied absolute paths.** Python virtual environments and some build caches embed absolute paths. Use Rift `postcreate` hooks to rebuild them.
- **Port clashes** between copies running dev servers. Derive ports from `$RIFT_ID`.
- **jj ignores git hooks**, so husky and pre-commit won't run. Any checks they did must run from the harness.
- **UI reads must not snapshot the working copy.** Always pass `--ignore-working-copy` when the UI polls jj.
- **Orphan processes.** Kill `claude` children on crash or quit, using process groups and a cleanup check at startup.
- **No sandbox.** Rift copies isolate files, not permissions. Agents still have full access to your Mac.
- **GPUI Kit is pre-1.0.** Pin exact versions and commit `Cargo.lock`. It has no terminal and no Mermaid diagrams: Mermaid fences show as code until diagrams are built separately.
- **The "already in use" repair is timing-bound.** It needs claude's stderr line to arrive within the 500 ms drain after claude exits; a later line shows as a plain "Claude exited" error instead.
- **⌘{ and ⌘} assume a US layout.** On layouts that type braces another way, previous/next thread may not fire.
- **Reduce Motion is polled every second.** Listening for NSWorkspace's accessibility display options notification would apply it at once.
- **Two Newjen instances on one project aren't prevented.** Both would drive the same threads and store; Codex and T3 Code hold a lock, and Newjen should too.

## Build order (one vertical slice first)

1. **Slice (done 2026-10-27):** `cargo run` opens a window → type a prompt → Claude replies over stream-json → the reply renders in the chat, with approve/deny working. One thread, no Rift, no jj.
2. **Done 2026-10-31.** Threads list, SQLite persistence, resume. Decided 2026-10-30 ([memo](research/threads-and-persistence.md)):
   - **Store:** `~/Library/Application Support/Newjen/newjen.db` (`NEWJEN_HOME` overrides it), WAL, migrations by `user_version`. Tables: projects, threads (with `provider`, the Claude session id and an unread flag), rendered transcript items, and grants. Writes run off the UI thread, at checkpoints only, never per streamed token.
   - **Resume:** the Claude session id is allocated when the thread is created: `--session-id` until `init` arrives, `--resume` after. Each thread stores its own working folder and always spawns from it, so slice 3 only has to set it to a Rift copy.
   - **Relaunch:** approvals left pending show as expired and running work as interrupted. Continuing is manual; nothing restarts on its own.
   - **Processes:** a thread's `claude` keeps running when you switch away, and stops after 30 minutes idle, never mid-turn or with an approval pending. Quitting stops all of them.
   - **Grants:** "Always allow" rules are saved per thread and re-applied at every spawn with `--allowedTools`, so the dock says "for this thread". They can be listed and revoked.
   - **List:** new threads on top, a status per thread (needs approval, working, failed, stopped, unread), titles from the first message (no hidden model calls). ⌘N opens a draft that is saved only once its first message is sent, so empty threads never pile up. The sidebar hides below 760 px of window width (⌘B toggles it) and shows a dot when a hidden thread needs you. Mocks: `design/newjen.pen` screens 8–12.
   - **Deferred:** deny with a note. The approval dock replaces the composer, so it needs its own text field; it moves to slice 4 with the diff view.
3. Rift copies, chosen per thread when you create it, with postcreate hooks. The default stays the project folder. Landing (slice 4) must handle both kinds of thread.
4. jj snapshots, diff view (GPUI Kit `diff`), and both landing modes.
5. Parallel threads, notifications (Dock badge), and a terminal panel (on a terminal crate such as `alacritty_terminal`; GPUI Kit has none).

**After the MVP slices: providers.** Newjen detects the installed agent CLIs (`claude`, `codex`) and drives whichever one a thread uses. Detection must not spend usage, the core gets one event model with a Claude adapter and a Codex adapter, and Codex approvals keep the exact-scope rule. This step also shows the account and plan, warns about API keys in the environment, and respects usage limits. Decided 2026-10-30 ([memo](research/providers-and-subscriptions.md)); it runs in its own session once the MVP slices are done.

## Subscription safety

Users run Newjen on their own Claude and ChatGPT subscriptions, so it must never put their accounts at risk. Newjen spawns only the user's own installed, unmodified CLIs; it never reads or stores credentials, never calls model APIs itself, never offers its own login and never retries around usage limits. It never disguises the CLI. Like T3 Code, it adds no Claude app label; Codex gets the `clientInfo` it requires. Switching a thread's provider with a context handoff, as T3 Code does, is planned for after the MVP. The rules and their sources are in [the memo](research/providers-and-subscriptions.md#2-subscription-safety) and in `CONTRIBUTING.md`.

## Appearance

Kit's default light and dark themes follow the system appearance. Components keep Kit's default sizes and fonts.

## Testing

- Record real stream-json sessions as fixture files and replay them in core tests, so agents can verify their own changes without the UI.
- Use the CLI crate for headless end-to-end checks.

## Open questions

- None blocking. Slices 1 and 2 are built (2026-10-27, 2026-10-31). Ely was replaced with GPUI Kit on 2026-10-31. Next: slice 3, Rift copies chosen per thread.
