# Newjen: plan (2026-10-26)

Newjen is a macOS-only agent development environment: run many isolated Claude Code agents in parallel from a fast native app.

- **Location:** `~/Newjen`, with remote `https://git.cafe/ericrasputin/newjen` (GitCafe, public, default branch `master`).
- **Audience:** personal for now, with plans to open it to contributors later. Keep the repo contributor-ready: pick an open-source license early, keep a clear README and CLAUDE.md, and avoid personal paths or secrets in code. Code signing and auto-updates come later.

## v1 goal

Parallel agents and speed. The stack provides both. Workflow/UX and customizability follow from owning the code; they are not v1 features.

**Done when:** you can open a project, start several threads, have each agent work in its own copy, approve its tool calls, review its diff, and land or discard the work, all in a native window that stays light on memory.

## Stack

| Layer | Choice | Notes |
|---|---|---|
| Language | Rust (Cargo workspace) | Toolchain 1.98.1 installed; Ely needs ≥ 1.95 |
| UI framework | GPUI: `gpui-pre =0.3.10`, `gpui-pre-platform =0.3.10` (`font-kit`) | Version fixed by Ely |
| Components | Ely: `ely-gpui-component =0.1.1` | Plain pinned dependency; chat, agent, terminal, editor and git (diff) components |
| Agent | Claude Code CLI (`claude` 2.1.310 installed) over stream-json | Claude only in v1 |
| Isolation | Rift (`rift-snapshot`), one APFS copy-on-write copy per thread | Installed |
| Version control | jj 0.46.0, colocated with git | Installed |
| Code hosting | GitCafe (git.cafe), a normal git remote; `jj git push` | `cafe` 0.11.0 installed via Bun 1.4.4. Still needed: run `cafe auth login`. Private repos are free during the beta |
| Storage | SQLite (threads, settings); Claude keeps its own transcripts | Resume with `--resume <session-id>` |
| Platform | macOS only | No signing or notarization until it's shared |

## Verified on this Mac (2026-10-26)

- **Ely and GPUI build and initialise.** The first debug build took 1m38s and the `target/` folder reached 3.5 GB. Later builds are incremental.
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
  app/       # GPUI + Ely shell; renders core state, sends user intents to core.
  cli/       # (optional) small CLI that drives core, used for testing and debugging
```

- **Core owns state.** It runs one actor per thread, which owns the `claude` child process and emits typed events: text, tool use, approval needed, result, error.
- **The app subscribes to those events** and renders them with Ely (`chat`, `agent` permissions, `git` diff viewer).
- **Thread model:** project → threads. Each thread has one Rift copy, one Claude process and one landing mode.

## Thread lifecycle

1. **New thread:** snapshot the original (`jj status`), run `rift create --copy-all`, then `jj new` in the copy. Rift `postcreate` hooks run per-copy setup, such as rebuilding venvs or starting services named with `$RIFT_ID`.
2. **Run:** spawn `claude` with the copy as its working directory, stream events, and surface approvals in the UI.
3. **Land work** using one of two modes, chosen per thread with a project default:
   - **Harness-managed:** the agent is told not to commit. At the end of each turn the harness snapshots the copy with jj (`jj describe`/`jj new`). You review the diff in the app, then:
     - **Land:** fetch the copy's change into the main repo as bookmark `thread/<id>`, then rebase or merge.
     - **Discard:** remove the copy.
   - **Agent-managed:** the harness never writes to version control in the copy, and reads only with `--ignore-working-copy`. Landing fetches whatever bookmarks the agent created.
4. **Close:** `rift remove <copy path>`, then `jj git remote remove <thread>` in the main repo, and later `rift gc`.

## Known gotchas

- **Copied absolute paths.** Python virtual environments and some build caches embed absolute paths. Use Rift `postcreate` hooks to rebuild them.
- **Port clashes** between copies running dev servers. Derive ports from `$RIFT_ID`.
- **jj ignores git hooks**, so husky and pre-commit won't run. Any checks they did must run from the harness.
- **UI reads must not snapshot the working copy.** Always pass `--ignore-working-copy` when the UI polls jj.
- **Orphan processes.** Kill `claude` children on crash or quit, using process groups and a cleanup check at startup.
- **No sandbox.** Rift copies isolate files, not permissions. Agents still have full access to your Mac.
- **Ely is early.** Its README says it is 0.x and written by agents ("no crate is stable"). Pin exact versions and commit `Cargo.lock`.

## Build order (one vertical slice first)

1. **Slice:** `cargo run` opens a window → type a prompt → Claude replies over stream-json → the reply renders in Ely chat, with approve/deny working. One thread, no Rift, no jj.
2. Threads list, SQLite persistence, resume.
3. One Rift copy per thread, with postcreate hooks.
4. jj snapshots, diff view (`ely-git`), and both landing modes.
5. Parallel threads, notifications (Dock badge), and a terminal panel (`ely-terminal`).

## Testing

- Record real stream-json sessions as fixture files and replay them in core tests, so agents can verify their own changes without the UI.
- Use the CLI crate for headless end-to-end checks.

## Open questions

- None blocking. Next: build slice 1.
