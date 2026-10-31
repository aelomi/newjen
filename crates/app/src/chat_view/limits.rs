use super::*;
use gpui_kit::component::alert::Alert;
use newjen_core::agent::{LimitSnapshot, RateLimitEvent};

fn limit_detail(snapshot: &LimitSnapshot, rejected: bool, now: i64) -> String {
    let reset = newjen_core::picker::limit_reset(snapshot);
    let mut detail = if rejected {
        "Newjen won't retry or resume on its own.".into()
    } else {
        snapshot.explanation.clone()
    };
    if let Some(reset) = reset {
        detail = format!(
            "Resets {}. {detail}",
            newjen_core::picker::reset_time(reset, now)
        );
    }
    detail
}

impl ChatView {
    pub(super) fn render_limit_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = self.thread().read(cx);
        let detected = thread
            .agent_config()
            .and_then(|a| {
                self.picker
                    .statuses
                    .iter()
                    .find(|s| s.provider == a.provider)
            })
            .and_then(|s| s.limit_event());
        let event = thread.limit_event().or(detected.as_ref())?;
        let label = thread.presentation().label;
        let (snapshot, rejected) = match event {
            RateLimitEvent::Rejected(snapshot) => (snapshot, true),
            RateLimitEvent::Warning(snapshot) => (snapshot, false),
            RateLimitEvent::Updated(_) => return None,
        };
        let title = if rejected {
            format!("{label} usage limit reached")
        } else if snapshot.explanation.to_lowercase().contains("credits") {
            "Your plan limit is used up".into()
        } else {
            format!("{label} usage warning")
        };
        let detail = limit_detail(snapshot, rejected, now_ms() / 1000);
        let alert = if rejected {
            Alert::error("usage-limit", detail).title(title)
        } else {
            Alert::warning("usage-limit", detail).title(title)
        };
        Some(
            v_flex()
                .id("limit-notice")
                .debug_selector(|| "limit-notice".into())
                .gap_1()
                .child(alert)
                .child(
                    h_flex().child(
                        ui::button_with_icon(
                            "refresh-limits",
                            "Refresh",
                            ui::IconName::RefreshCw,
                            ui::Look::Ghost,
                            ui::Size::Sm,
                            cx,
                        )
                        .debug_selector(|| "refresh-limits".into())
                        .on_click(cx.listener(|v, _, w, cx| v.refresh_providers(w, cx))),
                    ),
                )
                .into_any_element(),
        )
    }
}

pub(super) fn is_limit_error(item: &Item) -> bool {
    matches!(item, Item::Error { title, .. } if title.to_lowercase().contains("usage limit"))
}

pub(super) fn notice_replaces_error(
    item: &Item,
    last: bool,
    limit: Option<&RateLimitEvent>,
) -> bool {
    last && is_limit_error(item) && matches!(limit, Some(RateLimitEvent::Rejected(_)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refused_limit_notice_promises_no_retry_and_shows_only_reported_resets() {
        let snapshot = LimitSnapshot {
            bucket: "plan".into(),
            windows: vec![],
            explanation: "CLI report".into(),
        };
        assert_eq!(
            limit_detail(&snapshot, true, 0),
            "Newjen won't retry or resume on its own."
        );
        assert_eq!(limit_detail(&snapshot, false, 0), "CLI report");
    }
    #[test]
    fn only_the_latest_limit_error_is_replaced_by_the_composer_notice() {
        let report = RateLimitEvent::Rejected(LimitSnapshot {
            bucket: "plan".into(),
            windows: vec![],
            explanation: String::new(),
        });
        let mut item = Item::Error {
            id: 1,
            title: "Usage limit reached".into(),
            detail: "raw CLI report".into(),
        };
        assert!(notice_replaces_error(&item, true, Some(&report)));
        assert!(!notice_replaces_error(&item, false, Some(&report)));
        assert!(!notice_replaces_error(&item, true, None));
        if let Item::Error { title, .. } = &mut item {
            *title = "A different failure".into();
        }
        assert!(!notice_replaces_error(&item, true, Some(&report)));
    }
}
