# Review and land thread work

New threads choose **Harness manages jj** or **Agent manages jj** above the conversation. **Use as project default** saves that choice for future threads. Existing threads keep their saved mode.

Landing requires a jj repository colocated with git. Git-only projects can still run agents and use Rift copies. Their landing action is unavailable until a git implementation is added.

## Harness-managed work

Newjen asks the agent not to commit. At each turn's end, it describes the working change and starts a new one. The next message waits for that snapshot. A failed snapshot shows an error and a retry button.

For a project-folder thread, Newjen first snapshots existing work and starts the thread on its own change. The review excludes the earlier work. Resolve that thread before starting another project-folder thread. A copy can keep working independently.

Open **Review work** after the turn finishes. Kit's diff viewer shows the changes since the thread's base. Choose one of these actions.

- **Rebase and land** fetches a copy's changes under the project's current change.
- **Merge and land** creates a merge with the project's current change.
- **Discard work** asks for confirmation. For a copy, it archives the thread and removes the copy. For project-folder work, it returns the working folder to the thread's base and archives the conversation. Recorded jj history remains recoverable.

Project-folder changes are already present in the project. Landing runs the checks, records the thread bookmark, and starts a fresh change. It does not copy files over themselves.

Landing rejects files changed since review, oversized diffs, unexpected project history, and conflicts found during preflight. The final jj command uses the current project state. If an external editor or jj command changes the project after preflight, jj can leave a conflict in the project with both versions preserved. Resolve it there before continuing. A failed land keeps the copy. Refresh the review after changing files or fixing a conflict.

After landing, archive the thread to close it and remove its copy and temporary jj remote. Newjen removes only that copy's trash entry. It never runs `rift gc`.

## Agent-managed work

After a Rift copy is initialized, Newjen never writes to version control in that folder. Reads use `--ignore-working-copy`. The agent owns snapshots and bookmarks.

Review shows recorded jj changes only. Files changed since the last jj snapshot are not included. Copy landing requires one bookmarked head containing the recorded work. It fetches the agent's bookmarks, then creates `thread/<id>` in the project. The copy remains until you explicitly archive it.

For an original project-folder thread, the user-triggered setup first records existing work and starts a separate change in either mode. Explicit land and discard may also write jj in that original project. Agent-managed turns still receive no automatic snapshots. Discard returns the original folder to its recorded base after confirmation. If unsnapshotted files changed since review, the operation refuses and asks you to refresh the review.

## Declare landing checks

jj does not run git hooks. Declare their equivalent checks in `.newjen/checks.json` at the repository root.

```json
[
  ["cargo", "fmt", "--all", "--", "--check"],
  ["cargo", "test", "--workspace"]
]
```

Each entry is a program followed by its arguments. Commands run sequentially in the thread's working folder before landing. To use shell syntax, explicitly declare a shell command such as `["/bin/sh", "-c", "npm test && npm run lint"]`.

Copy landing reads the declaration from the original project, not from the agent's copy. A missing file means there are no declared checks. Newjen does not discover hooks automatically. A failed command blocks landing and preserves the work. Commands must finish without interactive input.

## Review edit approvals

An edit approval shows the proposed before-and-after diff using the current file. A missing original, an ambiguous replacement, a non-text file, or a provider request without a diff is shown as unavailable. Oversized edits can be allowed only once. File edits never gain a remembered permission.

Approval rechecks the file before sending an allow decision. A changed file requires another review.

The dock has its own optional denial field. **Deny** and Escape send that note. An empty note uses the standard denial message. The conversation's half-written composer text stays separate.

## Provider integration

The landing backend and the edit-preview types (`edits::EditProposal`) are provider-neutral. Each provider connects to them through its own adapter:

- **Edit previews.** The Claude adapter builds the proposal from the Write/Edit/MultiEdit/NotebookEdit tool input. The Codex adapter builds one git-style patch from the `changes` of a file-change approval; a request without them is shown as unavailable. Both are allow-once only.
- **Denial notes.** Claude receives the note as the denial's `message`. A Codex decision carries no text, so the note stays in the composer with a notice; send it as a message if the agent should read it.
- **Landing instructions.** The thread's landing mode produces one instruction text, sent as Claude's `--append-system-prompt` or Codex's `developerInstructions`. Neither provider ever receives the other's arguments.
