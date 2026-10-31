use super::*;
use gpui::{Animation, AnimationExt, ClickEvent};
use gpui_kit::component::{Icon, Sizable, popover::Popover, switch::Switch};
use newjen_core::providers::{self, ProviderStatus};
use newjen_core::{AgentConfig, Speed, picker};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Menu {
    Agent,
    Effort,
}
#[derive(Default)]
pub(super) struct Picker {
    pub statuses: Vec<ProviderStatus>,
    pub(super) checked: Option<Instant>,
    pub(super) refreshing: bool,
    tab: usize,
    row: usize,
    email: bool,
    open: Option<Menu>,
    pointer: bool,
    epoch: usize,
    initialized: bool,
    scroll: gpui::ScrollHandle,
    row_offset: std::cell::Cell<usize>,
}

impl ChatView {
    pub(super) fn refresh_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.refreshing {
            return;
        }
        self.picker.refreshing = true;
        let task = cx.background_spawn(async { providers::detect_all() });
        cx.spawn_in(window, async move |view, cx| {
            let statuses = task.await;
            let _ = view.update_in(cx, |view, window, cx| {
                let first_check = !view.picker.initialized;
                view.picker.initialized = true;
                view.picker.statuses = statuses;
                view.picker.checked = Some(Instant::now());
                view.picker.refreshing = false;
                let saved = view.store.last_agent_choice(view.project.id).ok().flatten();
                let restored = picker::restore_choice(saved, &view.picker.statuses);
                for slot in &view.slots {
                    slot.thread.update(cx, |thread, _| {
                        for status in &view.picker.statuses {
                            thread.refresh_limits(status);
                        }
                        if thread.is_draft()
                            && let Some(config) = restored.clone()
                            && let Some(status) = view
                                .picker
                                .statuses
                                .iter()
                                .find(|s| s.provider == config.provider)
                        {
                            let current_valid = thread.agent_config().is_some_and(|a| {
                                view.picker.statuses.iter().any(|s| a.validate(s).is_ok())
                            });
                            if first_check || !current_valid {
                                let _ = thread.set_agent_config(config, status);
                            }
                        }
                    });
                }
                view.sync_field(window, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn restore_draft_agent(&self, draft: &mut Thread) {
        if let Some(config) = picker::restore_choice(
            self.store.last_agent_choice(self.project.id).ok().flatten(),
            &self.picker.statuses,
        ) && let Some(status) = self
            .picker
            .statuses
            .iter()
            .find(|s| s.provider == config.provider)
        {
            let _ = draft.set_agent_config(config, status);
        }
    }

    fn choose_agent(&mut self, config: AgentConfig, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(status) = self
            .picker
            .statuses
            .iter()
            .find(|s| s.provider == config.provider)
            .cloned()
        {
            let result = self
                .thread()
                .update(cx, |t, _| t.set_agent_config(config, &status));
            if let Err(error) = result {
                self.slots[self.active].notice =
                    Some(Notice::held("Couldn't choose agent", error.to_string()));
            }
            self.sync_field(window, cx);
            cx.notify();
        }
    }

    fn picker_open(&mut self, menu: Menu, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.picker.open = open.then_some(menu);
        if open {
            self.picker.epoch += 1;
            self.picker.row = 0;
            if let Some(config) = self.thread().read(cx).agent_config()
                && let Some(tab) = self
                    .picker
                    .statuses
                    .iter()
                    .position(|s| s.provider == config.provider)
            {
                self.picker.tab = tab;
            }
            if self
                .picker
                .checked
                .is_none_or(|at| at.elapsed() > Duration::from_secs(300))
            {
                self.refresh_providers(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn render_agent_chips(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let config = self.thread().read(cx).agent_config().cloned();
        let status = config.as_ref().and_then(|c| {
            self.picker
                .statuses
                .iter()
                .find(|s| s.provider == c.provider)
        });
        let label = if self
            .picker
            .statuses
            .iter()
            .any(|s| !picker::model_rows(s).is_empty())
        {
            config
                .as_ref()
                .map(|c| picker::agent_label(c, status))
                .unwrap_or_else(|| "No agent".into())
        } else if self.picker.refreshing {
            "Checking agents…".into()
        } else {
            "No agent".into()
        };
        let mut row = h_flex()
            .gap_1()
            .min_w_0()
            .flex_shrink_0()
            .child(self.picker_popover(Menu::Agent, label, ui::IconName::ChevronDown, window, cx));
        if let Some(config) = config
            && status.is_some()
        {
            let mut label = picker::effort_label(&config.effort);
            if config.speed == Speed::Fast {
                label.push_str(" · Fast");
            }
            row = row.child(self.picker_popover(
                Menu::Effort,
                label,
                if config.speed == Speed::Fast {
                    ui::IconName::Zap
                } else {
                    ui::IconName::Gauge
                },
                window,
                cx,
            ));
        }
        row.into_any_element()
    }

    fn picker_popover(
        &self,
        menu: Menu,
        label: String,
        icon: ui::IconName,
        _: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let parent = cx.weak_entity();
        let change = parent.clone();
        let content = parent.clone();
        let id = match menu {
            Menu::Agent => "agent-picker",
            Menu::Effort => "effort-picker",
        };
        let title = label.clone();
        let button = ui::button(id, label, ui::Look::Ghost, ui::Size::Sm, cx)
            .when(menu == Menu::Effort, |button| {
                button.icon(Icon::new(icon).text_color(
                    if picker::is_fast(self.thread().read(cx).agent_config()) {
                        cx.theme().warning
                    } else {
                        cx.theme().muted_foreground
                    },
                ))
            })
            .max_w(px(240.))
            .overflow_hidden()
            .tooltip(title)
            .dropdown_caret(true)
            .debug_selector(move || id.to_string())
            .on_click(move |event: &ClickEvent, _, cx| {
                let _ = parent.update(cx, |v, _| {
                    v.picker.pointer = !matches!(event, ClickEvent::Keyboard(_))
                });
            });
        let pointer = cx.weak_entity();
        let keyboard = pointer.clone();
        div()
            .id((id, 0_usize))
            .capture_any_mouse_down(move |_, _, cx| {
                let _ = pointer.update(cx, |v, _| v.picker.pointer = true);
            })
            .on_key_down(move |_, _, cx| {
                let _ = keyboard.update(cx, |v, _| v.picker.pointer = false);
            })
            .child(
                Popover::new(id)
                    .track_focus(&self.picker_focus)
                    .anchor(gpui::Anchor::BottomLeft)
                    .p_1()
                    .w(px(320.))
                    .open(self.picker.open == Some(menu))
                    .trigger(button)
                    .on_open_change(move |open, window, cx| {
                        let _ = change.update(cx, |v, cx| v.picker_open(menu, *open, window, cx));
                    })
                    .content(move |_, window, cx| {
                        content
                            .update(cx, |v, cx| v.render_picker_menu(menu, window, cx))
                            .unwrap_or_else(|_| div().into_any_element())
                    }),
            )
            .into_any_element()
    }

    fn render_picker_menu(
        &self,
        menu: Menu,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let config = self.thread().read(cx).agent_config().cloned();
        let status = match menu {
            Menu::Agent => self.picker.statuses.get(self.picker.tab),
            Menu::Effort => config.as_ref().and_then(|a| {
                self.picker
                    .statuses
                    .iter()
                    .find(|s| s.provider == a.provider)
            }),
        };
        let mut panel = v_flex()
            .id("provider-menu")
            .debug_selector(|| "provider-menu".into())
            .gap_1()
            .text_sm();
        if menu == Menu::Agent {
            let mut tabs = h_flex().gap_1().p_1().rounded_md().bg(cx.theme().muted);
            for (ix, status) in self.picker.statuses.iter().enumerate() {
                let health = picker::account_view(status).health;
                let color = match health {
                    picker::Health::Ready => cx.theme().success,
                    picker::Health::Warning => cx.theme().warning,
                    picker::Health::Disabled => cx.theme().muted_foreground,
                    picker::Health::Danger => cx.theme().danger,
                };
                tabs = tabs.child(
                    ui::button(
                        ("provider-tab", ix),
                        "",
                        if ix == self.picker.tab {
                            ui::Look::Secondary
                        } else {
                            ui::Look::Ghost
                        },
                        ui::Size::Sm,
                        cx,
                    )
                    .flex_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().size(px(6.)).rounded_full().bg(color))
                            .child(status.provider.presentation().label),
                    )
                    .debug_selector(move || format!("provider-tab-{ix}"))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.picker.tab = ix;
                        v.picker.row = 0;
                        v.picker.email = false;
                        cx.notify();
                    })),
                );
            }
            panel = panel.child(tabs);
        } else {
            panel = panel.child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Effort"),
            );
        }
        let mut body = v_flex()
            .id("provider-menu-scroll")
            .track_scroll(&self.picker.scroll)
            .overflow_y_scroll()
            .min_h_0()
            .max_h(px(
                (f32::from(window.viewport_size().height) - 250.).max(110.)
            ));
        if let Some(status) = status {
            if menu == Menu::Agent {
                let account = picker::account_view(status);
                self.picker.row_offset.set(
                    usize::from(!account.plan.is_empty())
                        + account.callouts.len()
                        + usize::from(self.picker.email && account.email.is_some())
                        + usize::from(account.unavailable.is_some())
                        + usize::from(account.error.is_some()),
                );
                if !account.plan.is_empty() {
                    let version = account.version.unwrap_or_default();
                    let version = if version.chars().count() > 16 {
                        version.chars().take(16).collect::<String>() + "…"
                    } else {
                        version
                    };
                    body = body.child(
                        h_flex()
                            .px_2()
                            .py_1()
                            .gap_2()
                            .child(
                                ui::button(
                                    "provider-account",
                                    account.plan,
                                    ui::Look::Ghost,
                                    ui::Size::Sm,
                                    cx,
                                )
                                .debug_selector(|| "provider-account".into())
                                .text_color(if account.api_billing {
                                    cx.theme().warning
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .on_click(cx.listener(
                                    |v, _, _, cx| {
                                        v.picker.email = !v.picker.email;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(version),
                            ),
                    );
                }
                if self.picker.email
                    && let Some(email) = account.email
                {
                    body = body.child(
                        div()
                            .id("provider-email")
                            .debug_selector(|| "provider-email".into())
                            .px_2()
                            .text_xs()
                            .child(email),
                    );
                }
                for warning in account.callouts {
                    body = body.child(
                        div()
                            .mx_1()
                            .p_2()
                            .rounded_md()
                            .bg(cx.theme().warning.opacity(0.12))
                            .text_color(cx.theme().warning)
                            .child(warning),
                    );
                }
                if let Some(unavailable) = account.unavailable {
                    body = body.child(div().p_2().child(unavailable));
                }
                if let Some(error) = account.error {
                    body = body.child(div().p_2().font_family("monospace").text_xs().child(error));
                }
                for (ix, row) in picker::model_rows(status).into_iter().enumerate() {
                    let selected = config
                        .as_ref()
                        .is_some_and(|a| a.provider == status.provider && a.model == row.selection);
                    let mut choice = AgentConfig::cli_default(status.provider);
                    choice.model = row.selection;
                    body = body.child(self.picker_row(
                        ix,
                        (row.label, row.detail, selected, choice),
                        cx,
                    ));
                }
            } else if let Some(config) = config {
                self.picker.row_offset.set(0);
                for (ix, (effort, detail)) in
                    picker::effort_rows(&config, status).into_iter().enumerate()
                {
                    let label = picker::effort_label(&effort);
                    let selected = config.effort == effort;
                    let mut choice = config.clone();
                    choice.effort = effort;
                    body = body.child(self.picker_row(ix, (label, detail, selected, choice), cx));
                }
                if let Some(detail) = picker::fast_detail(&config, status) {
                    let fast_row = picker::effort_rows(&config, status).len();
                    let warning = matches!(&status.availability, providers::Availability::Ready { catalog, .. } if catalog.source == providers::CatalogSource::CliAliases);
                    let mut choice = config.clone();
                    choice.speed = if choice.speed == Speed::Fast {
                        Speed::Normal
                    } else {
                        Speed::Fast
                    };
                    body = body.child(
                        v_flex()
                            .when(self.picker.row == fast_row, |r| r.bg(cx.theme().muted))
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .p_2()
                            .child(
                                h_flex().justify_between().child("Fast").child(
                                    Switch::new("agent-fast")
                                        .accessibility_label("Fast")
                                        .checked(config.speed == Speed::Fast)
                                        .on_click(cx.listener(move |v, _, w, cx| {
                                            v.choose_agent(choice.clone(), w, cx)
                                        })),
                                ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if warning {
                                        cx.theme().warning
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(detail.to_owned()),
                            ),
                    );
                }
            }
        } else {
            body = body.child(div().p_2().child("Checking installed agents…"));
        }
        panel = panel.child(body);
        if menu == Menu::Agent {
            let checked = self
                .picker
                .checked
                .map(|at| {
                    if at.elapsed().as_secs() < 60 {
                        "Checked just now".into()
                    } else {
                        format!("Checked {} min ago", at.elapsed().as_secs() / 60)
                    }
                })
                .unwrap_or_else(|| "Not checked yet".into());
            panel = panel.child(
                h_flex()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .p_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(checked),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::button_with_icon(
                            "refresh-providers",
                            if self.picker.refreshing {
                                "Checking…"
                            } else {
                                "Refresh"
                            },
                            ui::IconName::RefreshCw,
                            ui::Look::Ghost,
                            ui::Size::Sm,
                            cx,
                        )
                        .debug_selector(|| "refresh-providers".into())
                        .on_click(cx.listener(|v, _, w, cx| v.refresh_providers(w, cx))),
                    ),
            );
        }
        let parent = cx.weak_entity();
        let panel = panel
            .track_focus(&self.picker_focus)
            .on_key_down(move |event, window, cx| {
                let _ = parent.update(cx, |v, cx| {
                    match event.keystroke.key.as_str() {
                        "left" | "right" if menu == Menu::Agent => {
                            let count = v.picker.statuses.len();
                            if count > 0 {
                                v.picker.tab = (v.picker.tab + 1) % count;
                                v.picker.row = 0;
                            }
                        }
                        "up" => v.picker.row = v.picker.row.saturating_sub(1),
                        "down" => {
                            let count = if menu == Menu::Agent {
                                v.picker
                                    .statuses
                                    .get(v.picker.tab)
                                    .map_or(0, |s| picker::model_rows(s).len())
                            } else {
                                v.thread()
                                    .read(cx)
                                    .agent_config()
                                    .and_then(|a| {
                                        v.picker
                                            .statuses
                                            .iter()
                                            .find(|s| s.provider == a.provider)
                                            .map(|s| {
                                                picker::effort_rows(a, s).len()
                                                    + usize::from(
                                                        picker::fast_detail(a, s).is_some(),
                                                    )
                                            })
                                    })
                                    .unwrap_or(0)
                            };
                            v.picker.row = (v.picker.row + 1).min(count.saturating_sub(1));
                        }
                        "enter" => {
                            let config = v.thread().read(cx).agent_config().cloned();
                            let choice = if menu == Menu::Agent {
                                v.picker.statuses.get(v.picker.tab).and_then(|s| {
                                    picker::model_rows(s).get(v.picker.row).cloned().map(|r| {
                                        let mut a = AgentConfig::cli_default(s.provider);
                                        a.model = r.selection;
                                        a
                                    })
                                })
                            } else {
                                config.and_then(|mut a| {
                                    let status = v
                                        .picker
                                        .statuses
                                        .iter()
                                        .find(|s| s.provider == a.provider)?;
                                    let rows = picker::effort_rows(&a, status);
                                    if let Some((effort, _)) = rows.get(v.picker.row) {
                                        a.effort = effort.clone();
                                        Some(a)
                                    } else if v.picker.row == rows.len()
                                        && picker::fast_detail(&a, status).is_some()
                                    {
                                        a.speed = if a.speed == Speed::Fast {
                                            Speed::Normal
                                        } else {
                                            Speed::Fast
                                        };
                                        Some(a)
                                    } else {
                                        None
                                    }
                                })
                            };
                            if let Some(choice) = choice {
                                v.choose_agent(choice, window, cx);
                                v.picker.open = None;
                                v.ensure_field(window, cx)
                                    .read(cx)
                                    .focus_handle(cx)
                                    .focus(window, cx);
                            }
                        }
                        _ => {
                            cx.propagate();
                            return;
                        }
                    }
                    v.picker
                        .scroll
                        .scroll_to_item(v.picker.row + v.picker.row_offset.get());
                    cx.stop_propagation();
                    cx.notify();
                });
            });
        if self.picker.pointer {
            panel
                .with_animation(
                    ("picker-enter", self.picker.epoch),
                    Animation::new(Duration::from_millis(120))
                        .with_easing(gpui_kit::component::animation::ease_out_cubic),
                    |p, delta| p.opacity(delta),
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        }
    }

    fn picker_row(
        &self,
        ix: usize,
        row: (String, Option<String>, bool, AgentConfig),
        cx: &Context<Self>,
    ) -> AnyElement {
        let (label, detail, selected, choice) = row;
        let mut row = h_flex()
            .id(("agent-row", ix))
            .debug_selector(move || format!("agent-row-{ix}"))
            .items_start()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .cursor_pointer()
            .when(ix == self.picker.row, |r| r.bg(cx.theme().muted))
            .hover(|r| r.bg(cx.theme().muted));
        row = row.child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().line_clamp(2).child(label))
                .children(detail.map(|d| {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(d)
                })),
        );
        if selected {
            row = row.child(Icon::new(ui::IconName::Check).small());
        }
        row.on_click(cx.listener(move |v, _, window, cx| {
            v.choose_agent(choice.clone(), window, cx);
            v.picker.open = None;
            v.ensure_field(window, cx)
                .read(cx)
                .focus_handle(cx)
                .focus(window, cx);
        }))
        .into_any_element()
    }
}
