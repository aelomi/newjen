# Changelog

What changed in each Newjen release, in plain English. Newjen shows the section for the version you run in its What's New sheet.

Every user-visible change adds a line under `## Unreleased`, in one of four groups: New features, Improvements, Fixes, Misc. A release renames `Unreleased` to the version and date.

## Unreleased

### New features

- The thread list is an inbox. A Queue holds what waits on you, finished threads you have read sit below it, working threads tuck into an In flight tray, and Snoozed and Settled open as panels at the bottom.
- Snooze a thread until later today, tomorrow or next Monday, settle it when you are done, or pin it to the top. Hover a row for Snooze and Settle, or right-click it.
- Archive all in the Settled panel removes every settled thread after one confirmation.
- When the list is empty, a spirit fox runner game fills the space while you wait. Click it to play and press Space to jump. It steps aside when a thread lands.
- Settings opens with ⌘, from the Newjen menu or the gear at the foot of the sidebar. Its Appearance page sets light or dark, the accent color, how much glass shows through, the interface and code fonts and sizes, how wide the chat is, and blue and orange for added and removed lines. Each choice applies at once and is kept.

### Improvements

- A thread stays where it is while you read it and moves to its new place when you switch away.
- A thread that ended and was not opened yet waits in the Queue, and a thread held back by a usage limit shows as waiting in the In flight tray.
- The Usage row at the foot of the sidebar is now a ring. It follows the tightest limit among the providers you are using. Near a limit it names the provider and when you will hit it, and at a limit it says when it resets. Click it to open Usage.
- Claude and Codex show their own marks.
- Tooltips, menus and popovers are fully opaque, so the text behind them no longer shows through.
- Rows in the thread list move with some ease. A working thread folds into the In flight tray when you switch away, a finished one lands in the Queue, a read one drops out of it, and a settled or snoozed one slides away. The gap closes after the row fades, and the counts crossfade. The Snoozed and Settled panels rise from their row while everything behind them dims. Reduce Motion keeps the fades and drops the movement.
- Text is smaller and calmer: a 14 pt base instead of 16, quieter secondary lines in the thread list, your messages in a soft gray bubble instead of a white one, and a lighter composer border.
- The thread list, the In flight tray and the Snoozed and Settled panels follow the interface text size from Settings, rows and all.

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
