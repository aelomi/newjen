use crate::agent::{AgentBatch, ApprovalIntent, ApprovalKey, ApprovalReply};
use crate::protocol::{Incoming, ResultMessage};
use crate::session::{SessionEvent, Update};
use crate::thread::CONTINUE_MESSAGE;
use std::path::Path;

/// True if `line` is what claude prints on stderr (and then exits 1) when `--session-id`
/// names a conversation that already exists: `Error: Session ID <id> is already in use.`
/// Only this exact message for this id counts, so another error that happens to say "already
/// in use" (a busy port, say) is shown as it is.
pub(crate) fn is_session_in_use(line: &str, session_id: &str) -> bool {
    line.contains(&format!("Session ID {session_id} is already in use"))
}

/// True if `result` is claude's "No conversation found" error for `session_id`. A message that
/// names another session id does not count.
pub(crate) fn is_missing_conversation(result: &ResultMessage, session_id: &str) -> bool {
    const NAMED: &str = "No conversation found with session ID:";
    let names_another = |text: &String| {
        text.split_once(NAMED).is_some_and(|(_, rest)| {
            rest.split_whitespace()
                .next()
                .map(|id| id.trim_end_matches('.'))
                != Some(session_id)
        })
    };
    result.is_missing_conversation()
        && !result
            .error_lines()
            .iter()
            .chain(result.result.as_ref())
            .any(names_another)
}

/// Bookkeeping for the two repairs (see the module docs). Reset by every user message.
#[derive(Debug, Default)]
struct Recovery {
    /// The user's last message, resent once if claude must be started again to receive it.
    /// Dropped when its turn ends.
    message: Option<String>,
    /// The current process printed the "already in use" line. Held back from the transcript
    /// until it is clear whether the repair works.
    in_use_line: Option<String>,
    /// A failed write to a process that has not started its conversation yet. Held back
    /// because it is usually the same "already in use" exit seen from the other side.
    write_failure: Option<String>,
    /// The "already in use" repair has run for this message.
    resumed: bool,
    /// The "No conversation found" repair has run for this message.
    started_fresh: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Controller {
    pub initialized: bool,
    recovery: Recovery,
    translator: super::events::ClaudeEvents,
}
impl Controller {
    pub fn begin_message(&mut self, text: &str) {
        self.recovery = Recovery {
            message: Some(text.into()),
            ..Recovery::default()
        };
    }
    pub fn message(&self) -> Option<&str> {
        self.recovery.message.as_deref()
    }
    pub fn end_turn(&mut self) {
        self.recovery.message = None;
    }
    pub fn reset(&mut self) {
        self.recovery = Recovery::default();
    }
    pub fn spawned(&mut self) {
        self.initialized = false;
        self.recovery.in_use_line = None;
        self.recovery.write_failure = None;
    }
    pub fn answer(&mut self, key: ApprovalKey, intent: ApprovalIntent) -> Option<ApprovalReply> {
        self.translator.answer(key, intent)
    }
    pub fn translate(&mut self, event: &SessionEvent, cwd: Option<&Path>) -> AgentBatch {
        self.translator.translate(event, cwd)
    }
    pub fn apply(
        &mut self,
        event: &SessionEvent,
        generation: u64,
        cwd: &Path,
        id: &mut String,
        started: &mut bool,
    ) -> Update {
        match event {
            // Hold the "already in use" line back until the exit shows whether the repair
            // works; it only becomes part of an error if it doesn't.
            SessionEvent::Stderr(line) if !*started && is_session_in_use(line, &*id) => {
                self.recovery.in_use_line = Some(line.clone());
                return Update::Held;
            }
            SessionEvent::WriteFailed(detail) if !*started => {
                self.recovery.write_failure = Some(detail.clone());
                return Update::Held;
            }
            SessionEvent::Exited {
                requested: false, ..
            } if self.recovery.in_use_line.is_some() && !self.recovery.resumed => {
                self.recovery.resumed = true;
                self.recovery.in_use_line = None;
                self.recovery.write_failure = None;
                *started = true;
                return Update::Restart;
            }
            // claude checks the conversation as it starts, before it reads any message, so this
            // also comes after "Try again" (which sends nothing). Either way the conversation is
            // gone, so switch to a new one now; otherwise every later start would fail the same
            // way.
            SessionEvent::Line(Incoming::Result(result))
                if *started
                    && !self.initialized
                    && !self.recovery.started_fresh
                    && is_missing_conversation(result, &*id) =>
            {
                self.recovery.started_fresh = true;
                *id = uuid::Uuid::new_v4().to_string();
                *started = false;
                return if self
                    .recovery
                    .message
                    .as_deref()
                    .is_some_and(|message| message != CONTINUE_MESSAGE)
                {
                    Update::FreshRestart
                } else {
                    Update::FreshStopped
                };
            }
            SessionEvent::Line(Incoming::System(message)) if message.subtype == "init" => {
                *started = true;
                self.initialized = true;
                // claude normally reports the id it was given; if not, its own id is the one
                // to resume.
                if let Some(session_id) = message.session_id.as_ref().filter(|id| !id.is_empty()) {
                    *id = session_id.clone();
                }
            }
            _ => {}
        }

        // Read before the held lines go in: one of them can end the turn, and that turn end
        // must still clear the message a repair would resend.
        let mut events = Vec::new();
        // Anything held back above is real now (the repair did not apply), so it goes in
        // first, in the order it happened.
        if let Some(line) = self.recovery.in_use_line.take() {
            events.extend(
                self.translator
                    .translate_generation(generation, &SessionEvent::Stderr(line), Some(cwd))
                    .events,
            );
        }
        if let Some(detail) = self.recovery.write_failure.take() {
            events.extend(
                self.translator
                    .translate_generation(generation, &SessionEvent::WriteFailed(detail), Some(cwd))
                    .events,
            );
        }

        let batch = self
            .translator
            .translate_generation(generation, event, Some(cwd));
        events.extend(batch.events);
        Update::Apply(AgentBatch {
            events,
            checkpoint: batch.checkpoint,
        })
    }
}
