use super::{CodecOutput, CodexCodec};
use crate::agent::*;
use crate::providers::detect::{RateLimitSnapshot, RateLimits};
use anyhow::Result;
use serde_json::Value;

impl CodexCodec {
    pub(super) fn rate_limits(&mut self, value: &Value, out: &mut CodecOutput) -> Result<()> {
        let report: RateLimits = serde_json::from_value(value.clone())?;
        if let Some(allowed) = report.ordinary_usage_allowed {
            self.ordinary_usage_allowed = Some(allowed);
        }
        self.merge_limit(&value["rateLimits"], out)?;
        if let Some(buckets) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
            for (id, snapshot) in buckets {
                let mut snapshot = snapshot.clone();
                snapshot["limitId"] = Value::String(id.clone());
                self.merge_limit(&snapshot, out)?;
            }
        }
        if matches!(self.state, super::CodexRuntime::Blocked) {
            out.event(AgentEvent::RateLimit(RateLimitEvent::Rejected(
                self.limit_snapshot("Usage limit reached"),
            )));
        }
        Ok(())
    }
    fn merge_limit(&mut self, update: &Value, out: &mut CodecOutput) -> Result<()> {
        let bucket = update["limitId"].as_str().unwrap_or("default").to_owned();
        let stored = self
            .limits
            .entry(bucket.clone())
            .or_insert_with(|| serde_json::json!({}));
        merge(stored, update);
        let limit: RateLimitSnapshot = serde_json::from_value(stored.clone())?;
        let mut snapshot = snapshot(&bucket, stored);
        let credits_can_pay = limit
            .credits
            .as_ref()
            .is_some_and(|credits| credits.has_credits || credits.unlimited);
        let plan_reached = self.ordinary_usage_allowed == Some(false)
            || limit.reached_type.as_ref().is_some_and(|s| !s.is_empty());
        let rejected =
            limit.spend_control_reached == Some(true) || (plan_reached && !credits_can_pay);
        let credit_backed = plan_reached && credits_can_pay;
        if credit_backed && !rejected {
            snapshot.explanation = "Plan quota reached. Turns are now paid from Codex credits on this account. Plan reset times are shown when reported; nothing resumes automatically.".into();
        }
        let warning = snapshot
            .windows
            .iter()
            .any(|w| w.used_percent.is_some_and(|p| p >= 90.0));
        let event = if rejected {
            RateLimitEvent::Rejected(snapshot)
        } else if credit_backed || warning {
            RateLimitEvent::Warning(snapshot)
        } else {
            RateLimitEvent::Updated(snapshot)
        };
        out.event(AgentEvent::RateLimit(event));
        if rejected {
            self.fail("Usage limit reached", "Codex reports a usage or spend limit. Reset information is shown when reported; nothing resumes automatically.", out);
        }
        Ok(())
    }
    pub(super) fn limit_snapshot(&self, explanation: &str) -> LimitSnapshot {
        let mut combined = LimitSnapshot {
            bucket: "account".into(),
            windows: Vec::new(),
            explanation: explanation.into(),
        };
        for (bucket, value) in &self.limits {
            combined.windows.extend(snapshot(bucket, value).windows);
        }
        combined
    }
}
fn merge(stored: &mut Value, update: &Value) {
    if let (Some(stored), Some(update)) = (stored.as_object_mut(), update.as_object()) {
        for (key, value) in update {
            if key == "spendControlReached" || !value.is_null() {
                if value.is_object() {
                    merge(
                        stored
                            .entry(key.clone())
                            .or_insert_with(|| serde_json::json!({})),
                        value,
                    );
                } else {
                    stored.insert(key.clone(), value.clone());
                }
            }
        }
    }
}
fn snapshot(bucket: &str, value: &Value) -> LimitSnapshot {
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        if let Some(window) = value.get(key).filter(|w| w.is_object()) {
            windows.push(LimitWindow {
                label: format!("{bucket} {key}"),
                used_percent: window["usedPercent"]
                    .as_f64()
                    .filter(|p| p.is_finite() && *p >= 0.0),
                resets_at_unix_seconds: window["resetsAt"].as_i64().filter(|n| *n >= 0),
            });
        }
    }
    if let Some(window) = value.get("individualLimit").filter(|w| w.is_object()) {
        windows.push(LimitWindow {
            label: format!("{bucket} spend"),
            used_percent: window["remainingPercent"].as_f64().map(|p| 100.0 - p),
            resets_at_unix_seconds: window["resetsAt"].as_i64().filter(|n| *n >= 0),
        });
    }
    LimitSnapshot {
        bucket: bucket.into(),
        windows,
        explanation: "Reset time is unavailable unless reported. Updates do not authorize a turn."
            .into(),
    }
}
