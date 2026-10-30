# Decision memo: threads, persistence and resume (2026-10-30)

Research for slice 2: a thread list, SQLite persistence and resume. It covers how other agent development environments store threads, resume agent sessions, manage agent processes and show the thread list, with T3 Code first. Sources are T3 Code `main` at commit `6575ce3` (2026-10-30), the public source of the others, and claude 2.1.314. Claims marked UNVERIFIED were not confirmed from source. The decisions taken from this memo are in [`docs/PLAN.md`](../PLAN.md#build-order-one-vertical-slice-first).

## 1. What is stored, and where

| Product | Transcript | Index / metadata | Location |
|---|---|---|---|
| **T3 Code** | Its own full copy: an append-only event log, `orchestration_v2_events`, projected into `projection_threads`, `projection_runs`, `projection_messages`, `projection_turn_items` and `projection_runtime_requests` (approvals). Claude's JSONL is read only to fork a thread or list subagents. | Same database | `~/.t3/userdata/statev2.sqlite`, 4.4 GB on the owner's Mac |
| **Zed + Claude** | Rebuilt from Claude's JSONL (the SDK's `getSessionMessages`). Zed's own agent stores compressed JSON blobs. | `threads.db` | `~/Library/Application Support/Zed/threads/` |
| **Paseo** | Rebuilt from Claude's JSONL | One JSON file per agent: session id, title, attention reason | `~/.paseo` |
| **Codex** | Its own JSONL "rollout" files | `state_5.sqlite`: `threads(id, rollout_path, title, cwd, archived)` and read receipts | `~/.codex` |
| **Vibe Kanban** | Raw JSONL logs stored in SQLite | `coding_agent_turns(agent_session_id, seen)` | – |
| **MonoCode** (Rust + rusqlite) | Rendered blocks per session (`blocks_json`) | `sessions(provider_session_id, title, …)` | `~/Library/Application Support/<bundle-id>/monocode.db` |

- **Claude does not replay history on `--resume`.** The output is hooks, then `init`, then the new turn (see the `resume.jsonl` fixture). To show earlier messages, an app needs its own copy or has to parse Claude's private JSONL format.
- **SQLite settings.**
  - T3: `journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`, `foreign_keys=ON`, numbered migrations.
  - MonoCode: WAL, a separate read-only connection and a `schema_migrations` table.

## 2. Resume

- **T3 creates the Claude session id itself.**
  - It allocates a UUID when the thread is created and passes it as `--session-id` on the first spawn, then `--resume` on every later spawn.
  - A session that already has turns must be resumed, not reopened with `--session-id`, "to avoid 'already in use'".
  - `resumeSessionAt` is used only to roll a thread back. Vibe Kanban instead starts a new process for every message (`--resume … --resume-session-at`).
- **After a restart, T3:**
  - Marks pending approvals `expired` ("The server restarted before this runtime request was resolved") and running turns `cancelled`.
  - By default sends "Continue where you left off." on its own, at most 3 times in 5 minutes, and never to a thread waiting on an approval.
  - Also offers a manual Resume button.

## 3. Process lifetime

- **Idle timeout.** T3 keeps a thread's `claude` running after you switch away, and stops it after **30 minutes idle**. It never stops one mid-turn, and waits up to 4 hours while background work is still reporting. The next message resumes it. Codex unloads idle threads after **1800 s** too.
- **No limit** on live processes was found in T3.
- **Quitting.** T3's server and Zeron's engine can outlive the app window, so quitting often doesn't stop agents. Newjen is a single process, so quitting stops everything.

## 4. Thread list

- **T3 sidebar layout.**
  - A flat list with sections: Pinned, Active, Working, Snoozed, Settled. Grouping by project is an opt-in Labs setting.
  - New threads go on top. Activity does not reorder the list.
- **Status, highest priority first:** pending approval (amber), awaiting input, failed (unseen), working (pulsing), connecting, completed (unseen, which works as "unread").
- **Titles.**
  - The first message, cut to 50 characters, with "New thread" as the fallback.
  - Then a model writes a better title. T3 runs a separate hidden `claude -p --model claude-haiku-…` call on the user's own login, or a Codex model.
  - The CLI also has `generate_session_title` and `rename_session` control requests.
- **Thread menu:** rename, mark unread, pin, snooze, archive, and delete (soft, with 5 s undo).
- **Shortcuts:**
  - ⌘N new thread.
  - ⌘⇧[ and ⌘⇧] previous and next thread.
  - ⌘1–9 jump to a thread.
  - ⌘K search.

## 5. "Always allow" grants across resume

- **No product we checked saves session grants or re-applies them after resume.** They live in the CLI process's memory, so a resumed session forgets them.
  - T3 forwards every suggested rule to `session` and falls back to the whole tool.
  - Paseo passes grants straight through.
- **Re-applying grants (verified on this Mac).** Rules passed at spawn with `--allowedTools "Bash(touch one.txt)" mcp__server__tool` show up in the CLI's `list_permission_rules` control response with `source: "cliArg"` and `editability: "session"`. No model turn is needed to check this.
- **Removing grants.** No control request removes a rule (`update_settings` writes settings files, so don't use it). Revoking means restarting the session with `--resume` and the remaining rules.
- **Control requests a host can send** (claude 2.1.314 binary): `initialize`, `interrupt`, `end_session`, `set_permission_mode`, `set_model`, `get_settings`, `list_permission_rules`, `get_context_usage`, `get_session_cost`, `mcp_status`, `rewind_conversation`, `fork_conversation`, `rename_session`, `generate_session_title`, `stop_task`, `get_workspace_diff`, and more.

## 6. What Newjen takes, and where it goes further

- **Its own small transcript copy.** Rendered transcript items per thread in SQLite, like MonoCode, rather than T3's event log, which has grown to gigabytes. Claude's JSONL remains the full record.
- **Session id at creation.** Allocated when the thread is created: `--session-id` until `init` arrives, `--resume` after. Recovers in both directions:
  - "already in use" → resume.
  - "No conversation found" → new id, keeping the shown history.
- **Honest restarts.** On launch, approvals left pending are marked expired and running work interrupted. Continuing is **manual only**. T3's automatic continue would start a turn the user didn't ask for, which the subscription-safety rules forbid.
- **Idle shutdown after 30 minutes,** never mid-turn or with an approval pending. Quitting stops every agent.
- **Grants that survive resume, which no other product does.**
  - "Always allow" rules are saved per thread and re-applied at every spawn with `--allowedTools`. The dock says "for this thread".
  - Revoking restarts the session at the next turn boundary.
- **No hidden model calls for titles.** The title is the first message; rename comes later.
