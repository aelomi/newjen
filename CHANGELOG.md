# Changelog

What changed in each Newjen release, in plain English. Newjen shows the section for the version you run in its What's New sheet.

Every user-visible change adds a line under `## Unreleased`, in one of four groups: New features, Improvements, Fixes, Misc. A release renames `Unreleased` to the version and date.

## Unreleased

### New features

- Settings opens with ⌘, from the Newjen menu or the gear beside Usage. Its Appearance page sets light or dark, the accent color, how much glass shows through, the interface and code fonts and sizes, how wide the chat is, and blue and orange for added and removed lines. Each choice applies at once and is kept.

## 0.1.0-alpha.2 · 3 November 2026

### New features

- A Usage view shows your plan windows and their pace, the threads in each window, and a week of tokens. Open it from Usage at the foot of the sidebar.
- A context chip beside Send shows how full a thread is. Click it for the thread's tokens per model.
- A thread can work in its own git worktree. Agents commit there themselves, and Newjen keeps private checkpoints of every turn.
- The git button commits, pushes, opens a pull request or merges a worktree's branch, only when you click it. It can write the commit message with your own Claude.
- Newjen updates itself. Check for Updates is in the Newjen menu, and What's New shows the notes after each update.

### Improvements

- A new look: a frosted window with solid panels floating on it.
- The sidebar groups threads by what needs you, what is working and what is done. ⌘J jumps to the next thread that needs you.
- Approvals read as plain English and name their risk. The shield in a thread's header lists what you allowed in that thread.
- The composer, the new-thread screen, the model picker, the project chooser and the quit dialog are redesigned.
- The Changes card lists every file an agent touched, with per-file review.
- Plan reports and their windows are kept across relaunches.
- Restarting to update reopens the projects you had open, and an update that can't install says why once and keeps the version you had.

### Fixes

- Quitting while work is still saving now warns you first.
- A thread that failed leaves Needs you once you open it.
- Esc leaves the Usage view, and a thread hidden behind it stays unread.
- The thread usage popover opens above the whole composer.
- The plan cards in Usage line up at one height, and a busy thread's long title truncates.
- A paused composer shows only its pill.
- A message that wasn't sent keeps its bubble at its text width.
- A provider billed only through an API key is reported as such.

### Misc

- Builds use Rust 1.99.1, pinned in `rust-toolchain.toml`.
- Older drafts from before git worktrees keep their messages.

## 0.1.0-alpha.1 · 1 November 2026

The first pre-release: Claude and Codex threads side by side, approvals, a thread list that survives restarts, worktrees, checkpoints, a terminal panel and Dock notifications.
