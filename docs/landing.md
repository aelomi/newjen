# Review and land thread work

Review and landing need a git repository. There is one mode: **Newjen commits**. Newjen asks the agent not to commit, stage, stash or branch, and it records the agent's work itself after each turn. Until 2026-11-02 this used jj and had a second, agent-managed mode; both are gone.

A thread works either in a git worktree of its own or in the project folder. Review, Land and Discard work for both, but they record the work differently.

## How work is recorded

At each turn's end, Newjen snapshots what the agent changed. The next message waits for that snapshot. A failed snapshot shows an error and a retry button. If a background task keeps the folder busy, Stop remains available. Stop ends those tasks and lets the snapshot finish.

**Worktree thread.** The worktree has its own branch, `newjen/<id8>-<random>`. Each snapshot is a real git commit on it: `git add --all`, then `git commit --no-verify`, with signing off. If you have no git identity, the commit is made as `Newjen <newjen@localhost>`. The worktree starts at the commit the project had checked out, so review shows the branch's diff since that base.

**Project-folder thread.** Newjen never commits to your branch until you land. The thread's base and its snapshots are commits built from a separate index file (`.git/newjen/<thread id>.index`) and kept under `refs/newjen/<thread id>`. Your own index, staged changes and branch are not touched. Uncommitted work that was there before the thread started is part of the base, so review shows only what the thread changed.

## Review

The **Changes** card beside the conversation lists every file the thread touched, with its added and removed lines. While a turn runs the numbers are an estimate from the agent's edit tool calls. After the turn ends and its snapshot finishes, git's own count replaces them. The header button next to the terminal button shows or hides the card; in windows too narrow for it, the button opens the review instead.

Press **Review** in the card after the turn finishes, or click a file to open the review on it. The review lists the files on the left and shows the selected file's diff on the right (Kit's diff viewer, changes since the thread's base).

## Land, Discard, Keep files and close

- **Land** (⌘↵) runs the checks, then lands the reviewed work. There is only this one way; the old Rebase and land is gone.
  - **Worktree thread:** merges the reviewed commit into the project's current branch with `git merge --no-ff --no-verify`, with the message `Land newjen/<…>: <thread title>`. Work that is already merged lands as a no-op.
  - **Project-folder thread:** commits only the thread's files on the current branch (`git add`, then `git commit --only -- <those files>`). If any of those files also had other uncommitted changes from before the thread, Newjen refuses, because git cannot separate them. Commit them in your terminal, or choose **Keep files and close**.
- **Discard** asks for confirmation. For a worktree thread, it archives the thread and removes the worktree. For a project-folder thread, it returns the thread's files to the base (and deletes files the thread added), then archives the conversation. Landing and discarding a project-folder thread both delete its ref and index file.
- **Keep files and close** releases the thread's tracking and archives the conversation. It changes no files and no history, and it lets other threads use the project folder. Use it to keep unfinished work for manual handling, or when landing is refused. The agent must stop first; a running snapshot must finish.

Before landing, Newjen checks the following.

- **Files changed since review:** it rejects the land and asks you to refresh the review. Refresh the review after changing files.
- **Oversized diffs:** it rejects them. Review and land those with git in your terminal. A worktree diff over the display limit still allows Discard, with an explicit warning that it deletes unreviewed work after confirmation.
- **A merge in progress** in the project: it refuses until you finish or abort it.
- **Predicted conflicts:** for a worktree thread, `git merge-tree --write-tree` predicts the merge. If it would conflict, Newjen refuses. Fix the changes in the worktree and review again.
- **A failed merge:** if the merge itself fails, for example because your own uncommitted changes touch the same files, Newjen runs `git merge --abort` and reports. The project's files are unchanged. Commit or stash your changes to those files and try again.

A failed land keeps the worktree.

## Archiving

Archiving a worktree thread removes its worktree: `git worktree remove --force`, then `git worktree prune`. It then deletes the branch with `git branch -d`, which succeeds only if the branch is merged into the project's `HEAD`. An unmerged branch stays, so its work can still be recovered with git; delete it yourself with `git branch -D` when you are sure.

## Declare landing checks

Newjen's snapshot and landing commits skip git hooks (`--no-verify`). Declare their equivalent checks in `.newjen/checks.json` at the repository root.

```json
[
  ["cargo", "fmt", "--all", "--", "--check"],
  ["cargo", "test", "--workspace"]
]
```

Each entry is a program followed by its arguments. Commands run sequentially in the thread's working folder before landing. To use shell syntax, explicitly declare a shell command such as `["/bin/sh", "-c", "npm test && npm run lint"]`.

Landing reads the file as committed at the project's `HEAD`, never from the agent's files, so an agent cannot change the checks it must pass. Commit changes to it before they apply. A missing file means there are no declared checks. Newjen does not discover hooks automatically. A failed command blocks landing and preserves the work. Commands must finish without interactive input.

## Review edit approvals

An edit approval shows the proposed before-and-after diff using the current file. A missing original, an ambiguous replacement, a non-text file, or a provider request without a diff is shown as unavailable. Oversized edits can be allowed only once. File edits never gain a remembered permission.

Approval rechecks the file before sending an allow decision. A changed file requires another review.

The dock has its own optional denial field. **Deny** and Escape send that note. An empty note uses the standard denial message. The conversation's half-written composer text stays separate.

## Provider integration

The landing backend and the edit-preview types (`edits::EditProposal`) are provider-neutral. Each provider connects to them through its own adapter:

- **Edit previews.** The Claude adapter builds the proposal from the Write/Edit/MultiEdit/NotebookEdit tool input. The Codex adapter builds one git-style patch from the `changes` of a file-change approval; a request without them is shown as unavailable. Both are allow-once only.
- **Denial notes.** Claude receives the note as the denial's `message`. A Codex decision carries no text, so the note stays in the composer with a notice; send it as a message if the agent should read it.
- **Landing instructions.** There is one fixed instruction text, which tells the agent that Newjen manages version control and not to commit, stage, branch or otherwise write git metadata. It is sent as Claude's `--append-system-prompt` or Codex's `developerInstructions`. Neither provider ever receives the other's arguments.

## Older threads

A database migration (version 6) marks threads that worked in Rift copies, and project-folder threads with work recorded in jj, as resolved. Their Rift copies stay on disk and Newjen no longer removes them; remove one yourself with `rift remove <path>`. Old Rift-copy threads cannot be continued.
