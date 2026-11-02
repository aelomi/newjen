use super::*;
use crate::agent::AgentEvent;
use crate::protocol::Incoming;
use crate::providers::claude::events::ClaudeEvents;
use crate::providers::{Account, ModelCatalog};
use crate::session::SessionEvent;
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn lines(path: &str) -> Vec<Value> {
    std::fs::read_to_string(format!("{FIXTURES}/{path}"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["msg"].clone())
        .collect()
}

fn claude_infos(path: &str) -> Vec<Value> {
    lines(path)
        .into_iter()
        .filter(|msg| msg["type"] == "rate_limit_event")
        .map(|msg| msg["rate_limit_info"].clone())
        .collect()
}

fn codex_report(path: &str, method: &str) -> RateLimits {
    let msg = lines(path)
        .into_iter()
        .find(|msg| msg["id"] == 4 && msg.get("result").is_some() && method == "read")
        .unwrap();
    serde_json::from_value(msg["result"].clone()).unwrap()
}

fn window<'a>(usage: &'a ProviderUsage, key: &str) -> &'a UsageWindow {
    usage.windows.iter().find(|w| w.key == key).unwrap()
}

fn status(
    provider: Provider,
    plan: &str,
    email: &str,
    limits: Option<RateLimits>,
) -> ProviderStatus {
    ProviderStatus {
        provider,
        availability: Availability::Ready {
            binary: "cli".into(),
            version: "1.0.0".into(),
            account: Account {
                billing: Billing::Subscription,
                plan: Some(plan.into()),
                email: Some(email.into()),
            },
            catalog: ModelCatalog {
                source: crate::providers::CatalogSource::CliAliases,
                models: Vec::new(),
            },
            rate_limits: limits.map(Box::new),
            warnings: Vec::new(),
        },
    }
}

#[test]
fn claude_recorded_event_gives_both_windows_with_utilisation_and_resets() {
    let info = &claude_infos("text_reply.jsonl")[0];
    let usage = ProviderUsage::from_claude_rate_limit(info).unwrap();
    assert_eq!(usage.provider, Provider::Claude);
    assert_eq!(usage.plan, None);
    assert_eq!(usage.credits, None);
    assert_eq!(usage.windows.len(), 2);
    let five = window(&usage, "five_hour");
    assert_eq!(five.label, "5 hours");
    assert_eq!(five.used_fraction, Some(0.13));
    assert_eq!(five.resets_at_unix_seconds, Some(1793032800));
    assert_eq!(five.status, UsageStatus::Ok);
    let week = window(&usage, "seven_day");
    assert_eq!(week.label, "Week");
    assert_eq!(week.used_fraction, Some(0.31));
    assert_eq!(week.resets_at_unix_seconds, Some(1793098800));
    assert_eq!(usage.status(), UsageStatus::Ok);
}

#[test]
fn claude_rejected_without_windows_has_a_reset_but_no_bar() {
    let info = &claude_infos("claude/limit-rejected.jsonl")[0];
    let usage = ProviderUsage::from_claude_rate_limit(info).unwrap();
    assert_eq!(usage.windows.len(), 1);
    let five = &usage.windows[0];
    assert_eq!(five.key, "five_hour");
    assert_eq!(five.used_fraction, None);
    assert_eq!(five.resets_at_unix_seconds, Some(1800000000));
    assert_eq!(five.status, UsageStatus::Limited);
    let text = five.summary(1799990000);
    assert!(text.starts_with("5 hours · resets "), "{text}");
    assert!(!text.contains('%'), "{text}");
}

#[test]
fn claude_event_without_status_is_ignored_and_high_utilisation_warns() {
    assert!(ProviderUsage::from_claude_rate_limit(&json!({})).is_none());
    let usage = ProviderUsage::from_claude_rate_limit(&json!({
        "status": "allowed",
        "rateLimitType": "five_hour",
        "unifiedWindows": {
            "five_hour": {"utilization": 0.93, "resetsAt": 10, "status": "allowed"},
            "seven_day": {"utilization": 1.0, "resetsAt": 20, "status": "allowed"}
        }
    }))
    .unwrap();
    assert_eq!(window(&usage, "five_hour").status, UsageStatus::Warning);
    assert_eq!(window(&usage, "seven_day").status, UsageStatus::Limited);
    assert_eq!(usage.status(), UsageStatus::Limited);
}

#[test]
fn every_recorded_claude_event_translates_to_a_usage_event() {
    let mut total = 0;
    for name in [
        "text_reply.jsonl",
        "multi_turn.jsonl",
        "background_task.jsonl",
    ] {
        let mut events = ClaudeEvents::default();
        for msg in lines(name) {
            let batch =
                events.translate(&SessionEvent::Line(Incoming::from_value(msg.clone())), None);
            if msg["type"] != "rate_limit_event" {
                continue;
            }
            let usage = batch
                .events
                .iter()
                .find_map(|event| match event {
                    AgentEvent::ProviderUsage(usage) => Some(usage),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{name}: no usage event"));
            assert_eq!(usage.windows.len(), 2);
            assert!(usage.windows.iter().all(|w| w.used_fraction.is_some()));
            total += 1;
        }
    }
    assert!(total >= 3);
}

#[test]
fn codex_account_read_gives_durations_percentages_and_credits() {
    let report: RateLimits = serde_json::from_slice(
        &std::fs::read(format!("{FIXTURES}/providers/codex-rate-limits.json")).unwrap(),
    )
    .map(|v: Value| serde_json::from_value(v["result"].clone()).unwrap())
    .unwrap();
    let usage = ProviderUsage::from_codex_report(&report);
    assert_eq!(usage.plan.as_deref(), Some("plus"));
    assert_eq!(usage.plan_label().as_deref(), Some("Plus"));
    assert_eq!(usage.windows.len(), 2);
    let five = window(&usage, "codex:primary");
    assert_eq!(five.label, "5 hours");
    assert_eq!(five.used_fraction, Some(0.17));
    assert_eq!(five.resets_at_unix_seconds, Some(1793462316));
    let week = window(&usage, "codex:secondary");
    assert_eq!(week.label, "Week");
    assert_eq!(week.used_fraction, Some(0.23));
    let credits = usage.credits.as_ref().unwrap();
    assert_eq!(credits.balance, Some(1000.0));
    assert_eq!(credits.summary(), "Credits 1000");
}

#[test]
fn codex_limit_reached_marks_the_window_limited_and_keeps_credits() {
    let report = codex_report("codex/read-only.jsonl", "read");
    let usage = ProviderUsage::from_codex_report(&report);
    assert_eq!(window(&usage, "codex:primary").status, UsageStatus::Limited);
    assert_eq!(window(&usage, "codex:secondary").used_fraction, Some(0.36));
    assert_eq!(usage.status(), UsageStatus::Limited);
    assert!(usage.credits.as_ref().unwrap().has_credits);
}

#[test]
fn codex_report_without_duration_or_balance_does_not_guess() {
    let report = codex_report("codex/runtime-credits.jsonl", "read");
    let usage = ProviderUsage::from_codex_report(&report);
    assert_eq!(usage.plan, None);
    let primary = window(&usage, "codex:primary");
    assert_eq!(primary.label, "Primary limit");
    assert_eq!(primary.used_fraction, Some(1.0));
    assert_eq!(primary.status, UsageStatus::Limited);
    assert_eq!(usage.windows.len(), 1);
    let credits = usage.credits.as_ref().unwrap();
    assert_eq!(credits.balance, None);
    assert_eq!(credits.summary(), "Credits available");
}

#[test]
fn codex_other_buckets_are_named_and_the_main_bucket_is_not_duplicated() {
    let snapshot = |id: &str, name: Option<&str>| -> RateLimitSnapshot {
        serde_json::from_value(json!({
            "limitId": id, "limitName": name,
            "primary": {"usedPercent": 40, "windowDurationMins": 300, "resetsAt": 100},
            "secondary": {"usedPercent": 10, "windowDurationMins": 4320, "resetsAt": 200}
        }))
        .unwrap()
    };
    let main = snapshot("codex", None);
    let other = snapshot("codex_extra", Some("Extra"));
    let report = RateLimits {
        rate_limits: main.clone(),
        by_limit_id: Some(
            [
                ("codex".to_owned(), main),
                ("codex_extra".to_owned(), other),
            ]
            .into(),
        ),
        ordinary_usage_allowed: None,
        account_id: None,
        reset_credits: None,
        upsell: None,
    };
    let usage = ProviderUsage::from_codex_report(&report);
    let labels: Vec<_> = usage.windows.iter().map(|w| w.label.as_str()).collect();
    assert_eq!(
        labels,
        ["5 hours", "3 days", "Extra · 5 hours", "Extra · 3 days"]
    );
}

#[test]
fn codex_runtime_updates_flow_through_the_codec_into_usage_events() {
    use crate::providers::codex::CodexCodec;
    let config = crate::agent::AgentConfig::cli_default(Provider::Codex);
    let (mut codec, _) = CodexCodec::new(config, "/tmp/newjen-usage-scratch".into(), None).unwrap();
    let update = lines("codex/read-only.jsonl")
        .into_iter()
        .find(|msg| msg["method"] == "account/rateLimits/updated")
        .unwrap();
    let out = codec.stdout(&update.to_string());
    let usage = out
        .batch
        .events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ProviderUsage(usage) => Some(usage),
            _ => None,
        })
        .expect("usage event");
    assert_eq!(usage.plan.as_deref(), Some("plus"));
    assert_eq!(window(usage, "codex:primary").label, "5 hours");
    assert_eq!(window(usage, "codex:secondary").used_fraction, Some(0.36));
    assert_eq!(usage.credits.as_ref().unwrap().balance, Some(1000.0));
}

#[test]
fn board_keeps_the_latest_window_and_never_erases_plan_or_credits() {
    let mut board = UsageBoard::new();
    assert!(board.get(Provider::Claude).is_none());
    let first =
        ProviderUsage::from_claude_rate_limit(&claude_infos("text_reply.jsonl")[0]).unwrap();
    board.record(&first, 100);
    board.record_status(&status(Provider::Claude, "max", "a@example.com", None), 101);
    let stored = board.get(Provider::Claude).unwrap();
    assert_eq!(stored.plan_label().as_deref(), Some("Max"));
    assert_eq!(stored.windows.len(), 2);
    assert_eq!(stored.updated_at_unix_seconds, Some(101));

    let rejected =
        ProviderUsage::from_claude_rate_limit(&claude_infos("claude/limit-rejected.jsonl")[0])
            .unwrap();
    board.record(&rejected, 200);
    let stored = board.get(Provider::Claude).unwrap();
    assert_eq!(stored.windows.len(), 2);
    assert_eq!(window(stored, "five_hour").used_fraction, None);
    assert_eq!(window(stored, "five_hour").status, UsageStatus::Limited);
    assert_eq!(window(stored, "seven_day").used_fraction, Some(0.31));
    assert_eq!(stored.plan.as_deref(), Some("max"));
    assert_eq!(stored.updated_at_unix_seconds, Some(200));
    assert!(board.get(Provider::Codex).is_none());
}

#[test]
fn status_seeds_codex_without_a_turn_and_other_accounts_start_clean() {
    let mut board = UsageBoard::new();
    let report = codex_report("codex/runtime-reply.jsonl", "read");
    board.record_status(
        &status(Provider::Codex, "plus", "a@example.com", Some(report)),
        5,
    );
    let stored = board.get(Provider::Codex).unwrap();
    assert_eq!(window(stored, "codex:primary").used_fraction, Some(0.1));
    assert_eq!(stored.plan.as_deref(), Some("plus"));

    board.record_status(&status(Provider::Codex, "pro", "b@example.com", None), 6);
    let stored = board.get(Provider::Codex).unwrap();
    assert!(stored.windows.is_empty());
    assert_eq!(stored.plan.as_deref(), Some("pro"));

    let signed_out = ProviderStatus {
        provider: Provider::Codex,
        availability: Availability::SignedOut {
            binary: "cli".into(),
            version: "1.0.0".into(),
            hint: String::new(),
        },
    };
    board.record_status(&signed_out, 7);
    assert!(board.get(Provider::Codex).is_none());
}

#[test]
fn summary_shows_a_percentage_only_when_reported_and_drops_it_after_the_reset() {
    let window = |fraction, resets| UsageWindow {
        key: "k".into(),
        label: "5 hours".into(),
        used_fraction: fraction,
        resets_at_unix_seconds: resets,
        length_seconds: None,
        status: UsageStatus::Ok,
    };
    assert!(
        window(Some(0.384), Some(2000))
            .summary(1000)
            .starts_with("5 hours · 38% · resets ")
    );
    assert_eq!(window(None, None).summary(1000), "5 hours");
    assert_eq!(
        window(Some(0.5), Some(900)).summary(1000),
        "5 hours · reset"
    );
    let credits = |has, unlimited, balance| UsageCredits {
        has_credits: has,
        unlimited,
        balance,
    };
    assert_eq!(credits(true, true, None).summary(), "Credits unlimited");
    assert_eq!(credits(true, false, Some(12.5)).summary(), "Credits 12.50");
    assert_eq!(credits(false, false, None).summary(), "No credits");
}

fn paced(used: Option<f32>, resets: i64, length: Option<i64>) -> UsageWindow {
    UsageWindow {
        key: "k".into(),
        label: "5 hours".into(),
        used_fraction: used,
        resets_at_unix_seconds: Some(resets),
        length_seconds: length,
        status: UsageStatus::Ok,
    }
}

#[test]
fn recorded_windows_know_how_long_they_last() {
    let usage =
        ProviderUsage::from_claude_rate_limit(&claude_infos("text_reply.jsonl")[0]).unwrap();
    assert_eq!(window(&usage, "five_hour").length_seconds, Some(18_000));
    assert_eq!(window(&usage, "seven_day").length_seconds, Some(604_800));
    let rejected =
        ProviderUsage::from_claude_rate_limit(&claude_infos("claude/limit-rejected.jsonl")[0])
            .unwrap();
    assert_eq!(window(&rejected, "five_hour").length_seconds, Some(18_000));

    let codex = ProviderUsage::from_codex_report(&codex_report("codex/read-only.jsonl", "read"));
    let primary = window(&codex, "codex:primary");
    assert_eq!(
        primary.length_seconds,
        Some(primary_minutes(&codex_report("codex/read-only.jsonl", "read")) * 60)
    );
    assert_eq!(
        primary.started_at(),
        Some(primary.resets_at_unix_seconds.unwrap() - primary.length_seconds.unwrap())
    );
}

fn primary_minutes(report: &RateLimits) -> i64 {
    report
        .rate_limits
        .primary
        .as_ref()
        .unwrap()
        .window_duration_mins
        .unwrap()
}

#[test]
fn an_unnamed_claude_window_and_a_spend_window_have_no_length() {
    let info = json!({"status":"allowed","unifiedWindows":{"seven_day_opus":{"utilization":0.5,"resetsAt":100}}});
    let usage = ProviderUsage::from_claude_rate_limit(&info).unwrap();
    assert_eq!(window(&usage, "seven_day_opus").length_seconds, None);
    assert_eq!(window(&usage, "seven_day_opus").started_at(), None);
}

#[test]
fn billing_comes_from_the_account_and_a_turn_report_never_erases_it() {
    let mut board = UsageBoard::new();
    let mut api = status(Provider::Claude, "max", "a@example.com", None);
    if let Availability::Ready { account, .. } = &mut api.availability {
        account.billing = Billing::ApiKey;
    }
    board.record_status(&api, 1);
    assert_eq!(board.get(Provider::Claude), None, "nothing to show yet");
    let turn = ProviderUsage::from_claude_rate_limit(&claude_infos("text_reply.jsonl")[0]).unwrap();
    assert_eq!(turn.billing, Billing::Unknown);
    board.record(&turn, 2);
    assert_eq!(
        board.get(Provider::Claude).unwrap().billing,
        Billing::ApiKey
    );
    board.record_status(&status(Provider::Claude, "max", "a@example.com", None), 3);
    assert_eq!(
        board.get(Provider::Claude).unwrap().billing,
        Billing::Subscription
    );
}

#[test]
fn elapsed_fraction_is_the_share_of_the_window_since_it_began() {
    let window = paced(Some(0.0), 18_000, Some(18_000));
    assert_eq!(window.elapsed_fraction(0), Some(0.0));
    assert_eq!(window.elapsed_fraction(4_500), Some(0.25));
    assert_eq!(window.elapsed_fraction(18_000), Some(1.0));
    assert_eq!(window.elapsed_fraction(-100), Some(0.0));
    assert_eq!(paced(None, 100, None).elapsed_fraction(50), None);
    assert_eq!(paced(None, 100, Some(0)).elapsed_fraction(50), None);
}

#[test]
fn pace_has_a_five_point_band_either_side_of_even() {
    let at = |used| paced(Some(used), 20_000, Some(10_000)).pace(15_000);
    assert_eq!(at(0.20), Some(Pace::Under));
    assert_eq!(at(0.44), Some(Pace::Under));
    assert_eq!(at(0.45), Some(Pace::On));
    assert_eq!(at(0.50), Some(Pace::On));
    assert_eq!(at(0.55), Some(Pace::On));
    assert!(matches!(at(0.56), Some(Pace::Ahead { .. })));
}

#[test]
fn running_ahead_projects_when_the_window_empties() {
    let window = |used| paced(Some(used), 20_000, Some(10_000));
    // Half the window gone (5,000 s of 10,000) and 80% used: empty after 6,250 s.
    assert_eq!(
        window(0.8).pace(15_000),
        Some(Pace::Ahead {
            runs_out_at: Some(10_000 + 6_250)
        })
    );
    // Nothing has elapsed yet, so there is no rate to project.
    assert_eq!(
        paced(Some(0.3), 20_000, Some(10_000)).pace(10_000),
        Some(Pace::Ahead { runs_out_at: None })
    );
    assert_eq!(
        window(1.0).pace(15_000),
        Some(Pace::Ahead {
            runs_out_at: Some(15_000)
        })
    );
}

#[test]
fn pace_needs_a_known_use_and_length_and_a_window_still_open() {
    assert_eq!(paced(None, 20_000, Some(10_000)).pace(15_000), None);
    assert_eq!(paced(Some(0.5), 20_000, None).pace(15_000), None);
    assert_eq!(paced(Some(0.5), 20_000, Some(10_000)).pace(20_000), None);
    assert_eq!(paced(Some(0.5), 20_000, Some(10_000)).pace(25_000), None);
}
