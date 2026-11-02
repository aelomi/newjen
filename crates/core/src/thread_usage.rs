//! What one thread has used, folded from the adapters' usage events.
//!
//! [`ThreadUsage::apply`] is the only way it changes, so a thread rebuilt from its saved rows
//! ([`ThreadUsage::from_rows`]) and one that watched every event end up equal.

use crate::agent::{AgentEvent, ContextFill, ModelTurn, TokenCounts};
use crate::providers::{Billing, Provider};

/// One model's share of one finished turn, as the store keeps it (the `turn_usage` table).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnUsageRow {
    /// The turn's position among the thread's turns that reported usage, from 0.
    pub turn: u32,
    /// Unix milliseconds.
    pub ended_at: i64,
    pub provider: Provider,
    /// What paid for the turn when it ended.
    pub billing: Billing,
    pub model: String,
    pub tokens: TokenCounts,
    pub cost_usd: Option<f64>,
    /// Set on the main model's row: how full its context was when the turn ended.
    pub context_used: Option<u64>,
    pub context_window: Option<u64>,
}

impl TurnUsageRow {
    fn model_turn(&self) -> ModelTurn {
        ModelTurn {
            model: self.model.clone(),
            tokens: self.tokens,
            cost_usd: self.cost_usd,
            context_window: self.context_window,
        }
    }
}

/// One model's tokens and cost across the whole thread.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTotal {
    pub model: String,
    pub tokens: TokenCounts,
    /// `None` when no turn reported a cost for this model (Codex never does).
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThreadUsage {
    context: Option<ContextFill>,
    main_model: Option<String>,
    models: Vec<ModelTotal>,
    last_turn: Option<TokenCounts>,
    turns: u32,
}

impl ThreadUsage {
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::ContextUsed { model, fill } => {
                let window = fill.window.or_else(|| {
                    self.context
                        .filter(|_| self.main_model.as_deref() == Some(model))
                        .and_then(|known| known.window)
                });
                self.context = Some(ContextFill {
                    used: fill.used,
                    window,
                });
                self.main_model = Some(model.clone());
            }
            AgentEvent::TurnUsage(turns) if !turns.is_empty() => self.add_turn(turns),
            _ => {}
        }
    }

    fn add_turn(&mut self, turns: &[ModelTurn]) {
        let mut sum = TokenCounts::default();
        for turn in turns {
            sum = sum.saturating_add(turn.tokens);
            match self
                .models
                .iter_mut()
                .find(|total| total.model == turn.model)
            {
                Some(total) => {
                    total.tokens = total.tokens.saturating_add(turn.tokens);
                    total.cost_usd = add_cost(total.cost_usd, turn.cost_usd);
                }
                None => self.models.push(ModelTotal {
                    model: turn.model.clone(),
                    tokens: turn.tokens,
                    cost_usd: turn.cost_usd,
                }),
            }
        }
        self.last_turn = Some(sum);
        self.turns += 1;
    }

    /// The thread as the saved rows describe it, oldest turn first.
    pub fn from_rows(rows: &[TurnUsageRow]) -> Self {
        let mut usage = Self::default();
        let mut turn_numbers: Vec<u32> = rows.iter().map(|row| row.turn).collect();
        turn_numbers.sort_unstable();
        turn_numbers.dedup();
        for number in turn_numbers {
            let mut turn_rows: Vec<&TurnUsageRow> =
                rows.iter().filter(|row| row.turn == number).collect();
            turn_rows.sort_by(|a, b| a.model.cmp(&b.model));
            if let Some((model, used, window)) = turn_rows
                .iter()
                .find_map(|row| Some((&row.model, row.context_used?, row.context_window)))
            {
                usage.apply(&AgentEvent::ContextUsed {
                    model: model.clone(),
                    fill: ContextFill { used, window },
                });
            }
            let turns = turn_rows.iter().map(|row| row.model_turn()).collect();
            usage.apply(&AgentEvent::TurnUsage(turns));
        }
        usage
    }

    /// How full the main conversation's context was after its latest model call.
    pub fn context(&self) -> Option<ContextFill> {
        self.context
    }

    /// The model of the main conversation, as opposed to subagents' models.
    pub fn main_model(&self) -> Option<&str> {
        self.main_model.as_deref()
    }

    /// Every model the thread has used, in the order each first appeared.
    pub fn models(&self) -> &[ModelTotal] {
        &self.models
    }

    /// All models together over the last turn that reported usage.
    pub fn last_turn(&self) -> Option<TokenCounts> {
        self.last_turn
    }

    /// How many turns reported usage.
    pub fn turns(&self) -> u32 {
        self.turns
    }

    /// All models together over the whole thread.
    pub fn total(&self) -> TokenCounts {
        self.models
            .iter()
            .fold(TokenCounts::default(), |sum, model| {
                sum.saturating_add(model.tokens)
            })
    }

    pub fn cache_share(&self) -> Option<f32> {
        self.total().cache_share()
    }

    /// `None` when no model reported a cost.
    pub fn cost_usd(&self) -> Option<f64> {
        self.models
            .iter()
            .fold(None, |sum, model| add_cost(sum, model.cost_usd))
    }
}

fn add_cost(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: u64, cached: u64, written: u64, output: u64) -> TokenCounts {
        TokenCounts {
            input,
            cached_input: cached,
            cache_write_input: written,
            output,
            reasoning_output: 0,
        }
    }

    fn turn(model: &str, tokens: TokenCounts, cost: Option<f64>) -> ModelTurn {
        ModelTurn {
            model: model.into(),
            tokens,
            cost_usd: cost,
            context_window: Some(1000),
        }
    }

    fn context(model: &str, used: u64, window: Option<u64>) -> AgentEvent {
        AgentEvent::ContextUsed {
            model: model.into(),
            fill: ContextFill { used, window },
        }
    }

    #[test]
    fn context_is_replaced_never_summed() {
        let mut usage = ThreadUsage::default();
        usage.apply(&context("a", 100, None));
        usage.apply(&context("a", 150, None));
        assert_eq!(
            usage.context(),
            Some(ContextFill {
                used: 150,
                window: None
            })
        );
        assert_eq!(usage.main_model(), Some("a"));
        assert_eq!(usage.turns(), 0);
    }

    #[test]
    fn a_later_call_keeps_the_window_the_same_model_reported() {
        let mut usage = ThreadUsage::default();
        usage.apply(&context("a", 100, Some(1000)));
        usage.apply(&context("a", 200, None));
        assert_eq!(usage.context().unwrap().window, Some(1000));
        usage.apply(&context("b", 300, None));
        assert_eq!(usage.context().unwrap().window, None);
    }

    #[test]
    fn turns_add_per_model_in_first_seen_order() {
        let mut usage = ThreadUsage::default();
        usage.apply(&AgentEvent::TurnUsage(vec![
            turn("main", tokens(2, 10, 5, 4), Some(0.25)),
            turn("sub", tokens(1, 0, 0, 1), Some(0.5)),
        ]));
        usage.apply(&AgentEvent::TurnUsage(vec![turn(
            "main",
            tokens(3, 20, 0, 6),
            Some(0.25),
        )]));
        assert_eq!(usage.turns(), 2);
        let names: Vec<_> = usage.models().iter().map(|m| m.model.as_str()).collect();
        assert_eq!(names, ["main", "sub"]);
        assert_eq!(usage.models()[0].tokens, tokens(5, 30, 5, 10));
        assert_eq!(usage.models()[0].cost_usd, Some(0.5));
        assert_eq!(usage.last_turn(), Some(tokens(3, 20, 0, 6)));
        assert_eq!(usage.total(), tokens(6, 30, 5, 11));
        assert_eq!(usage.cost_usd(), Some(1.0));
    }

    #[test]
    fn cache_share_is_the_cached_part_of_the_input_side() {
        let mut usage = ThreadUsage::default();
        assert_eq!(usage.cache_share(), None);
        usage.apply(&AgentEvent::TurnUsage(vec![turn(
            "main",
            tokens(10, 60, 30, 99),
            None,
        )]));
        assert_eq!(usage.cache_share(), Some(0.6));
        assert_eq!(usage.cost_usd(), None);
    }

    #[test]
    fn an_empty_turn_and_unrelated_events_change_nothing() {
        let mut usage = ThreadUsage::default();
        usage.apply(&AgentEvent::TurnUsage(Vec::new()));
        usage.apply(&AgentEvent::TurnActive);
        assert_eq!(usage, ThreadUsage::default());
    }

    #[test]
    fn rows_rebuild_the_same_usage_the_events_built() {
        let mut live = ThreadUsage::default();
        let mut rows = Vec::new();
        for (number, used) in [100, 250].into_iter().enumerate() {
            live.apply(&context("main", used, None));
            live.apply(&context("main", used + 1, None));
            live.apply(&context("main", used + 1, Some(1000)));
            let models = vec![
                turn("main", tokens(2, 3, 4, 5), Some(0.5)),
                turn("sub", tokens(1, 1, 1, 1), None),
            ];
            live.apply(&AgentEvent::TurnUsage(models.clone()));
            let fill = live.context().unwrap();
            rows.extend(models.iter().map(|model| TurnUsageRow {
                turn: number as u32,
                ended_at: 0,
                provider: Provider::Claude,
                billing: Billing::Subscription,
                model: model.model.clone(),
                tokens: model.tokens,
                cost_usd: model.cost_usd,
                context_used: (model.model == "main").then_some(fill.used),
                context_window: model.context_window,
            }));
        }
        assert_eq!(ThreadUsage::from_rows(&rows), live);
        rows.reverse();
        assert_eq!(
            ThreadUsage::from_rows(&rows),
            live,
            "row order is irrelevant"
        );
    }
}
