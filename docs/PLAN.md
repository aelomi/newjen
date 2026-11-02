# Newjen: plan (2026-10-26)

Newjen is a macOS-only agent development environment: run many isolated Claude Code and Codex agents in parallel from a fast native app.

- **Location:** `~/Newjen`, with remote `https://github.com/aelomi/newjen` (GitHub, public, default branch `master`). It moved from GitCafe on 2026-11-01.
- **Audience:** personal for now, with plans to open it to contributors later. Keep the repo contributor-ready: pick an open-source license early, keep a clear README and CONTRIBUTING.md, and avoid personal paths or secrets in code. Code signing and auto-updates come later.

## v1 goal

Parallel agents and speed. The stack provides both. Workflow/UX and customizability follow from owning the code; they are not v1 features.

**Done when:** you can open a project, start several threads, have each agent work in the project or in its own git worktree, approve its tool calls, review its diff, and land or discard the work, all in a native window that stays light on memory.

## Stack

| Layer | Choice | Notes |
|---|---|---|
| Language | Rust (Cargo workspace) | Toolchain 1.99.1 pinned in `rust-toolchain.toml` |
| UI framework | GPUI: `gpui-pre =0.3.10`, `gpui-pre-platform =0.3.10` (`font-kit`) | The version GPUI Kit pins |
| Components | GPUI Kit (Longbridge): `gpui-kit =0.8.1` | Plain pinned dependency, Apache-2.0. Kit components with Kit's default theme, sizes and fonts: title bar, list items, Sheet, Popover, messages/scroller, markdown, input, buttons, tags and menus; diff in slice 4. Newjen hand-builds only the approval card, tool card and working-dot pulse, with thin behavior helpers. |
| Agent | Claude Code CLI (`claude`) over stream-json and Codex CLI (`codex app-server`), both in the MVP (decided 2026-10-31) | Newjen detects which CLIs are installed without spending usage; each thread picks its provider, model, effort and speed. API-key providers and ACP later |
| Isolation | Git worktrees, opt-in per thread (decided 2026-11-02) | By default a thread works in the project folder. Rift copies were used from 2026-10-30 to 2026-11-02 and may return as an experiment |
| Version control | Plain git (decided 2026-11-02) | jj 0.46.0 was used until then and may return as an experiment |
| Code hosting | GitHub, a normal git remote; `git push` | Moved from GitCafe on 2026-11-01: GitCafe has no releases and no hosted macOS runners, and contributors know GitHub. `gh` is logged in |
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
- **Git worktrees and git actions** (rebuilt on git on 2026-11-02; [the isolation memo](research/isolation.md) holds the research behind the Rift choice this replaces):
  - **A worktree** is `git worktree add -b newjen/<id8>-<random> <path> HEAD`, made when the first message is sent, beside the repository in `.newjen-worktrees/<repository folder name>/`. It starts at the checked-out commit. Uncommitted and gitignored files (`node_modules`, `target/`) are not in it, so each worktree rebuilds its own dependencies.
  - **It needs a git repository with at least one commit.** Otherwise the worktree choice is unavailable.
  - **Agents commit themselves**, as in T3 Code. Newjen's per-turn checkpoints are private commits under `refs/newjen/<thread id>/<n>`, built from a separate index file, used only for per-turn diffs and reverting a worktree turn. One git button commits (message from the user's own `claude`), pushes, opens a PR with `gh`, or merges a worktree branch with `git merge --no-ff` after a `git merge-tree --write-tree` check. [How it works](git.md).
  - **Archiving** runs `git worktree remove --force` and `git worktree prune`, then `git branch -d`, so an unmerged branch stays and its work can be recovered.

## Architecture

```
crates/
  core/      # no UI: agent sessions, git worktrees and landing, SQLite, events. Tested headlessly.
  app/       # GPUI + GPUI Kit shell; Kit's default themes and fonts follow system appearance.
             # Renders core state, sends user intents to core; ui/ holds the two agent cards, pulse and thin helpers.
  cli/       # (optional) small CLI that drives core, used for testing and debugging
```

- **Core owns state.** It runs one actor per thread, which owns the agent's child process (`claude` or `codex app-server`) and emits one set of provider-neutral events (`agent.rs`): text, tool use, approval needed, result, error, usage limits. Each provider has an adapter under `providers/` that translates its wire protocol into those events and back.
- **The app subscribes to those events** and renders them with Kit's own components and default theme (`cx.theme()`). Newjen owns only the approval card, tool card, working-dot pulse and thin key/focus helpers (`crates/app/src/ui/`). Replies keep lazily built `TextViewState` entities; Kit draws markdown and highlighted code blocks, and `MessageScroller` follows the tail. No bundled fonts or separate palette. `diff` comes in slice 4.
- **Thread model:** projects → threads, several projects open in one window. Each thread has its own working folder (the project folder by default, or its own git worktree when you choose one), one provider with its model, effort and speed, one agent process, and optionally one terminal. A thread always runs from the same folder, because Claude ties `--resume` to the folder a conversation started in.

## Thread lifecycle

1. **New thread:** works in the project folder unless you choose a git worktree (a choice, and it must be seamless when chosen). For a worktree, it is made when the first message is sent, and that message waits until it is ready: `git worktree add -b newjen/<id8>-<6 random lowercase letters or digits> <path> HEAD`, with the path under `<parent>/.newjen-worktrees/<repository folder name>/` beside the repository. The worktree starts at the checked-out commit; uncommitted and gitignored files are not in it. It needs a git repository with at least one commit. There are no setup hooks. The agent gets `NEWJEN_WORKTREE` (its branch) and `NEWJEN_SOURCE` (the repository root) in its environment, so dev servers can derive ports. A failed attempt gives the message back. The held message and destination are saved before git runs, and Cancel (⌘.) stops git and removes the half-made worktree. Each worktree is saved with its thread (path, branch and base commit).
2. **Run:** spawn `claude` or `codex` with the thread's working folder as its working directory, stream events, and surface approvals in the UI.
3. **Review and ship work** ([how it works](git.md)): the agent uses git as it likes. Newjen checkpoints the folder privately after each turn and shows all changes since the thread started, uncommitted changes, or one turn. The git button commits, pushes, opens a PR, or merges a worktree's branch into the project. A worktree turn can be reverted. The thread stays usable afterwards.
4. **Close:** archiving a worktree thread removes the worktree in the background (`git worktree remove --force`, `git worktree prune`) and deletes its branch with `git branch -d`, which refuses an unmerged branch so its work stays recoverable with git. A removal Newjen quit before finishing is done at the next launch.

## Approvals and agent lifecycle

Decided on 2026-10-27 after comparing Claude Code, Codex, T3 Code, MonoCode, Zeron, Paseo, Synara, Zed, Vibe Kanban, Claude Squad and Cursor ([memo](research/approvals-and-lifecycle.md)).

- **Grants live only in the session.** "Always allow" never writes to settings files (each worktree would get its own), and it remembers only what it can name exactly: CLI `addRules` suggestions whose rules all carry `ruleContent` (or a single MCP tool), or an exact Bash command without `*`. Everything else, including edits, is allowed once. The dock says which, before the click.
- **Deny and stop are separate.** Esc denies and Claude carries on; ⌘. denies and ends the turn.
- **Claude dies with Newjen.** A watcher kills claude's process group as soon as Newjen exits, and the pid registry reaps anything left at the next launch.
- **Later slices:**
  - Slice 2: grants kept per thread across resume, with a list and revoke, which no other ADE has; allocate `--session-id` before spawning; show approvals left pending by a crash as expired.
  - Slice 4: deny with a note (typed text becomes the deny message); a real diff before approving an edit; a diff too large to show means allow once.
  - Slice 5: one approvals inbox across threads, notifications only when Newjen is in the background, a Dock badge for threads waiting on you, an idle reaper that never stops a thread with a pending approval, and asking before quitting with turns running.

## Known gotchas

- **A worktree has no dependencies.** Gitignored files (`node_modules`, `target/`, virtual environments) are not in it, so each worktree rebuilds its own. This costs time and disk.
- **Port clashes** between worktrees running dev servers. Derive ports from `$NEWJEN_WORKTREE`.
- **Private checkpoint refs** live under `refs/newjen/`. A `git push --mirror` or a push of `refs/*` would publish them; ordinary branch pushes don't.
- **Orphan processes.** Kill `claude` children on crash or quit, using process groups and a cleanup check at startup.
- **No sandbox.** Worktrees isolate files, not permissions. Agents still have full access to your Mac.
- **GPUI Kit is pre-1.0.** Pin exact versions and commit `Cargo.lock`. It has no terminal and no Mermaid diagrams: Mermaid fences show as code until diagrams are built separately.
- **The "already in use" repair is timing-bound.** It needs claude's stderr line to arrive within the 500 ms drain after claude exits; a later line shows as a plain "Claude exited" error instead.
- **⌘{ and ⌘} assume a US layout.** On layouts that type braces another way, previous/next thread may not fire.
- **Reduce Motion is polled every second.** Listening for NSWorkspace's accessibility display options notification would apply it at once.
- **One Newjen instance per data folder.** Startup takes an OS lock on the folder itself before opening the store or reaping processes. A second CLI launch forwards its project or activation request over a private, versioned Unix socket and exits quietly after the running window accepts it. Handoff has a bounded timeout. macOS file-open and reopen events use the same window. Only the lock holder owns and cleans up the socket, including after a crash. The lock releases on exit or crash; separate `NEWJEN_HOME` folders can run in parallel.
- **Desktop notifications need an app bundle.** `cargo run` has no bundle id, so only the Dock badge works; a quiet line above the sidebar's usage row says so. Authorization also failed on the development Mac even from a temporary bundle, so banners are unverified.
- **A Codex denial carries no note.** The typed note stays in the composer to send as a message.
- **The terminal stops its shell with SIGKILL** on close, archive and quit; a bounded graceful stop is a follow-up. Detached daemons that ignore the PTY hangup can outlive a crash.

## Build order (one vertical slice first)

1. **Slice (done 2026-10-27):** `cargo run` opens a window → type a prompt → Claude replies over stream-json → the reply renders in the chat, with approve/deny working. One thread, no worktrees, no landing.
2. **Done 2026-10-31.** Threads list, SQLite persistence, resume. Decided 2026-10-30 ([memo](research/threads-and-persistence.md)):
   - **Store:** `~/Library/Application Support/Newjen/newjen.db` (`NEWJEN_HOME` overrides it), WAL, migrations by `user_version`. Tables: projects, threads (with `provider`, the Claude session id and an unread flag), rendered transcript items, and grants. Writes run off the UI thread, at checkpoints only, never per streamed token.
   - **Resume:** the Claude session id is allocated when the thread is created: `--session-id` until `init` arrives, `--resume` after. Each thread stores its own working folder and always spawns from it, so slice 3 only has to set it to a worktree.
   - **Relaunch:** approvals left pending show as expired and running work as interrupted. Continuing is manual; nothing restarts on its own.
   - **Processes:** a thread's `claude` keeps running when you switch away, and stops after 30 minutes idle, never mid-turn or with an approval pending. Quitting stops all of them.
   - **Grants:** "Always allow" rules are saved per thread and re-applied at every spawn with `--allowedTools`, so the dock says "for this thread". They can be listed and revoked.
   - **List:** new threads on top, a status per thread (needs approval, working, failed, stopped, unread), titles from the first message (no hidden model calls). ⌘N opens a draft that is saved only once its first message is sent, so empty threads never pile up. The sidebar hides below 760 px of window width (⌘B toggles it) and shows a dot when a hidden thread needs you. Mocks: `design/newjen.pen` screens 8–12.
   - **Deferred:** deny with a note. The approval dock replaces the composer, so it needs its own text field; it moves to slice 4 with the diff view.
3. **Done 2026-10-31, rebuilt on git worktrees 2026-11-02.** Isolated copies of the project, chosen per thread when you create it. The default stays the project folder. Landing (slice 4) handles both kinds of thread. The slice was first built on Rift copies with postcreate hooks. On 2026-11-02 it was rebuilt on git worktrees, which every other ADE checked uses ([memo](research/isolation.md)).
4. **Done 2026-10-31, rebuilt on git 2026-11-02.** Checkpoints, diff view (GPUI Kit `diff`) and git actions ([how it works](git.md)). The slice was first built on jj with two landing modes (**Harness manages jj** and **Agent manages jj**). On 2026-11-02 it moved to git, first with one mode where only Newjen committed and the agent was told not to, then, the same day, to T3 Code's model at the owner's request: the agent uses git freely, Newjen keeps private per-turn checkpoints for diffs and undo, and one git button commits (AI message from the user's own `claude`), pushes, opens a PR or merges a worktree branch. Edit approvals show the before-and-after diff and recheck the file before allowing; a denial can carry a note (Claude receives it as the deny message; Codex's decision carries no text, so the note stays in the composer).
5. **Done 2026-10-31.** One approvals inbox across every loaded project (Allow once and Deny; grants stay in the thread's dock), a Dock badge counting threads waiting on you, notifications only when Newjen is in the background, a per-thread terminal panel on `alacritty_terminal` and `portable-pty`, and one quit prompt for ⌘Q, ⌘W and the native quit that counts running turns, approvals, background tasks, worktree preparation, snapshots and terminals ([memo](research/slice5-lifecycle.md)).
6. **Done 2026-10-31, in the MVP** (decided 2026-10-31; the plan had put it after the MVP). Providers: Newjen detects the installed agent CLIs without spending usage ([memo](research/provider-detection.md)), drives Claude over stream-json and Codex over `codex app-server` ([memo](research/codex-codec.md)) through one neutral event and approval model, and each new thread picks its agent, model, effort and speed in the composer; the last choice becomes the project's default. It shows the account and plan, warns about API keys in the environment, and pauses sending when a usage limit is rejected until the reset time (Codex can confirm an early reset with Refresh; Claude cannot report its limits without spending usage). Codex approvals keep the exact-scope rule. Nothing retries or resumes on its own.

**Also done 2026-10-31:** opening and switching projects in one window, with recent projects and a welcome screen ([memo](research/opening-projects.md)); one Newjen per data folder; cancelling a worktree while it is made; a broken connection to the agent ends its session at once so the next message starts a fresh one.

**Planned next:**

- **Disk use per thread.** Show each thread's real disk cost (a worktree rebuilds its own `target/` or `node_modules`), warn before making a worktree when disk is low, and offer "free this thread's build output" when a thread finishes.
- **Rift copies and jj, as an experiment.** The 2026-11-02 decision (below) says they may return once the build is solid.
- **Landing for plain folders that are not git repositories**, per the memos above.
- **Switching a thread's provider with a context handoff**, as T3 Code does.

## Subscription safety

Users run Newjen on their own Claude and ChatGPT subscriptions, so it must never put their accounts at risk. Newjen spawns only the user's own installed, unmodified CLIs; it never reads or stores credentials, never calls model APIs itself, never offers its own login and never retries around usage limits: a rejected limit pauses sending until the reset the CLI reported. It never disguises the CLI. Like T3 Code, it adds no Claude app label; Codex gets the `clientInfo` it requires. Switching a thread's provider with a context handoff, as T3 Code does, is planned for after the MVP. The rules and their sources are in [the memo](research/providers-and-subscriptions.md#2-subscription-safety) and in `CONTRIBUTING.md`.

## Appearance

Kit's default light and dark themes follow the system appearance. Components keep Kit's default sizes and fonts.

## Testing

- Record real stream-json sessions as fixture files and replay them in core tests, so agents can verify their own changes without the UI.
- Use the CLI crate for headless end-to-end checks.

## Open questions

- **Decided 2026-11-02: git worktrees and plain git replace Rift copies and jj.** The owner wants to nail this build on the common git-worktree model first, before experimenting with Rift and jj. Slices 3 and 4 were first built on Rift and jj (2026-10-31) and rebuilt on git. Rift and jj may return later as an experiment. Threads from the Rift and jj era are marked resolved by a database migration, and their Rift copies stay on disk.
- **Decided 2026-11-02: agents do version control themselves, as in T3 Code.** The first git rebuild told the agent not to commit and committed only on Land; the owner rejected that ("what is the point if the agent can't do that for me?") and asked for T3 Code's model, read from its source (`pingdotgg/t3code` at `d890e5c`). Newjen improves on it with a local **Merge** for worktree threads, for repositories without a remote.

- None blocking. All five MVP slices and providers are built (2026-10-27 to 2026-10-31). Ely was replaced with GPUI Kit on 2026-10-31. Next: the planned items above.
- **Rift gaps found in slice 3 (historical):** a copy folder deleted by hand, or removed directly after its `.rift` marker was lost (including `git clean -fdx`), stayed in Rift's registry until the user ran `rift gc`. Newjen deleted its own trash entry instead of running `rift gc`. Newjen no longer makes or removes Rift copies; old ones stay on disk (remove one with `rift remove <path>`).
