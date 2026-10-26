# Decision memo: approvals and process lifecycle for Newjen (2026-10-27)

A comparison of how other agent development environments handle tool approvals and agent processes, written before settling slice 1's "Always allow" policy. The decisions taken from it are summarized in [`docs/PLAN.md`](../PLAN.md#approvals-and-agent-lifecycle). Product details reflect their public docs and source as of this date.

**Verdict on slice 1:** keep the current policy, and fix one gap in it. "Always allow" can still grant a whole tool, because it forwards any `addRules` suggestion the CLI sends.

## 1. Comparison

| Product | Approval choices | Session / always scope: shell · edits · other | Persistence | Mode/grant shown · revoked | Diff before edit approval | Multiple pending | Lifecycle: run / stop / crash / resume |
|---|---|---|---|---|---|---|---|
| **Claude Code TUI** 2.1.310 | Yes · Yes + don't ask (prefix) · Yes + auto mode · No; Tab adds a note; Esc stops the turn | Persistent prefix rule, up to 5 per compound command · session `acceptEdits` (also rm/mv/mkdir/sed) · WebFetch domain, bare MCP tool rule | `.claude/settings.local.json` at repo root; session rules in memory | Status-bar mode; `/permissions` lists rules with their source · delete there; whether session rules can be deleted is unknown | Yes; too large to show → once only | Undocumented (VS Code queues) | In-process / Esc, SIGTERM / no sweep for SDK sessions / `--resume`, session grants lost |
| **Codex** | Yes · prefix rule always · No = cancel the turn (decline exists only in the protocol) | Exact argv+cwd for session; prefix in `~/.codex/rules` (all projects, runs unsandboxed) · per file · network host: once / conversation / forever | Memory; rules and config files | `/status` · session grants can't be listed or revoked | Diff in transcript first (`item/started`); modal shows paths only | LIFO (`approval_overlay.rs`) | Own loop over app-server / `turn/interrupt` + kill process group / no sweep, PDEATHSIG is Linux-only / `thread/resume` |
| **T3 Code** | Approve · Decline; overflow menu: Always this session · Cancel (deny + interrupt); typing declines with feedback | Claude: CLI suggestions → `session`, falling back to the **whole tool** · Codex: every command or every edit in the thread | Memory only | Mode dropdown · no grant list | No (path only) | One at a time, 1/N | Agent SDK / interrupt then close / fd-3 watcher shim plus startup reconcile (approvals expire visibly) / `--session-id` + `resumeSessionAt` |
| **MonoCode** | Allow · Deny (fixed message) | None; mode only | Mode in SQLite | Composer pill · switch mode (respawns) | Yes, old/new snippet; Write shows content only | All inline, any order | Raw stream-json / deny pending + `stop_task` + interrupt / env-marker sweep at startup / `--session-id`, auto "Continue" |
| **Zeron** (shipped) | None: everything auto-allowed (PRs #715/#722 open) | n/a | n/a | n/a | No (Changes pane afterwards) | Questions only, one at a time | stream-json / interrupt → 2 s TERM → 3 s KILL / journal recovery, no orphan sweep / `--resume` |
| **Paseo** | Accept · Deny; typing = deny with feedback + steer | None (suggestions ignored) | Mode in agent JSON | Shield icon, Shift+Tab · no grant list | Yes, in the card | All stacked; CLI `--all` | Agent SDK / interrupt with ack contract / record repair only / SDK resume |
| **Synara** | Once · Always this session · Decline · Cancel turn; keys 1–4 | Always → **all tools, and the thread becomes Full access** (Codex: sandbox off); MCP stays narrow | Thread mode persisted | Access menu · switch mode (restarts) | No | One at a time, 1/N; Hub can answer | Agent SDK / 10 s interrupt ack / boot reconcile, no sweep / stored resume cursor |
| **Zed + claude-agent-acp** | Yes · Yes, always (label built from suggestions) · No; cmd-y / cmd-alt-y / cmd-alt-z; no feedback | Claude's own rules | Claude settings (path under SDK unknown) | Mode selector · edit settings | Yes (adapter `diff.ts`) | Global FIFO across sessions | ACP → SDK → CLI / `session/cancel` / setsid + SIGKILL group on drop, no sweep / `session/load` |
| **Vibe Kanban** | Approve · Deny with reason; 10 h timeout → deny + interrupt | None | Memory | Variant dropdown | old/new snippet | One per process (reader blocks) | One process per turn / interrupt → 5 s → INT → TERM → KILL / DB rows marked failed, no kill / `--resume-session-at` |
| **Claude Squad** | Claude TUI in tmux; AutoYes presses Enter | Claude's own | Claude's own | Claude's own | Claude's own | Per pane; no "waiting" status | tmux PTY / none / tmux survives / no `--resume` |
| **Cursor** | Labels unknown; Auto-review / Allowlist / Run Everything | Allowlisted shell, MCP, fetch · edits not gated (inferred) | `permissions.json` (user and project) | Settings | No (checkpoints afterwards) | Unknown | Unknown |
| **Newjen slice 1 (today)** | Allow once · Always allow · Deny; ⌘. = deny + stop | CLI `addRules` → `session`, else exact Bash command (no `*`) · edits once only · other: any `addRules` forwarded | CLI memory | Note shown after the click if it fell back · no list | No (file name) | FIFO, "1 of N", 300 ms key guard | Raw stream-json, process group / interrupt, no ack deadline / pid + start-time reap at launch / `--resume` |

## 2. What the best do well, and where everyone falls short

**Do well:**
- **Scope comes from structured suggestions and is labelled exactly.** Claude Code and claude-agent-acp build their labels from `permission_suggestions`. Zed refuses patterns for `./script` (`pattern_extraction.rs`).
- **No standing grant unless the prompt shows everything it covers.** Claude Code makes approval one-time only when the diff is too large (permissions.md L33).
- **"Always" goes to `session` only, never to settings files** (T3 binCli 200510).
- **Decline and Cancel are separate.** Codex `bespoke_event_handling.rs` and T3 split "deny and continue" from "deny and stop".
- **Deny with feedback.** TUI Tab, Paseo `session.ts` L4030, Vibe Kanban's reason field.
- **Diff inside the prompt.** TUI, claude-agent-acp `diff.ts`, Paseo.
- **Safe keys.** Codex holds the modal until 1 s of typing idle; Synara's digit keys work only with focus in the card.

**Where all of them fall short:**
- **Nobody lists or revokes session grants.** Codex, T3, Paseo, Synara and Zed all have none.
- **Grants vanish silently on restart** (T3, Synara, Codex), so behaviour after recovery differs from before.
- **"Accept edits" labels hide rm/mv/sed** everywhere it is exposed (MonoCode, T3, claude-agent-acp, Paseo).
- **Some over-broaden.** One Synara click gives Full access; T3 Codex allows every command; T3's fallback allows the whole tool.
- **Pending approvals die with the process.** Only T3 shows them as expired instead of silently dropping them.
- **Most have no orphan sweep.** Only MonoCode, T3 and Newjen have one.

## 3. Recommendation

### (a) Slice 1 now: keep, with these extensions

**Why keep.** The policy already does the two strongest things in the research: grants go to `session` only, and edits get no grant because `setMode acceptEdits` is unsafe. Session-only also matters for slice 3: each Rift copy would otherwise get its own `.claude/settings.local.json`. Esc = deny-and-continue with ⌘. = deny-and-stop matches Codex's Decline/Cancel split. Also keep the FIFO queue, the "1 of N" counter, the 300 ms key guard and the pinned `--permission-mode default` (MonoCode and Synara pin it too).

**Change and extend:**
1. **Fix the whole-tool gap.** `session_permissions` (`crates/core/src/protocol.rs` ~645–656) forwards every `addRules` suggestion and only rewrites its destination. Its own doc comment says whole-tool rules are too broad, and the only test covers the case with no suggestions. Forward a suggestion only if:
   - every rule has a `ruleContent` and `behavior: "allow"`, or
   - it is a bare `toolName` for an `mcp__` tool (one MCP tool, the narrow case the TUI and Synara treat as safe).
   
   No local fixture shows a bare-rule suggestion yet; the evidence is the TUI binary strings in the research. So the first step is to record a WebSearch and an MCP fixture.
2. **Show the scope before the click.** Ely's prompt has a title, a detail text and three fixed buttons, so reserve one detail row the way `purpose` does:
   - "Always allow → remembers `touch a.txt` for this session"
   - "Always allow → `npm test *` (any arguments) for this session"
   - "Always allow = once here (no safe rule for edits)"
   
   Keep the note shown after the click as a fallback. This fixes T3's main weakness ("the button never shows which scope").
3. **Downgrade to once when the command was cut.** If `fit_rows` trimmed the command, "Always allow" acts as allow once. This mirrors the TUI rule for diffs too large to show.
4. **Show `blocked_path`** (present in `tool_allow.jsonl`) in the detail when it is outside the project. Parse `suppressAlwaysAllowRule` and `defaultToNo` defensively if they ever appear; none of our 2.1.310 fixtures carry them.
5. **Handle withdrawn and unknown control requests.** `transcript.rs:273` ignores both today.
   - `control_cancel_request` (Python SDK `query.py` L420): drop the matching pending request and mark the tool cancelled.
   - Any other control-request subtype: answer with an error `control_response` so the CLI never stalls (Zeron `claude/mod.rs:1728-1790`).
   
   Neither is in our fixtures. Newjen sends no hooks or SDK MCP servers, so they may never arrive today. Verify with a fixture first.

### (b) Later slices

- **Slice 2:**
  - Deny with feedback: keep the composer visible while an approval is pending, and send typed text as the deny `message` (Paseo, T3, Vibe Kanban).
  - A visible mode chip whose `acceptEdits` description honestly says "also runs rm/mv/mkdir/sed".
  - A **list of session grants, with revoke**. No product has this; it is where Newjen can do better than all of them. Core already knows every `updatedPermissions` it sent, so store them per thread in SQLite. To carry grants across `--resume` and to revoke them, re-pass them as `--allowedTools` on respawn, or use `removeRules`. **No product does either, so the mechanism is unverified: test it against 2.1.310 before designing UI on it.**
  - Pending approvals shown as "expired" after a restart (T3 `responseCapability`), never silently dropped.
- **Slice 3:** never accept `localSettings` destinations, because each Rift copy has its own `.claude`.
- **Slice 4:** a real diff before approving:
  - For Edit, build it from old/new against the file on disk.
  - For Write, compare against the existing file (MonoCode only shows the new content).
  - Render with Ely `git` components. A diff too large to show means once only.
  - After the turn, a per-turn jj diff with line comments sent back to Claude (Vibe Kanban, Zeron).
- **Slice 5:**
  - One approvals inbox across threads (Zed's global FIFO; Codex's thread label plus jump key; Synara's ⌘⌥A).
  - Notifications only when the window is unfocused, with approvals outranking "done" (Codex).
  - Dock badge = number of threads awaiting input (MonoCode, Paseo).
  - Make it visible somewhere that the user's own `~/.claude` allow rules auto-approve tools without a prompt (Paseo and MonoCode hide this).

## 4. Process-lifecycle practices Newjen lacks

1. **Kill children as soon as Newjen dies.** The orphan registry only reaps at the next launch. Between a crash and the relaunch, the CLI can keep running background work for up to 10 minutes after stdin closes (headless.md). T3's fd-3 watcher shim (binCli 198806) closes that window: `sh -c '{ read <&3; kill -TERM 0; sleep 5; kill -KILL 0; } & exec "$@" 3<&-'`.
2. **Give interrupt an ack deadline.** `Session::interrupt()` writes the request and returns. The init fixture advertises `interrupt_receipt_v1`, so wait for the receipt (Synara 10 s, Vibe Kanban 5 s), then escalate through the signal steps `shutdown` already has. Paseo's rule is worth copying: after an ambiguous interrupt, accept no new work.
3. **Hard stop vs soft stop.** Stop should also stop background tasks (MonoCode sends `stop_task`; Zeron kills a snapshot of descendants).
4. **`--session-id` allocated before spawn** (T3, MonoCode, Synara), so the thread-to-session mapping is durable before the first event. Slice 2.
5. **Startup reconciliation.** Keep an in-flight table (MonoCode `in_flight_sessions`, Zeron run journal) and show a visible "Turn interrupted" marker. Continuing should be opt-in and should tell Claude to check before repeating anything; MonoCode's automatic "Continue" is a weakness.
6. **One process per transcript** (Claude agent view; Codex `writer_lock.rs`). Needed once resume comes from the thread list.
7. **Idle reaper**, which never stops a thread with a pending approval (Paseo 1 h, Synara 30 min). Slice 5.
8. **Ask before quitting while turns are running** (MonoCode, Synara).

The code this memo judges is in `crates/core/src/protocol.rs`, `crates/core/src/transcript.rs`, `crates/core/src/session.rs` and `crates/app/src/approval.rs`. The fixtures are in `crates/core/tests/fixtures/`.
