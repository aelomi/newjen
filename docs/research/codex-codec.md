# Codex codec (CLI 0.166.0)

The standalone `providers::codex::CodexCodec` translates app-server JSON lines into
`AgentBatch`, outbound JSON messages, and explicit process-control effects. It is
not connected to Thread/Session, persistence, or the picker yet. No dependencies
were added. Detection and the codec share JSON-line framing, RPC request envelopes,
and the initialize/initialized handshake. Account/model decoding is reused too.

`CodexCodec::new(AgentConfig, cwd, resume)` returns the codec and its initialize
message. `stdout`, `prompt`, `answer`, `interrupt`, `shutdown`, and
`transport_failed` drive it. `runtime`, `catalog`, `account`, and `grants` expose
read-only state. `prompt` is the only operation that emits `turn/start`.
`answer` returns `None` for consumed, foreign-generation, or resolved keys.
A file-change remember intent is downgraded to allow-once.

The caller must write `CodecOutput.messages` in order, apply its batch atomically,
and honor every effect. `Shutdown` requires terminating the process group.
`ArmStopDeadline` bounds turn completion, including time after
`StopAcknowledged`; only `TurnEnded` clears that deadline. Report expiry/I/O
failure through `transport_failed`. These process responsibilities deliberately
remain with the future Session integration. `shutdown` revokes all process grants;
there is no persisted or local automatic command approval cache.

The runtime uses Initializing, Opening, Ready, StartingTurn, Running, Stopping,
Blocked, and Closed variants. Interrupt serialization requires both server IDs.
Stopping before a confirmed turn closes the process generation; late replies
cannot start work. A failed resume never becomes a new thread automatically.

Neutral additions are configuration, token usage and quota snapshots, streamed
tool output/duration, approval updates/answer events, and process grant lifetime.
Transcript reduces these without provider checks. Tool descriptions can update
without changing row IDs, and denied outcomes survive later callbacks/results.
Quota and usage accessors currently describe live state; their persistence and UI
presentation belong to integration. Reasoning and plan text are intentionally
hidden rather than presented as replies. Unsupported rich activities get an
honest generic tool row, not guessed successful output.

## Configuration and safety

Model/effort/speed are checked against `model/list`. Default model omits `model`;
overriding effort or selecting fast requires a chosen catalog model. Effort is
sent on `turn/start`, never `thread/start`. Normal sends explicit `default` at
thread open and `serviceTierForTurn`; fast uses the advertised tier. The effective
model, speed, cwd, untrusted approval policy, user reviewer, and read-only sandbox
must match. Policy changes fail closed. No managed daemon, experimental
capabilities, credential handling, or originator override is introduced.

Command remember requires a nonempty command, absolute cwd, ordinary command kind,
a local (or omitted) environment, and no managed-network context. Its full command
and cwd appear in the exact scope label; lifetime is this process session. There
are no execpolicy or network amendments. Missing command/cwd, writeStdin, remote,
network, and unknown kinds stay once-only. File edits always stay once-only,
regardless of grantRoot. Every unknown server request gets a bounded RPC error and
a shutdown effect, including input, elicitation, permissions, token refresh, and
attestation requests. A denied-and-stopped callback sends `cancel` followed by
correlated `turn/interrupt`.

Quota updates merge by bucket, preserving unreported windows and metadata;
spendControlReached null remains unavailable rather than reporting recovery.
Rejected usage/admission or spend controls close admission and request shutdown.
Percentages alone are warnings, not authority to spend or resume. Both retrying
and final errors stop, with usage/rate/budget errors classified separately; late
completion cannot replace the terminal failure. Tokens never imply dollar cost.

## Real recordings and sandbox experiment

Recorded 2026-10-31 using the owner's installed unmodified CLI, clientInfo name
`newjen`, in a disposable colocated jj/git repository at
`/tmp/newjen-codex-scratch`. Eleven user-authorized turns total: nine read-only,
two workspace-write, plus fresh-process resume with no prompt. The advertised
lightweight model `gpt-6-luna` used low effort and normal tier. `model/list` does
not expose price, so cheapest-model ranking is unverified. No Claude turns ran.

Both policies used `approvalPolicy:"untrusted"`, `approvalsReviewer:"user"`.
The table reports tested actions only, not a promise that all actions ask.

| Tested action | read-only | workspace-write | Evidence |
|---|---|---|---|
| Create a project file with shell | Asked; approved; exit 0 | Asked; approved; exit 0 | read-only lines 89–93, 398–402; workspace-write 71–75 |
| `jj describe -m test` | Asked; approved; changed description; exit 0 | Asked; approved; “Nothing changed”; exit 0 | read-only 417–422; workspace-write 90–94 |
| `git status --short` | Asked; approved; exit 0, Apple tool/cache diagnostics | Asked; approved; exit 0 | read-only 436–458; workspace-write 109–113 |
| Write a marker under `.git/` | Asked; approved; exit 0 | Asked; approved; exit 0 | read-only 462–466; workspace-write 128–132 |
| Write a marker under `.jj/` | Asked; approved; exit 0 | Asked; approved; exit 0 | read-only 481–485; workspace-write 147–151 |
| Edit/add with apply_patch | One fileChange approval; completed | One fileChange approval; completed | read-only 643 onward; workspace-write 310 onward |

Approved commands can escape the initial read-only restriction sufficiently to
write project and metadata files. Read-only is not an immutable filesystem after
approval. The workspace-write jj command was a no-op because the previous test
had already set the description; the direct `.jj/` write independently succeeded.
These observations do not justify broadening Newjen's conservative runtime policy:
workspace-write open responses are explicitly rejected by the codec.

**Session cache scope:** read-only lines 267–269 approve the complete command
`/bin/zsh -lc 'touch remembered.txt .'` in the scratch cwd with
`acceptForSession`. The identical command in the next turn (301–359) runs without
an approval. A different command asks at line 398; the other four commands ask
too. This proves identical-command reuse and absence of a blanket command grant
in that process. Cwd/environment changes, argv equivalence, network escalations,
and cache rehydration across processes are not established. Newjen does not
implement any local cache or persist these grants.

**Quota caveat:** the recorded account read reports `ordinaryUsageAllowed:false`
and a reached ordinary quota, although the CLI accepted the explicitly authorized
recordings using its available credits. The codec intentionally blocks on this
report. A separate replay asserts rejection before thread open and preservation
of that failure after stop/exit. Turn replays use synthetic allowed admission;
the workspace-write traffic also uses synthetic read-only startup so its item
translation can be exercised. Raw fixtures retain both original reports. These
replays are not a claim that Newjen would admit that recorded account or policy.

## Fixtures and tests

All files under `crates/core/tests/fixtures/codex/` are sanitized JSONL (`dir` is
from the client's perspective: in goes to app-server, out comes from it).
Home paths are `/Users/REDACTED`; email/account/org/user identifiers and token-like
strings are redacted. Thread, turn, item, and RPC IDs remain for correlation.

- `read-only.jsonl`: initialize/account/catalog/quota/open; text; command
  allow-once; deny; cancel+interrupt; session grant and identical reuse; a different
  command; jj/git/project/metadata experiment; file approval; long-reply interrupt.
- `workspace-write.jsonl`: initialize/open; five approved commands; file approval.
- `resume.jsonl`: new process initialize and thread/resume of the recorded thread;
  no prompt and no history duplicated into Transcript.

Unit tests replay actual text/tool/approval/turn traffic through the real
Transcript, asserting rendered outcomes, status, no invented cost, and exact
outbound approval JSON bytes. Synthetic tests cover request correlation (integer
and string IDs), two callbacks per item, resolved-before-click, denied precedence,
file preview updates, incomplete approvals, unsupported requests, stale keys,
all runtime stop states, ack vs completion, retrying/final/usage errors, sparse
limits/reset seconds, signed token validation, startup/resume/policy failures,
model pagination, effective choices, and process grant expiry.
`live_codex_text_reply` is ignored and runs at most one tiny turn when explicitly
selected. It currently respects the recorded quota rejection; it was not run.
No ignored Claude tests were run.

The recording payloads were also checked structurally against the supplied
0.166.0 initialize, account, model, thread, turn, and approval request/response
schemas: 54 request/decision payloads passed. This supplemental local structural
check is not a full JSON Schema standards validator; repository tests remain
self-contained and do not depend on the temporary schema directory.

## Verification

All final gates exited 0:

- `cargo fmt --all -- --check` (no output).
- `cargo clippy --workspace --all-targets -- -D warnings` (finished successfully;
  existing Cargo future-compatibility advisory for third-party `block` 0.1.6).
- `cargo test --workspace` (320 passed, 0 failed, 5 ignored across test binaries).
  Codex-specific coverage: 26 passed, 1 ignored live test. The other ignored
  tests are existing live CLI tests; no ignored tests were invoked.

The process adapter is intentionally not spawned by Thread/Session yet. End-to-end
Newjen process watchdogs, generation filtering, persistent settings/quota, picker,
and native UI behavior still require that integration. Fast tier selection is
validated synthetically; no fast-tier model usage was recorded. Cache behavior
across cwd/environment changes, remote execution, and process resume is unverified.
