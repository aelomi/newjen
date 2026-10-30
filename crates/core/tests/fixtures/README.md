# Protocol fixtures

Real recordings of the Claude Code CLI's stream-json protocol, used by the `core` tests
(parsing, transcript replay, and the `newjen-fake-claude` test binary).

- **Recorded with:** claude 2.1.310 on 2026-10-26, using the CLAUDE.md spawn command plus
  `--model haiku --strict-mcp-config` (small, cheap, and no personal MCP connectors).
  Only `partial`, `partial_tool_allow`, `interrupt` and `background_task` also used
  `--include-partial-messages`, so only they contain `stream_event` lines. `resume` and
  `error_bad_resume` used `--resume`, `error_bad_model` used an unknown `--model`, and
  `error_not_logged_in` used `--bare` with no API key.
- **`write_allow_session`** (claude 2.1.310, 2026-10-26, same flags, fresh empty project):
  the *old* "Allow for this session" answer on a Write. The CLI's only suggestion for Write is
  `{"type":"setMode","mode":"acceptEdits","destination":"session"}`; we answered allow with
  that as `updatedPermissions`, and the CLI ran the second Write without asking. Newjen no
  longer sends this: verified live, `acceptEdits` also auto-approves file-changing Bash
  commands such as `rm` and `mkdir`. Today "Allow for this session" on a Write is a plain
  allow-once, so a second Write asks again. The recording is kept as evidence and to test the
  transcript. Its SessionStart `hook_response` output was blanked (it came from
  user-installed plugins).
- **`background_task`** (claude 2.1.314, 2026-10-31, same flags, fresh empty project): a Bash
  call with `run_in_background` (`sleep 6; echo finished`). It shows
  `system/background_tasks_changed` (the full task list) and `system/task_started`, the turn
  ending while the task runs, then `task_updated`, `task_notification`,
  `background_tasks_changed` with `[]`, and a turn claude starts by itself (`init`, a reply,
  a `result` with `origin.kind: "task-notification"`) with no user message in between. Both
  results have `queued_turn_count: 0`; a count above 0 (a user message waiting for the next
  turn) was not recorded, only read from the CLI's schema text.
- **Line format:** one JSON object per line, `{"dir": "out" | "in", "msg": {...}}`.
  `out` is a line the CLI printed on stdout; `in` is a line we wrote to its stdin, in the
  order it happened. In `probe_malformed_stdin.jsonl` the `in` line is
  `{"_raw_stdin_text": "..."}`, because the input deliberately was not JSON.
- **What was sanitized:** the project path became `/tmp/project`; the home folder became
  `/Users/user` and the login name `user`; the account email and organization became
  `user@example.com`; `subscriptionType` became `"unknown"`; user-installed plugins, skills
  and agents were removed from `system/init` and the `initialize` response. In
  `background_task`, the task's output path became `/tmp/claude-501/-tmp-project/…` and every
  SessionStart `hook_response` output was blanked. Session ids, message ids, timestamps, pids
  and usage numbers are kept: they identify nobody.

**Never commit real account data or personal paths.** When you record new fixtures, sanitize
them first and grep for your user name, email and home folder before committing.
