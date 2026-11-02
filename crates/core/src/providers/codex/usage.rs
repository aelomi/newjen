use super::wire::{Counts, Usage};
use crate::agent::{ModelTurn, TokenCounts};

/// Codex's `total` counts the whole thread, so a turn's tokens are the growth of `total` over
/// the turn.
#[derive(Debug, Default)]
pub(super) struct TurnTokens {
    previous_end: Option<Counts>,
    baseline: Option<Counts>,
    latest: Option<Counts>,
    window: Option<u64>,
}

impl TurnTokens {
    pub(super) fn observe(&mut self, usage: &Usage) {
        if self.baseline.is_none() {
            // After a resume the first notification is the only way to learn where `total`
            // stood: it is `total` less the call that just finished.
            self.baseline = Some(
                self.previous_end
                    .filter(|end| end.total <= usage.total.total)
                    .unwrap_or_else(|| usage.total.minus(usage.last)),
            );
        }
        self.latest = Some(usage.total);
        self.window = usage.context_window.or(self.window);
    }

    pub(super) fn finish(&mut self, model: &str) -> Option<ModelTurn> {
        let latest = self.latest.take()?;
        let baseline = self.baseline.take().unwrap_or_default();
        self.previous_end = Some(latest);
        let tokens = TokenCounts::from(latest.minus(baseline));
        let window = self.window.take();
        (tokens != TokenCounts::default()).then(|| ModelTurn {
            model: model.to_owned(),
            tokens,
            cost_usd: None,
            context_window: window,
        })
    }
}

impl From<Counts> for TokenCounts {
    fn from(counts: Counts) -> Self {
        Self {
            input: counts.input.saturating_sub(counts.cached_input),
            cached_input: counts.cached_input,
            cache_write_input: counts.cache_write_input,
            output: counts.output,
            reasoning_output: counts.reasoning_output,
        }
    }
}
