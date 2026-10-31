# Shared approvals, terminal, and application lifecycle

Slice 5 keeps agent ownership in each existing thread. The inbox reads live approvals from those threads. It never copies pending requests into a second queue. Answers carry the thread ID and request ID, so changing the selected thread cannot redirect an answer.

The inbox exposes each waiting thread's oldest approval, then advances through that thread's queue. It offers Allow once and Deny. Thread-scoped grants remain in the conversation approval dock. The Dock badge counts distinct threads with live approvals, including the selected thread. Unread completed conversations do not inflate that number.

## Research and choices

T3 Code was the starting point. Its notification coordinator observes attention changes and deduplicates notifications. Newjen uses the same transition-based rule and checks application foreground state before delivery. T3's Dock badge tracks notifications and clears on focus. Newjen instead derives the badge from live approval state, as requested. T3 keeps terminal processes separate from terminal rendering. Newjen also retains a terminal when its panel is hidden or another thread is selected.

The inspected T3 snapshot puts those boundaries in `apps/web/src/components/ThreadNotificationCoordinator.logic.ts`, `ThreadNotificationCoordinator.tsx`, `apps/desktop/src/ipc/methods/notificationBadge.ts`, and `apps/server/src/terminal/Manager.ts`. The [current notification gate](https://github.com/pingdotgg/t3code/blob/main/apps/web/src/components/ThreadNotificationCoordinator.logic.ts) was also checked on 2026-10-31.

[Claude desktop](https://code.claude.com/docs/en/desktop) keeps parallel sessions and permission review visible in its app. Newjen adds one place to answer across threads. [Codex's App Server](https://openai.com/index/unlocking-the-codex-harness/) can initiate approval requests and wait for a client response. Nothing in the inbox or badge requires a Claude-specific event or model.

[Conductor](https://www.conductor.build/docs) associates a terminal and review path with each workspace. Newjen chooses the thread's working folder, including its Rift copy. [Zed terminal threads](https://zed.dev/docs/ai/terminal-threads) separate terminal organization from CLI authentication and provider settings. Newjen's terminal similarly runs a normal shell without starting an agent automatically. [Paseo](https://paseo.sh/changelog) supports provider permission prompts and notification controls. Newjen keeps permission decisions inside its app and never puts approval buttons into a desktop banner.

Zed's terminal and terminal_view crates declare GPL-3.0-or-later in their manifests. Their architecture was reference material only. Newjen's renderer is independent. Its dependencies are exactly pinned to `alacritty_terminal` 0.26.0, Apache-2.0, and `portable-pty` 0.9.0, MIT.

## macOS attention

The attention adapter uses AppKit for the Dock badge and UserNotifications for desktop banners. It detects bundle identity before asking for the notification center. GPUI's macOS notification implementation makes the same check because an unbundled call can abort.

An unbundled `cargo run` process has no bundle ID. It shows the Dock badge and requests Dock attention in the background. The inbox explains that desktop notifications require an app bundle. It does not send through System Events or impersonate another application.

A local probe put the same executable into an unsigned `.app` with a bundle ID. The notification center initialized successfully. That proves bundle initialization without a signing workflow. It does not prove macOS will grant permission or display a banner. The app requests authorization when the first background event needs a banner. Denial or delivery errors leave approvals available in the inbox and Dock. No signing or packaging system is added here.

Notification clicks reveal the corresponding thread. They never answer an approval. Startup history, repeated streamed updates, user stops, and foreground events do not notify.

## Terminal and quit

Each thread lazily owns one shell and terminal grid. Hiding the panel preserves them. Closing the terminal, archiving the thread, or quitting stops the shell. The terminal grid and PTY resize together. A draft's working folder cannot change while it owns a terminal. Empty drafts with terminals survive thread switches.

A terminal can edit files. Opening a project-folder terminal is refused while a Rift copy is being prepared. Preparing a new Rift copy is refused while any project-folder terminal remains open. Close the terminal before taking that snapshot. A thread's terminal is stopped before its Rift copy is removed.

Quit and window close share one confirmation gate. It counts running turns, live approvals, agent background tasks, Rift preparation, and owned terminal sessions. An owned PTY requires confirmation even after shell exit and output EOF, since redirected background jobs may still be running. Keep working leaves those processes alone. Quit and stop uses the existing thread shutdown and store flush.

GPUI exposes window-close interception but no public application-quit veto. A narrow native adapter adds the missing `applicationShouldTerminate:` selector only to GPUI's exact application delegate class. It never replaces the delegate or an existing selector. Native quit requests enter the same confirmation gate. Installation failures are shown in the app.

## Merge connections

Integrated onto the providers, open-project and slice 4 work, this is what is connected now. The inbox reads each thread's neutral `ApprovalView` and answers through `Thread::answer` with the thread id and `ApprovalKey`, so Codex and Claude requests take the same path and nothing translates one provider's request into the other's arguments. File edits in the inbox use slice 4's approval card and `Thread::pending_edit` preview, with the same requirement as the dock: an allow is checked against the file as it is now, and an oversized or unavailable preview allows once; the inbox's Deny sends the plain denial, and deny notes stay in the dock. `ProjectView` owns the attention adapter, the Dock badge, the notification click, the native quit guard and the quit confirmation, so one badge, one inbox and one quit gate cover every loaded project: each `ChatView` reports its threads' transitions and blockers, and the inbox lists the peers' waiting threads next to its own, with "Open thread" switching projects. Terminals keep running when a project is switched away and stop on archive, Close terminal and quit; a project-folder terminal is refused while a Rift copy of that repository is being made, and `copy_hold` refuses a copy while a project-folder terminal is open, in this project or a peer sharing the repository. The terminal's shell gets the login shell's PATH from `newjen_core::resolved_path()` and drops the same nested-session variables as sessions (`newjen_core::env::SESSION_ENV`). ⌘Q, ⌘W, the close button and `applicationShouldTerminate:` all reach `ProjectView::request_quit` through the opened window handle, which counts running turns, live approvals, background tasks, Rift preparation, jj snapshot jobs and owned terminals before asking.

## Verification record

The native adapter probe called `NSApplication.terminate`, observed cancellation into the app channel, armed quit, and read `TerminateNow` from the selector. It also set the real `NSDockTile` badge to 2, read it back, cleared it, and initialized the unbundled attention fallback. This used a synthetic delegate with GPUI's class name, so it checks AppKit integration without claiming a full Newjen window test.

Sol and Astra independently reproduced a redirected background job surviving both shell exit and PTY EOF. The quit gate now treats every owned terminal session as a blocker. Their source reviews also found archive panel state and drawer focus issues. Both were corrected, with regression tests added. Fable review was unavailable because of the provider rate limit.

The real Newjen window was driven on a scratch database with recorded fake-agent fixtures. Two simultaneous waiting threads were answered from the inbox while the second thread stayed selected. Keep working preserved both pending approvals. A real shell printed its working folder and terminal size; its history survived a thread switch and resizing to 640 by 480. Confirmed Quit and stop ended the app and every recorded direct child process. No live provider usage was spent.

Both direct execution and LaunchServices launch of the temporary unsigned bundle reached the notification center, but authorization failed with `UNErrorDomain` error 1 on this Mac. The inbox displayed the failure while retaining its pending approval. No banner or notification-click delivery is claimed verified. No signing was added.

`cargo fmt --all -- --check`, warning-denied all-target Clippy, `cargo test --workspace --locked`, and `cargo build --workspace --locked` passed. The workspace suite passed 317 tests and left three live-provider tests ignored. The existing turn-start model passed all twelve bounded properties in quick, repairs, and Rift modes. That model covers agent lifecycle, not terminal jobs or native notification delivery.

Terminal mouse-report protocols and IME composition are not implemented. Explicit close, archive, and quit stop owned terminal sessions. Crash cleanup relies on PTY hangup, so deliberately detached daemons that ignore it can outlive a crash.
