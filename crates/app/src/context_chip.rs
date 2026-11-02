//! The chip in the composer and its "This thread" popover: how full the conversation's context
//! is, what each model used, and what that is worth. Only this thread's models count here; the
//! account's limits live in the Usage view.

use gpui::{
    Anchor, AnyElement, App, ClickEvent, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, Styled, Window, div, px, relative,
};
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{ActiveTheme as _, Selectable, Sizable as _};
use newjen_core::agent::ContextFill;
use newjen_core::picker::reset_time;
use newjen_core::providers::{Billing, Provider};
use newjen_core::thread_usage::{ModelTotal, ThreadUsage};

use crate::format::{compact_tokens, dollars, model_name, thousands};
use crate::theme::material;
use crate::ui::{self, IconName};

/// From this share of the window the ring turns amber.
const NEAR_FULL: f32 = 0.8;
const WIDTH: f32 = 300.0;
const PADDING: f32 = 12.0;
const BAR_WIDTH: f32 = WIDTH - 2.0 * PADDING;
const SEGMENT_GAP: f32 = 1.0;
const MODELS_SHOWN: usize = 5;
const MODELS_KEPT_WHEN_CUT: usize = 4;
const RING: f32 = 16.0;

/// What the chip shows. It follows the thread's usage and whether the account's limit has
/// paused the thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChipState {
    /// Nothing has been reported yet.
    Hidden,
    /// Tokens, with no ring: the CLI has not said how large the window is.
    Tokens(u64),
    Ring {
        fraction: f32,
    },
    NearFull {
        fraction: f32,
    },
    /// The thread cannot send until the account's limit resets (Unix seconds).
    Paused {
        resets_at: i64,
    },
}

impl ChipState {
    pub fn of(usage: &ThreadUsage, paused_until: Option<i64>) -> Self {
        if let Some(resets_at) = paused_until {
            return Self::Paused { resets_at };
        }
        match usage.context() {
            Some(ContextFill {
                used: 0,
                window: None,
            }) => Self::Hidden,
            Some(fill) => match fill.fraction() {
                Some(fraction) if fraction >= NEAR_FULL => Self::NearFull { fraction },
                Some(fraction) => Self::Ring { fraction },
                None => Self::Tokens(fill.used),
            },
            None => match usage.total().total() {
                0 => Self::Hidden,
                total => Self::Tokens(total),
            },
        }
    }

    fn label(&self, now: i64) -> Option<String> {
        match *self {
            Self::Hidden => None,
            Self::Tokens(tokens) => Some(format!("{} tokens", compact_tokens(tokens))),
            Self::Ring { fraction } | Self::NearFull { fraction } => Some(percent(fraction)),
            Self::Paused { resets_at } => {
                Some(format!("Paused · resets {}", reset_time(resets_at, now)))
            }
        }
    }
}

fn percent(fraction: f32) -> String {
    let whole = (fraction * 100.0).round();
    let whole = if fraction > 0.0 {
        whole.max(1.0)
    } else {
        whole
    };
    format!("{whole:.0}%")
}

/// Everything the popover says, as the strings it shows. Built apart from drawing so it can
/// be tested.
#[derive(Debug, Clone, PartialEq)]
pub struct PopoverModel {
    pub provider: Provider,
    pub context: Option<ContextModel>,
    pub models: ModelsModel,
    /// The last turn's tokens.
    pub turn: Option<String>,
    /// The whole thread's tokens and turns.
    pub whole: String,
    pub cache: Option<String>,
    pub price: PriceModel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextModel {
    /// "68k of 200k", or "12.4k tokens" while the window is unknown.
    pub value: String,
    /// The bar, only when the window is known.
    pub fraction: Option<f32>,
    pub near_full: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    pub name: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelsModel {
    /// Several models, the most used first. `more` is how many were folded into one row and
    /// their tokens.
    List {
        rows: Vec<ModelRow>,
        more: Option<(usize, u64)>,
    },
    /// One model that reports its thinking apart, as Codex does.
    Reasoning { title: String, rows: Vec<ModelRow> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PriceModel {
    /// An API key pays: a real charge.
    Charged {
        amount: String,
        variable: &'static str,
    },
    /// A plan pays: what the tokens would cost at the API's list prices.
    Value {
        amount: String,
        note: Option<String>,
    },
    Unreported {
        note: String,
    },
}

/// What the popover needs from the thread and its provider.
pub struct Facts<'a> {
    pub provider: Provider,
    pub usage: &'a ThreadUsage,
    pub billing: Billing,
    /// "Max", as the CLI names the plan.
    pub plan: Option<String>,
    /// "High", when the thread chose an effort.
    pub effort: Option<String>,
}

impl PopoverModel {
    pub fn build(facts: &Facts) -> Self {
        let Facts {
            provider, usage, ..
        } = *facts;
        let total = usage.total();
        let context = usage.context().map(|fill| match fill.window {
            Some(window) if window > 0 => ContextModel {
                value: format!(
                    "{} of {}",
                    compact_tokens(fill.used),
                    compact_tokens(window)
                ),
                fraction: fill.fraction(),
                near_full: fill.fraction().is_some_and(|f| f >= NEAR_FULL),
            },
            _ => ContextModel {
                value: format!("{} tokens", compact_tokens(fill.used)),
                fraction: None,
                near_full: false,
            },
        });
        let turns = usage.turns();
        Self {
            provider,
            context,
            models: models_model(facts),
            turn: usage
                .last_turn()
                .map(|tokens| format!("{} tokens", compact_tokens(tokens.total()))),
            whole: format!(
                "{} tokens · {} {}",
                compact_tokens(total.total()),
                thousands(turns as usize),
                if turns == 1 { "turn" } else { "turns" }
            ),
            cache: usage.cache_share().map(percent),
            price: price_model(facts),
        }
    }

    /// The price shown at rest in the chip: only a real charge.
    pub fn charge(&self) -> Option<&str> {
        match &self.price {
            PriceModel::Charged { amount, .. } => Some(amount),
            _ => None,
        }
    }
}

fn models_model(facts: &Facts) -> ModelsModel {
    let models = facts.usage.models();
    if let [only] = models
        && only.tokens.reasoning_output > 0
    {
        let reasoning = only.tokens.reasoning_output.min(only.tokens.total());
        let mut title = model_name(&only.model);
        if let Some(effort) = &facts.effort {
            title = format!("{title} · {}", effort.to_lowercase());
        }
        return ModelsModel::Reasoning {
            title,
            rows: vec![
                ModelRow {
                    name: "Input and output".into(),
                    tokens: only.tokens.total() - reasoning,
                },
                ModelRow {
                    name: "Reasoning".into(),
                    tokens: reasoning,
                },
            ],
        };
    }
    let main = facts.usage.main_model();
    let mut ranked: Vec<&ModelTotal> = models.iter().collect();
    ranked.sort_by_key(|model| std::cmp::Reverse(model.tokens.total()));
    let named = models.len() > 1;
    let row = |model: &ModelTotal| ModelRow {
        name: if named && main == Some(model.model.as_str()) {
            format!("{} · main", model_name(&model.model))
        } else {
            model_name(&model.model)
        },
        tokens: model.tokens.total(),
    };
    if ranked.len() > MODELS_SHOWN {
        let (shown, rest) = ranked.split_at(MODELS_KEPT_WHEN_CUT);
        ModelsModel::List {
            rows: shown.iter().map(|model| row(model)).collect(),
            more: Some((rest.len(), rest.iter().map(|m| m.tokens.total()).sum())),
        }
    } else {
        ModelsModel::List {
            rows: ranked.into_iter().map(row).collect(),
            more: None,
        }
    }
}

fn price_model(facts: &Facts) -> PriceModel {
    let label = facts.provider.presentation().label;
    let variable = facts.provider.api_key_var();
    let plan = facts.plan.as_ref().map_or_else(
        || "subscription plan".to_string(),
        |plan| format!("{plan} plan"),
    );
    match (facts.usage.cost_usd(), facts.billing) {
        (Some(cost), Billing::ApiKey) => PriceModel::Charged {
            amount: dollars(cost),
            variable,
        },
        (Some(cost), billing) => PriceModel::Value {
            amount: match dollars(cost) {
                small if small.starts_with('<') => small,
                price => format!("≈ {price}"),
            },
            note: (billing == Billing::Subscription)
                .then(|| format!("Paid by your {plan}, not billed per token.")),
        },
        (None, billing) => PriceModel::Unreported {
            note: match (facts.provider, billing) {
                (Provider::Codex, Billing::Subscription) => {
                    format!("{label} does not report a price. Counts toward your {plan}.")
                }
                (Provider::Codex, Billing::ApiKey) => {
                    format!(
                        "{label} does not report a price. Charged to your API key ({variable})."
                    )
                }
                (Provider::Codex, Billing::Unknown) => format!("{label} does not report a price."),
                (Provider::Claude, _) => format!("{label} has not reported a price yet."),
            },
        },
    }
}

/// Whether the popover opened with the pointer (so it fades in) or from the keyboard.
pub struct ChipMenu {
    pointer: bool,
}

impl ChipMenu {
    pub fn new() -> Self {
        Self { pointer: false }
    }
}

/// The chip and its popover, for `state` (never [`ChipState::Hidden`]).
pub fn context_chip(
    menu: &Entity<ChipMenu>,
    state: ChipState,
    model: PopoverModel,
    now: i64,
    cx: &App,
) -> AnyElement {
    let pointer = menu.read(cx).pointer;
    let trigger = Trigger {
        state,
        charge: model.charge().map(str::to_owned),
        now,
        owner: menu.downgrade(),
    };
    let reset = menu.downgrade();
    Popover::new("thread-usage")
        .anchor(Anchor::BottomRight)
        .appearance(false)
        .trigger(trigger)
        .on_open_change(move |open, _, cx| {
            if !*open {
                let _ = reset.update(cx, |menu, _| menu.pointer = false);
            }
        })
        .content(move |_, _, cx| {
            ui::popover_surface("thread-usage-enter", panel(&model, cx), pointer, cx)
        })
        .into_any_element()
}

#[derive(IntoElement)]
struct Trigger {
    state: ChipState,
    charge: Option<String>,
    now: i64,
    owner: gpui::WeakEntity<ChipMenu>,
}

impl Selectable for Trigger {
    fn selected(self, _: bool) -> Self {
        self
    }

    fn is_selected(&self) -> bool {
        false
    }
}

impl RenderOnce for Trigger {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors;
        let muted = material(cx).glass_muted;
        let label = self.state.label(self.now).unwrap_or_default();
        let paused = matches!(self.state, ChipState::Paused { .. });
        let amber = matches!(self.state, ChipState::NearFull { .. });
        let ink = if paused {
            colors.danger
        } else if amber {
            colors.warning
        } else {
            muted
        };
        let owner = self.owner;
        let ring = match self.state {
            ChipState::Ring { fraction } | ChipState::NearFull { fraction } => Some(ui::ring(
                fraction,
                px(RING),
                px(2.),
                if amber {
                    colors.warning
                } else {
                    colors.foreground
                },
                colors.foreground.opacity(0.14),
            )),
            _ => None,
        };
        let content = h_flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(ink)
            .children(
                paused.then(|| ui::icon(IconName::CircleX, gpui_kit::component::Size::XSmall, ink)),
            )
            .children(ring)
            .child(label)
            .children(
                self.charge
                    .map(|charge| div().text_color(muted).child(format!("· {charge}"))),
            );
        let button = Button::new("context-chip")
            .ghost()
            .small()
            .rounded_full()
            .accessibility_label("Usage in this thread")
            .debug_selector(|| "context-chip".to_string())
            .on_click(move |event: &ClickEvent, _, cx| {
                let _ = owner.update(cx, |menu, _| {
                    menu.pointer = !matches!(event, ClickEvent::Keyboard(_));
                });
            })
            .child(content);
        if paused {
            button.bg(colors.danger.opacity(0.14)).into_any_element()
        } else {
            button.tooltip("Usage in this thread").into_any_element()
        }
    }
}

fn panel(model: &PopoverModel, cx: &App) -> gpui::Div {
    let muted = material(cx).glass_muted;
    let stroke = material(cx).card_stroke;
    let colors = cx.theme().colors;
    let divider = || div().h(px(1.)).w_full().bg(stroke);
    let pair = |label: &str, value: String| {
        h_flex()
            .justify_between()
            .gap_2()
            .child(div().text_color(muted).child(label.to_string()))
            .child(div().child(value))
    };
    let provider = model.provider;
    let mut body = v_flex()
        .debug_selector(|| "thread-usage-panel".to_string())
        .w(px(WIDTH))
        .p(px(PADDING))
        .gap_3()
        .text_sm()
        .child(
            h_flex()
                .items_center()
                .gap_2()
                .child(ui::provider_icon(provider, cx).size_4())
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("This thread"),
                )
                .child(div().text_color(muted).child(provider.presentation().label)),
        );
    if let Some(context) = &model.context {
        let color = if context.near_full {
            colors.warning
        } else {
            colors.foreground
        };
        body = body.child(
            v_flex()
                .gap_1p5()
                .child(
                    h_flex()
                        .justify_between()
                        .child(div().text_color(muted).child("Context"))
                        .child(div().text_color(color).child(context.value.clone())),
                )
                .children(context.fraction.map(|fraction| {
                    Progress::new("thread-context")
                        .value(fraction * 100.0)
                        .color(color)
                        .with_size(gpui_kit::component::Size::Size(px(4.)))
                }))
                .children(context.near_full.then(|| {
                    div().text_xs().text_color(colors.warning).child(format!(
                        "Nearly full. {} compacts the conversation on its own.",
                        provider.presentation().label
                    ))
                })),
        );
    }
    body = body.child(divider()).child(models_section(model, cx));
    body = body.child(divider()).child(
        v_flex()
            .gap_1p5()
            .children(model.turn.clone().map(|turn| pair("This turn", turn)))
            .child(pair("Whole thread", model.whole.clone()))
            .children(
                model
                    .cache
                    .clone()
                    .map(|share| pair("Read from cache", share)),
            ),
    );
    body.child(divider()).child(price_section(model, cx))
}

fn models_section(model: &PopoverModel, cx: &App) -> gpui::Div {
    let muted = material(cx).glass_muted;
    let ink = cx.theme().colors.foreground;
    let (title, rows, more): (String, &[ModelRow], Option<(usize, u64)>) = match &model.models {
        ModelsModel::List { rows, more } => ("Models".into(), rows, *more),
        ModelsModel::Reasoning { title, rows } => (title.clone(), rows, None),
    };
    let mut weights: Vec<u64> = rows.iter().map(|row| row.tokens).collect();
    weights.extend(more.map(|(_, tokens)| tokens));
    let shade = |ix: usize| ink.opacity([1.0, 0.62, 0.42, 0.28, 0.18, 0.12][ix.min(5)]);
    v_flex()
        .gap_1p5()
        .child(
            h_flex()
                .justify_between()
                .text_color(muted)
                .child(title)
                .child("tokens"),
        )
        .child(segments(&weights, &shade))
        .children(
            rows.iter()
                .enumerate()
                .map(|(ix, row)| model_line(shade(ix), row.name.clone(), row.tokens)),
        )
        .children(
            more.map(|(count, tokens)| {
                model_line(shade(rows.len()), format!("{count} more"), tokens)
            }),
        )
}

fn model_line(dot: Hsla, name: String, tokens: u64) -> gpui::Div {
    h_flex()
        .items_center()
        .gap_2()
        .child(div().flex_none().size(px(6.)).rounded_full().bg(dot))
        .child(div().flex_1().min_w_0().text_ellipsis().child(name))
        .child(div().flex_none().child(compact_tokens(tokens)))
}

/// The stacked bar: one segment per weight, as wide as its share of `BAR_WIDTH`.
fn segments(weights: &[u64], shade: &dyn Fn(usize) -> Hsla) -> gpui::Div {
    let total: u64 = weights.iter().sum();
    let room = BAR_WIDTH - SEGMENT_GAP * weights.len().saturating_sub(1) as f32;
    h_flex()
        .w(relative(1.))
        .h(px(6.))
        .gap(px(SEGMENT_GAP))
        .rounded_full()
        .overflow_hidden()
        .children(
            weights
                .iter()
                .enumerate()
                .filter(|_| total > 0)
                .map(|(ix, weight)| {
                    let width = (room * *weight as f32 / total as f32).max(2.0);
                    div().flex_none().h_full().w(px(width)).bg(shade(ix))
                }),
        )
}

fn price_section(model: &PopoverModel, cx: &App) -> gpui::Div {
    let muted = material(cx).glass_muted;
    let warning = cx.theme().colors.warning;
    match &model.price {
        PriceModel::Charged { amount, variable } => v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_color(muted).child("Charged to your API key"))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(amount.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(warning)
                    .child(format!("{variable} is set, so this thread bills the API.")),
            ),
        PriceModel::Value { amount, note } => v_flex()
            .gap_1()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_color(muted).child("Value at API list price"))
                    .child(div().child(amount.clone())),
            )
            .children(
                note.clone()
                    .map(|note| div().text_xs().text_color(muted).child(note)),
            ),
        PriceModel::Unreported { note } => div().text_xs().text_color(muted).child(note.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use newjen_core::agent::{AgentEvent, ModelTurn, TokenCounts};

    fn tokens(input: u64, cached: u64, output: u64, reasoning: u64) -> TokenCounts {
        TokenCounts {
            input,
            cached_input: cached,
            cache_write_input: 0,
            output,
            reasoning_output: reasoning,
        }
    }

    fn usage(
        context: Option<(u64, Option<u64>)>,
        turns: &[Vec<(&str, TokenCounts, Option<f64>)>],
    ) -> ThreadUsage {
        let mut usage = ThreadUsage::default();
        if let Some((used, window)) = context {
            usage.apply(&AgentEvent::ContextUsed {
                model: turns
                    .first()
                    .and_then(|turn| turn.first())
                    .map_or("main", |first| first.0)
                    .into(),
                fill: ContextFill { used, window },
            });
        }
        for turn in turns {
            usage.apply(&AgentEvent::TurnUsage(
                turn.iter()
                    .map(|(model, tokens, cost)| ModelTurn {
                        model: (*model).into(),
                        tokens: *tokens,
                        cost_usd: *cost,
                        context_window: None,
                    })
                    .collect(),
            ));
        }
        usage
    }

    fn facts<'a>(provider: Provider, usage: &'a ThreadUsage, billing: Billing) -> Facts<'a> {
        Facts {
            provider,
            usage,
            billing,
            plan: Some("Max".into()),
            effort: Some("High".into()),
        }
    }

    #[test]
    fn the_chip_follows_what_is_known() {
        let empty = ThreadUsage::default();
        assert_eq!(ChipState::of(&empty, None), ChipState::Hidden);

        let unknown_window = usage(Some((12_400, None)), &[]);
        assert_eq!(
            ChipState::of(&unknown_window, None),
            ChipState::Tokens(12_400)
        );

        let part = usage(Some((68_000, Some(200_000))), &[]);
        assert_eq!(
            ChipState::of(&part, None),
            ChipState::Ring { fraction: 0.34 }
        );

        let nearly = usage(Some((160_000, Some(200_000))), &[]);
        assert_eq!(
            ChipState::of(&nearly, None),
            ChipState::NearFull { fraction: 0.8 }
        );
        let below = usage(Some((159_000, Some(200_000))), &[]);
        assert!(matches!(
            ChipState::of(&below, None),
            ChipState::Ring { .. }
        ));

        assert_eq!(
            ChipState::of(&part, Some(5_000)),
            ChipState::Paused { resets_at: 5_000 }
        );
        assert_eq!(
            ChipState::of(&empty, Some(5_000)),
            ChipState::Paused { resets_at: 5_000 }
        );

        let turns_only = usage(None, &[vec![("main", tokens(10, 0, 5, 0), None)]]);
        assert_eq!(ChipState::of(&turns_only, None), ChipState::Tokens(15));
    }

    #[test]
    fn the_chip_text_rounds_to_a_whole_percent_and_never_reads_zero_for_some_use() {
        assert_eq!(percent(0.34), "34%");
        assert_eq!(percent(0.884), "88%");
        assert_eq!(percent(0.0004), "1%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(1.0), "100%");
        assert_eq!(
            ChipState::Tokens(12_400).label(0).as_deref(),
            Some("12.4k tokens")
        );
        assert_eq!(ChipState::Hidden.label(0), None);
    }

    #[test]
    fn a_plan_thread_shows_value_at_list_price_and_no_price_in_the_chip() {
        let usage = usage(
            Some((68_000, Some(200_000))),
            &[
                vec![
                    (
                        "claude-opus-5-5",
                        tokens(1_000, 800_000, 40_000, 0),
                        Some(3.0),
                    ),
                    (
                        "claude-haiku-5-5",
                        tokens(100, 50_000, 2_000, 0),
                        Some(1.12),
                    ),
                ],
                vec![("claude-opus-5-5", tokens(500, 360_000, 17_900, 0), None)],
            ],
        );
        let model = PopoverModel::build(&facts(Provider::Claude, &usage, Billing::Subscription));
        assert_eq!(model.context.as_ref().unwrap().value, "68k of 200k");
        assert_eq!(model.turn.as_deref(), Some("378k tokens"));
        assert_eq!(model.whole, "1.27M tokens · 2 turns");
        assert_eq!(model.cache.as_deref(), Some("100%"));
        assert_eq!(
            model.price,
            PriceModel::Value {
                amount: "≈ $4.12".into(),
                note: Some("Paid by your Max plan, not billed per token.".into())
            }
        );
        assert_eq!(model.charge(), None);
        let ModelsModel::List { rows, more } = model.models else {
            panic!("two models list")
        };
        assert_eq!(more, None);
        assert_eq!(rows[0].name, "Opus 5.5 · main");
        assert_eq!(rows[1].name, "Haiku 5.5");
    }

    #[test]
    fn an_api_key_makes_the_value_a_charge_that_shows_at_rest() {
        let usage = usage(
            None,
            &[vec![("claude-opus-5-5", tokens(1, 2, 3, 0), Some(0.4231))]],
        );
        let model = PopoverModel::build(&facts(Provider::Claude, &usage, Billing::ApiKey));
        assert_eq!(
            model.price,
            PriceModel::Charged {
                amount: "$0.42".into(),
                variable: "ANTHROPIC_API_KEY"
            }
        );
        assert_eq!(model.charge(), Some("$0.42"));
        assert!(model.context.is_none());
    }

    #[test]
    fn codex_reports_no_price_and_splits_reasoning_out() {
        let usage = usage(
            Some((121_000, Some(272_000))),
            &[vec![(
                "gpt-6.1-sol",
                tokens(100_000, 500_000, 260_000, 248_000),
                None,
            )]],
        );
        let model = PopoverModel::build(&facts(Provider::Codex, &usage, Billing::Subscription));
        assert_eq!(
            model.price,
            PriceModel::Unreported {
                note: "Codex does not report a price. Counts toward your Max plan.".into()
            }
        );
        assert_eq!(
            model.models,
            ModelsModel::Reasoning {
                title: "GPT-6.1-Sol · high".into(),
                rows: vec![
                    ModelRow {
                        name: "Input and output".into(),
                        tokens: 612_000
                    },
                    ModelRow {
                        name: "Reasoning".into(),
                        tokens: 248_000
                    },
                ]
            }
        );
        let api = PopoverModel::build(&facts(Provider::Codex, &usage, Billing::ApiKey));
        assert_eq!(
            api.price,
            PriceModel::Unreported {
                note: "Codex does not report a price. Charged to your API key (OPENAI_API_KEY)."
                    .into()
            }
        );
        assert_eq!(api.charge(), None);
    }

    #[test]
    fn many_models_fold_the_smallest_into_one_row() {
        let names = ["a", "b", "c", "d", "e", "f"];
        let turn: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(ix, name)| (*name, tokens(1_000 * (ix as u64 + 1), 0, 0, 0), None))
            .collect();
        let usage = usage(None, &[turn]);
        let model = PopoverModel::build(&facts(Provider::Claude, &usage, Billing::Subscription));
        let ModelsModel::List { rows, more } = model.models else {
            panic!("list")
        };
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].tokens, 6_000);
        assert_eq!(more, Some((2, 3_000)));
    }

    #[test]
    fn one_turn_is_singular_and_an_unreported_price_says_so() {
        let usage = usage(None, &[vec![("claude-opus-5-5", tokens(1, 0, 1, 0), None)]]);
        let model = PopoverModel::build(&facts(Provider::Claude, &usage, Billing::Subscription));
        assert_eq!(model.whole, "2 tokens · 1 turn");
        assert_eq!(
            model.price,
            PriceModel::Unreported {
                note: "Claude has not reported a price yet.".into()
            }
        );
    }
}
