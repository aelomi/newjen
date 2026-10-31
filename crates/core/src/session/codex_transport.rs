use super::*;
use crate::agent::{AgentBatch, AgentEvent, Checkpoint, GrantView};
use crate::providers::codex::{CodecOutput, CodexCodec, CodexRuntime, ControlEffect};

pub(super) struct CodexProcess {
    codec: CodexCodec,
    pending_prompt: Option<String>,
}
impl CodexProcess {
    pub fn new(config: &SessionConfig) -> Result<(Self, CodecOutput)> {
        let resume = match &config.conversation {
            Some(Conversation::Resume(id)) => Some(id.clone()),
            Some(Conversation::New(_)) | None => None,
        };
        let (mut codec, out) = CodexCodec::new(config.agent.clone(), config.cwd.clone(), resume)?;
        codec.set_instructions(config.instructions.clone());
        Ok((
            Self {
                codec,
                pending_prompt: None,
            },
            out,
        ))
    }
    pub fn prompt(&mut self, text: &str) -> Result<CodecOutput> {
        match self.codec.runtime() {
            CodexRuntime::Ready { .. } => self.codec.prompt(text),
            CodexRuntime::Initializing | CodexRuntime::Opening => {
                anyhow::ensure!(
                    self.pending_prompt.is_none(),
                    "A user message is already waiting for initialization"
                );
                self.pending_prompt = Some(text.into());
                Ok(CodecOutput::default())
            }
            _ => anyhow::bail!("The agent cannot accept another prompt"),
        }
    }
    pub fn stdout(&mut self, line: &str) -> CodecOutput {
        let mut out = self.codec.stdout(line);
        if matches!(self.codec.runtime(), CodexRuntime::Ready { .. })
            && let Some(text) = self.pending_prompt.take()
        {
            match self.codec.prompt(&text) {
                Ok(prompt) => {
                    out.messages.extend(prompt.messages);
                    out.effects.extend(prompt.effects);
                    out.batch.events.extend(prompt.batch.events);
                }
                Err(error) => return self.codec.transport_failed(&format!("{error:#}")),
            }
        }
        if matches!(
            self.codec.runtime(),
            CodexRuntime::Blocked | CodexRuntime::Closed
        ) {
            self.pending_prompt = None;
        }
        out
    }
    pub fn answer(
        &mut self,
        key: crate::agent::ApprovalKey,
        intent: crate::agent::ApprovalIntent,
    ) -> Option<CodecOutput> {
        self.codec.answer(key, intent)
    }
    pub fn interrupt(&mut self) -> CodecOutput {
        self.pending_prompt = None;
        self.codec.interrupt()
    }
    pub fn grants(&self) -> Vec<GrantView> {
        self.codec.grants().to_vec()
    }
}

impl Control {
    pub(super) fn codex_output(self: &Arc<Self>, out: CodecOutput) -> Result<()> {
        for effect in &out.effects {
            match effect {
                ControlEffect::Shutdown => (),
                ControlEffect::ArmStopDeadline => {
                    static NEXT: AtomicU64 = AtomicU64::new(1);
                    let id = format!("codex-stop-{}", NEXT.fetch_add(1, Ordering::Relaxed));
                    *lock(&self.pending_interrupt) = Some(id.clone());
                    let control = self.clone();
                    thread::spawn(move || {
                        thread::sleep(control.interrupt_ack_timeout);
                        control.expire_interrupt(&id);
                    });
                }
                ControlEffect::StopAcknowledged => (),
                ControlEffect::TurnEnded => {
                    lock(&self.pending_interrupt).take();
                }
            }
        }
        for message in out.messages {
            queue_line(&self.to_stdin, &message)?;
        }
        if !out.batch.events.is_empty() {
            let _ = self.events.send_blocking(SessionEvent::Batch(out.batch));
        }
        if out.effects.contains(&ControlEffect::Shutdown) {
            self.shutdown();
        }
        Ok(())
    }
}

pub(super) fn process_batch(event: &SessionEvent) -> AgentBatch {
    let events = match event {
        SessionEvent::Batch(batch) => return batch.clone(),
        SessionEvent::Stderr(line) => vec![AgentEvent::Stderr(line.clone())],
        SessionEvent::WriteFailed(detail) => vec![AgentEvent::WriteFailed(detail.clone())],
        SessionEvent::InterruptTimedOut => vec![AgentEvent::InterruptTimedOut],
        SessionEvent::Exited { code, requested } => vec![AgentEvent::Exited {
            code: *code,
            requested: *requested,
        }],
        SessionEvent::Line(_) => Vec::new(),
    };
    AgentBatch {
        events,
        checkpoint: Checkpoint::Save,
    }
}
