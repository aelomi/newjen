# Protocol fixtures

Real recordings of the Claude Code CLI's stream-json protocol, used by the `core` tests
(parsing, transcript replay, and the `newjen-fake-claude` test binary).

- **Recorded with:** claude 2.1.310 on 2026-10-26, using the CLAUDE.md spawn command plus
  `--model haiku --strict-mcp-config` (small, cheap, and no personal MCP connectors).
  Only `partial`, `partial_tool_allow` and `interrupt` also used `--include-partial-messages`,
  so only they contain `stream_event` lines. `resume` and `error_bad_resume` used `--resume`,
  `error_bad_model` used an unknown `--model`, and `error_not_logged_in` used `--bare` with
  no API key.
- **Line format:** one JSON object per line, `{"dir": "out" | "in", "msg": {...}}`.
  `out` is a line the CLI printed on stdout; `in` is a line we wrote to its stdin, in the
  order it happened. In `probe_malformed_stdin.jsonl` the `in` line is
  `{"_raw_stdin_text": "..."}`, because the input deliberately was not JSON.
- **What was sanitized:** the project path became `/tmp/project`; the home folder became
  `/Users/user` and the login name `user`; the account email and organization became
  `user@example.com`; `subscriptionType` became `"unknown"`; user-installed plugins, skills
  and agents were removed from `system/init` and the `initialize` response. Session ids,
  message ids, timestamps, pids and usage numbers are kept: they identify nobody.

**Never commit real account data or personal paths.** When you record new fixtures, sanitize
them first and grep for your user name, email and home folder before committing.
