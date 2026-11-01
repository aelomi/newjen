//! What a project's threads contribute to the window's attention and quit gate. The
//! adapter that shows the Dock badge and notifications, and the quit confirmation, belong to
//! `ProjectView`, which sees every loaded project; this side only reports transitions and
//! counts.

use super::*;
use crate::attention::Kind;

/// What a `ChatView` tells `ProjectView` about its threads (see `ProjectView::attend`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attention {
    /// A transition worth a notification while Newjen is in the background.
    Notify {
        thread_id: String,
        title: String,
        kind: Kind,
    },
    /// The thread's request was answered, its turn moved on, or it was archived: its
    /// notification is stale, and the badge may have changed.
    Dismiss(String),
}

impl gpui::EventEmitter<Attention> for ChatView {}

/// Why quitting now would lose work, counted across every loaded project.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct QuitBlockers {
    pub working: usize,
    pub waiting: usize,
    pub background: usize,
    pub preparing: usize,
    pub snapshots: usize,
    pub terminals: usize,
}

impl QuitBlockers {
    pub fn any(&self) -> bool {
        self.working
            + self.waiting
            + self.background
            + self.preparing
            + self.snapshots
            + self.terminals
            > 0
    }

    pub fn add(&mut self, other: &QuitBlockers) {
        self.working += other.working;
        self.waiting += other.waiting;
        self.background += other.background;
        self.preparing += other.preparing;
        self.snapshots += other.snapshots;
        self.terminals += other.terminals;
    }

    /// What quitting stops besides the threads' turns, one plain sentence each.
    pub fn notes(&self) -> Vec<String> {
        let count = |count: usize, one: &str, many: &str| match count {
            0 => None,
            1 => Some(one.to_string()),
            _ => Some(many.replace("{}", &count.to_string())),
        };
        [
            count(
                self.preparing,
                "A Rift copy is still being made.",
                "{} Rift copies are still being made.",
            ),
            count(
                self.snapshots,
                "A jj snapshot is still being saved.",
                "{} jj snapshots are still being saved.",
            ),
            count(
                self.background,
                "1 background task will stop.",
                "{} background tasks will stop.",
            ),
            count(
                self.terminals,
                "1 open terminal will close.",
                "{} open terminals will close.",
            ),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// The parts of a thread whose change is worth a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Observed {
    status: Status,
    /// The pending request's key, as text (`ApprovalKey` has no constructor outside core).
    request: Option<String>,
}

impl Observed {
    pub(super) fn of(thread: &Thread) -> Self {
        Self {
            status: thread.status(),
            request: thread
                .transcript()
                .pending_approval()
                .map(|request| request.key().to_string()),
        }
    }

    /// Only transitions notify: a new request, or a busy turn that ended. Streamed updates,
    /// a stop by the user and startup history stay quiet.
    fn notification(&self, next: &Self) -> Option<Kind> {
        if next.request.is_some() && next.request != self.request {
            return Some(Kind::NeedsApproval);
        }
        if self.status.is_busy() {
            match next.status {
                Status::Done => return Some(Kind::Done),
                Status::Failed => return Some(Kind::Failed),
                _ => {}
            }
        }
        None
    }
}

impl ChatView {
    /// This project's threads with a live approval.
    pub fn waiting_threads(&self, cx: &App) -> usize {
        self.slots
            .iter()
            .filter(|slot| {
                slot.thread
                    .read(cx)
                    .transcript()
                    .pending_approval()
                    .is_some()
            })
            .count()
    }

    /// What this project's threads hold that quitting would stop.
    pub fn quit_blockers(&self, cx: &App) -> QuitBlockers {
        let mut blockers = QuitBlockers::default();
        for slot in &self.slots {
            let thread = slot.thread.read(cx);
            blockers.working += usize::from(thread.status() == Status::Working);
            blockers.waiting += usize::from(thread.transcript().pending_approval().is_some());
            blockers.background += thread.background_tasks().len();
            blockers.preparing += usize::from(thread.preparing().is_some());
            blockers.snapshots += usize::from(thread.work_pending());
            // Even after the shell exited: a redirected background job can outlive it.
            blockers.terminals += usize::from(
                slot.terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.read(cx).has_session()),
            );
        }
        blockers
    }

    /// The title and the line of what each thread that quitting would stop mid-task is doing.
    pub fn busy_threads(&self, cx: &App) -> Vec<(String, String)> {
        let now = now_ms() / 1000;
        self.slots
            .iter()
            .filter_map(|slot| {
                let thread = slot.thread.read(cx);
                matches!(thread.status(), Status::Working | Status::Waiting).then(|| {
                    (
                        thread.title().to_string(),
                        thread
                            .activity_line(now)
                            .unwrap_or_else(|| "Working".to_string()),
                    )
                })
            })
            .collect()
    }

    /// Row `ix`'s thread changed: reports the transitions `ProjectView` turns into a badge
    /// count and notifications.
    pub(super) fn observe_attention(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Self>) {
        let (next, new_request, id, title) = {
            let slot = &self.slots[ix];
            let thread = slot.thread.read(cx);
            let next = Observed::of(thread);
            (
                next.clone(),
                next.request != slot.observed.request,
                thread.id().to_string(),
                thread.title().to_string(),
            )
        };
        let slot = &mut self.slots[ix];
        let previous = std::mem::replace(&mut slot.observed, next.clone());
        if new_request || (next.status.is_busy() && !previous.status.is_busy()) {
            cx.emit(Attention::Dismiss(id.clone()));
        }
        if let Some(kind) = previous.notification(&next) {
            cx.emit(Attention::Notify {
                thread_id: id,
                title,
                kind,
            });
        }
    }
}

#[cfg(test)]
mod attention_tests {
    use super::*;

    #[test]
    fn attention_changes_notify_once_and_stop_is_quiet() {
        let working = Observed {
            status: Status::Working,
            request: None,
        };
        let waiting = Observed {
            status: Status::Waiting,
            request: Some("1:1".to_string()),
        };
        let second = Observed {
            status: Status::Waiting,
            request: Some("1:2".to_string()),
        };
        assert_eq!(working.notification(&waiting), Some(Kind::NeedsApproval));
        assert_eq!(waiting.notification(&waiting), None);
        assert_eq!(waiting.notification(&second), Some(Kind::NeedsApproval));
        assert_eq!(
            working.notification(&Observed {
                status: Status::Done,
                request: None
            }),
            Some(Kind::Done)
        );
        assert_eq!(
            working.notification(&Observed {
                status: Status::Failed,
                request: None
            }),
            Some(Kind::Failed)
        );
        assert_eq!(
            working.notification(&Observed {
                status: Status::Stopped,
                request: None
            }),
            None
        );
    }

    #[test]
    fn quit_blockers_add_up_across_projects() {
        let mut total = QuitBlockers {
            working: 1,
            ..Default::default()
        };
        assert!(total.any());
        total.add(&QuitBlockers {
            terminals: 2,
            snapshots: 1,
            ..Default::default()
        });
        assert_eq!(
            total.notes(),
            [
                "A jj snapshot is still being saved.",
                "2 open terminals will close."
            ]
        );
        assert!(QuitBlockers::default().notes().is_empty());
        assert!(!QuitBlockers::default().any());
    }
}
