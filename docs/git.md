# Git in Newjen

Agents do version control themselves, as in T3 Code. Newjen sends them no instructions about git: an agent may commit, branch, push or open a pull request, and those commands go through the same approvals as any other command. Newjen adds three things around that: private checkpoints, a Changes view, and one git button. Decided 2026-11-02, after an earlier design in which only Newjen could commit ([why it changed](PLAN.md)).

A thread works in the project folder, or in a git worktree of its own on a `newjen/<id8>-<random>` branch. Everything below works for both unless it says otherwise.

## Checkpoints

When a thread starts, and after each turn, Newjen records the thread's folder as a private commit under `refs/newjen/<thread id>/<n>`. Checkpoint 0 is the starting point; checkpoint *n* is the folder after turn *n*.

- They are built in a separate index file (`.git/newjen/<thread id>.index`), so your index, staged changes, `HEAD` and branches are never touched, and `git log` never shows them.
- They cover only the thread's folder. A project in a subfolder of its repository never picks up edits elsewhere in the repository. Embedded git repositories (a nested `git clone` or `git init`) are left out.
- The next message waits for the turn's checkpoint. A failed checkpoint shows an error and a retry button.
- Archiving a thread forgets its checkpoints. Files and commits stay.

## Changes

The **Changes** card beside the conversation lists every file the thread changed since it started, with added and removed lines. While a turn runs the numbers are an estimate from the agent's edit tool calls; git's count replaces them when the turn's checkpoint is written.

The review panel shows a diff in one of three scopes:

- **All changes**: the folder now against checkpoint 0, so the agent's own commits and its uncommitted work show together, and uncommitted work that was there before the thread started does not.
- **Uncommitted**: the folder against `HEAD`.
- **Turn N**: what one turn changed, between its two checkpoints.

**Revert turn N** puts a worktree thread's files back to how they were before that turn. It never moves `HEAD`, so a commit the turn made shows as an uncommitted undo. Like T3 Code, Newjen offers file restore only in a worktree, where no other thread shares the files.

## The git button

One button picks the next sensible action from the folder's state, and its menu lists the others:

| State | Main action |
| --- | --- |
| Uncommitted changes, a remote, `gh` installed, not the default branch | Commit, push & PR |
| Uncommitted changes and a remote | Commit & push |
| Uncommitted changes, no remote | Commit |
| Committed work not pushed | Push (or Push & create PR) |
| Worktree thread, clean, not merged into the project | Merge into the project's branch |

Worktree threads also offer **Commit & merge**. ⌘↵ runs the main action. Actions are disabled while a turn runs.

- **Commit** stages the thread's folder and commits it on the current branch with your own git identity and hooks. Its message is written by your own `claude` CLI on a small model with no tools (`claude -p --model haiku`), from the uncommitted diff, only when you press the button. You can edit it before committing. Without `claude`, the thread title is used.
- **Push** pushes the current branch and sets its upstream on `origin` the first time.
- **Create PR** runs `gh pr create` for the current branch, titled with the commit's first line.
- **Merge** merges a worktree's branch into the branch the project has checked out, with `git merge --no-ff`. It refuses a detached `HEAD`, a merge in progress, and a merge `git merge-tree` predicts would conflict. A merge that still fails is aborted, leaving your files as they were. Uncommitted work in the worktree is not merged; commit it first.

A thread stays usable after any of these.

## Worktrees

- Made when the first message is sent: `git worktree add -b newjen/<id8>-<random> <path> HEAD`, beside the repository in `.newjen-worktrees/<repository>/`. It starts at the commit the project has checked out; uncommitted and gitignored files (`node_modules`, `target/`) are not in it.
- It needs a git repository with a first commit, and a project in a subfolder must be committed.
- The agent gets `NEWJEN_WORKTREE` (its branch) and `NEWJEN_SOURCE` (the repository root), so a dev server can derive its port.
- Archiving removes the worktree, then deletes the branch only if it is merged into the project's `HEAD`. An unmerged branch stays, so its work can be recovered with git.

## Edit approvals

An edit approval shows the proposed before-and-after diff using the current file. A missing original, an ambiguous replacement, a non-text file, or a provider request without a diff is shown as unavailable. Oversized edits can be allowed only once. File edits never gain a remembered permission. Approval rechecks the file before sending an allow decision; a changed file requires another review.

The dock has its own optional denial field. **Deny** and Escape send that note. Claude receives it as the denial's `message`. A Codex decision carries no text, so the note stays in the composer with a notice.

## Older threads

Database migration 6 marks threads that worked in Rift copies, and project-folder threads with work recorded in jj, as resolved; they stay read-only. Their Rift copies stay on disk; remove one yourself with `rift remove <path>`.
