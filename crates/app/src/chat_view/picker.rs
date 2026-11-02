use super::*;
use gpui::{Animation, AnimationExt, ClickEvent};
use gpui_kit::component::{Disableable as _, Icon, Sizable, popover::Popover, switch::Switch};
use newjen_core::providers::{self, Billing, ModelSelection, Provider, ProviderStatus, Warning};
use newjen_core::{AgentConfig, EffortSelection, Speed, picker};

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
    row: usize,
    open: Option<Menu>,
    pointer: bool,
    epoch: usize,
    initialized: bool,
    scroll: gpui::ScrollHandle,
    child_of_row: std::cell::RefCell<Vec<usize>>,
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
            self.picker.row = self
                .thread()
                .read(cx)
                .agent_config()
                .and_then(|config| match menu {
                    Menu::Agent => self.model_rows().iter().position(|(provider, row)| {
                        *provider == config.provider && row.selection == config.model
                    }),
                    Menu::Effort => self
                        .picker
                        .statuses
                        .iter()
                        .find(|s| s.provider == config.provider)
                        .and_then(|s| {
                            picker::effort_rows(config, s)
                                .iter()
                                .position(|(effort, _)| *effort == config.effort)
                        }),
                })
                .unwrap_or(0);
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

    /// Every provider's models in menu order, for the keys.
    fn model_rows(&self) -> Vec<(Provider, picker::ModelRow)> {
        self.picker
            .statuses
            .iter()
            .flat_map(|status| {
                picker::model_rows(status)
                    .into_iter()
                    .map(|row| (status.provider, row))
            })
            .collect()
    }

    pub(super) fn render_agent_chips(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let config = self.thread().read(cx).agent_config().cloned();
        let status = config.as_ref().and_then(|c| {
            self.picker
                .statuses
                .iter()
                .find(|s| s.provider == c.provider)
        });
        let any_model = self
            .picker
            .statuses
            .iter()
            .any(|s| !picker::model_rows(s).is_empty());
        let label = if any_model {
            config
                .as_ref()
                .map(|c| model_label(c, status))
                .unwrap_or_else(|| "No agent".into())
        } else if self.picker.refreshing {
            "Checking agents…".into()
        } else {
            "No agent".into()
        };
        let agent_icon = config
            .as_ref()
            .filter(|_| any_model)
            .map(|c| provider_icon(c.provider, cx));
        let mut row = h_flex()
            .gap_1()
            .min_w_0()
            .flex_shrink_0()
            .child(self.picker_popover(Menu::Agent, label, agent_icon, window, cx));
        if let Some(config) = config
            && status.is_some()
        {
            let fast = config.speed == Speed::Fast;
            let mut label = picker::effort_label(&config.effort);
            if fast {
                label.push_str(" · Fast");
            }
            let icon = Icon::new(if fast {
                ui::IconName::Zap
            } else {
                ui::IconName::Gauge
            })
            .text_color(if fast {
                cx.theme().warning
            } else {
                cx.theme().muted_foreground
            });
            row = row.child(self.picker_popover(Menu::Effort, label, Some(icon), window, cx));
        }
        row.into_any_element()
    }

    /// What a started thread shows where the chips were: its agent and effort, fixed.
    pub(super) fn render_frozen_chips(
        &self,
        config: Option<&AgentConfig>,
        status: Option<&ProviderStatus>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let Some(config) = config else {
            return div()
                .text_xs()
                .text_color(muted)
                .child("Unavailable provider")
                .into_any_element();
        };
        let part = |icon: Icon, text: String| {
            h_flex()
                .gap_1p5()
                .min_w_0()
                .child(icon)
                .child(div().truncate().text_xs().text_color(muted).child(text))
        };
        let fast = config.speed == Speed::Fast;
        h_flex()
            .gap_3()
            .min_w_0()
            .px_2()
            .child(part(
                provider_icon(config.provider, cx),
                model_label(config, status),
            ))
            .when(
                config.effort != EffortSelection::CliDefault || fast,
                |row| {
                    row.child(part(
                        Icon::new(if fast {
                            ui::IconName::Zap
                        } else {
                            ui::IconName::Gauge
                        })
                        .text_color(if fast {
                            cx.theme().warning
                        } else {
                            muted
                        }),
                        if fast {
                            format!("{} · Fast", picker::effort_label(&config.effort))
                        } else {
                            picker::effort_label(&config.effort)
                        },
                    ))
                },
            )
            .into_any_element()
    }

    fn picker_popover(
        &self,
        menu: Menu,
        label: String,
        icon: Option<Icon>,
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
            .when_some(icon, |button, icon| button.icon(icon))
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
                    .w(if menu == Menu::Agent {
                        px(264.)
                    } else {
                        px(220.)
                    })
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
        let mut panel = v_flex()
            .id("provider-menu")
            .debug_selector(|| "provider-menu".into())
            .gap_1()
            .text_sm();
        let mut body = v_flex()
            .id("provider-menu-scroll")
            .track_scroll(&self.picker.scroll)
            .overflow_y_scroll()
            .min_h_0()
            .max_h(px(
                (f32::from(window.viewport_size().height) - 250.).max(110.)
            ));
        let mut positions = Vec::new();
        let mut children = 0;
        match menu {
            Menu::Agent => {
                if self.picker.statuses.is_empty() {
                    body = body.child(div().p_2().child("Checking installed agents…"));
                }
                let mut flat = 0;
                for status in &self.picker.statuses {
                    let account = picker::account_view(status);
                    let (icon, name) = (
                        provider_icon(status.provider, cx),
                        status.provider.presentation().label,
                    );
                    body = body.child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .px_2()
                            .pt_2()
                            .pb_1()
                            .child(icon)
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(name),
                            )
                            .children(plan_pill(status, &account, cx)),
                    );
                    children += 1;
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
                        children += 1;
                    }
                    if let Some(unavailable) = account.unavailable {
                        body = body.child(
                            div()
                                .px_2()
                                .pb_1()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(unavailable),
                        );
                        children += 1;
                    }
                    if let Some(error) = account.error {
                        body = body
                            .child(div().px_2().font_family("monospace").text_xs().child(error));
                        children += 1;
                    }
                    for row in picker::model_rows(status) {
                        let selected = config.as_ref().is_some_and(|a| {
                            a.provider == status.provider && a.model == row.selection
                        });
                        let mut choice = AgentConfig::cli_default(status.provider);
                        choice.model = row.selection;
                        body = body.child(self.picker_row(
                            flat,
                            (row.label, row.detail, selected, choice),
                            cx,
                        ));
                        positions.push(children);
                        children += 1;
                        flat += 1;
                    }
                }
                panel = panel.child(body);
                panel = panel.child(self.fast_row(config.as_ref(), cx));
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
                        .px_2()
                        .pt_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(checked),
                        )
                        .child(div().flex_1())
                        .child(
                            ui::icon_button(
                                "refresh-providers",
                                ui::IconName::RefreshCw,
                                if self.picker.refreshing {
                                    "Checking…"
                                } else {
                                    "Refresh"
                                },
                                cx,
                            )
                            .debug_selector(|| "refresh-providers".into())
                            .on_click(cx.listener(|v, _, w, cx| v.refresh_providers(w, cx))),
                        ),
                );
            }
            Menu::Effort => {
                panel = panel.child(
                    div()
                        .px_2()
                        .pt_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Effort"),
                );
                if let Some(config) = config.as_ref()
                    && let Some(status) = self
                        .picker
                        .statuses
                        .iter()
                        .find(|s| s.provider == config.provider)
                {
                    for (ix, (effort, detail)) in
                        picker::effort_rows(config, status).into_iter().enumerate()
                    {
                        let label = picker::effort_label(&effort);
                        let selected = config.effort == effort;
                        let mut choice = config.clone();
                        choice.effort = effort;
                        body =
                            body.child(self.picker_row(ix, (label, detail, selected, choice), cx));
                        positions.push(ix);
                    }
                } else {
                    body = body.child(div().p_2().child("Checking installed agents…"));
                }
                panel = panel.child(body);
            }
        }
        *self.picker.child_of_row.borrow_mut() = positions;
        let parent = cx.weak_entity();
        let panel = panel
            .track_focus(&self.picker_focus)
            .on_key_down(move |event, window, cx| {
                let _ = parent.update(cx, |v, cx| {
                    match event.keystroke.key.as_str() {
                        "up" => v.picker.row = v.picker.row.saturating_sub(1),
                        "down" => {
                            let count = v.menu_row_count(menu, cx);
                            v.picker.row = (v.picker.row + 1).min(count.saturating_sub(1));
                        }
                        "enter" => {
                            if let Some(choice) = v.menu_choice(menu, cx) {
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
                    if let Some(child) = v.picker.child_of_row.borrow().get(v.picker.row) {
                        v.picker.scroll.scroll_to_item(*child);
                    }
                    cx.stop_propagation();
                    cx.notify();
                });
            });
        if self.picker.pointer && !cx.reduce_motion() {
            panel
                .relative()
                .with_animation(
                    ("picker-enter", self.picker.epoch),
                    Animation::new(Duration::from_millis(150))
                        .with_easing(gpui_kit::component::animation::ease_out_cubic),
                    |p, delta| p.opacity(delta).top(px(-4. * (1. - delta))),
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        }
    }

    /// The rows the keys move over: every model, then Fast when the model has it.
    fn menu_row_count(&self, menu: Menu, cx: &App) -> usize {
        let config = self.thread().read(cx).agent_config();
        match menu {
            Menu::Agent => self.model_rows().len() + usize::from(self.fast_supported(config)),
            Menu::Effort => config
                .and_then(|a| {
                    self.picker
                        .statuses
                        .iter()
                        .find(|s| s.provider == a.provider)
                        .map(|s| picker::effort_rows(a, s).len())
                })
                .unwrap_or(0),
        }
    }

    pub(super) fn fast_supported(&self, config: Option<&AgentConfig>) -> bool {
        config.is_some_and(|config| {
            self.picker
                .statuses
                .iter()
                .find(|s| s.provider == config.provider)
                .is_some_and(|s| picker::fast_detail(config, s).is_some())
        })
    }

    /// What Enter on the highlighted row chooses.
    fn menu_choice(&self, menu: Menu, cx: &App) -> Option<AgentConfig> {
        let config = self.thread().read(cx).agent_config().cloned();
        match menu {
            Menu::Agent => {
                let rows = self.model_rows();
                if let Some((provider, row)) = rows.get(self.picker.row) {
                    let mut choice = AgentConfig::cli_default(*provider);
                    choice.model = row.selection.clone();
                    Some(choice)
                } else if self.picker.row == rows.len() && self.fast_supported(config.as_ref()) {
                    config.map(toggled_speed)
                } else {
                    None
                }
            }
            Menu::Effort => {
                let mut config = config?;
                let status = self
                    .picker
                    .statuses
                    .iter()
                    .find(|s| s.provider == config.provider)?;
                let (effort, _) = picker::effort_rows(&config, status)
                    .into_iter()
                    .nth(self.picker.row)?;
                config.effort = effort;
                Some(config)
            }
        }
    }

    fn fast_row(&self, config: Option<&AgentConfig>, cx: &mut Context<Self>) -> AnyElement {
        let supported = self.fast_supported(config);
        let on = config.is_some_and(|c| c.speed == Speed::Fast);
        let detail = config.and_then(|config| {
            self.picker
                .statuses
                .iter()
                .find(|s| s.provider == config.provider)
                .and_then(|s| picker::fast_detail(config, s).map(str::to_owned))
        });
        let warn = config.is_some_and(|config| {
            self.picker.statuses.iter().any(|s| {
                s.provider == config.provider
                    && matches!(&s.availability, providers::Availability::Ready { catalog, .. }
                        if catalog.source == providers::CatalogSource::CliAliases)
            })
        });
        let row = self.model_rows().len();
        let muted = cx.theme().muted_foreground;
        let toggle = config.cloned().map(toggled_speed);
        let hint = detail.clone();
        v_flex()
            .id("agent-fast-row")
            .border_t_1()
            .border_color(cx.theme().border)
            .mt_1()
            .px_2()
            .py_1p5()
            .rounded_md()
            .when(supported && self.picker.row == row, |r| {
                r.bg(crate::theme::material(cx).glass_selected)
            })
            .when(!supported, |r| r.opacity(0.5))
            .when_some(hint.filter(|_| supported), |r, hint| {
                r.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
                })
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(ui::IconName::Zap).text_color(muted))
                    .child(div().flex_1().text_color(muted).child("Fast"))
                    .child(
                        Switch::new("agent-fast")
                            .accessibility_label("Fast")
                            .checked(on)
                            .disabled(!supported)
                            .on_click(cx.listener(move |v, _, w, cx| {
                                if let Some(choice) = toggle.clone() {
                                    v.choose_agent(choice, w, cx)
                                }
                            })),
                    ),
            )
            .when(warn && supported, |r| {
                r.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().warning)
                        .child(detail.unwrap_or_default()),
                )
            })
            .into_any_element()
    }

    fn picker_row(
        &self,
        ix: usize,
        row: (String, Option<String>, bool, AgentConfig),
        cx: &Context<Self>,
    ) -> AnyElement {
        let (label, detail, selected, choice) = row;
        let material = crate::theme::material(cx);
        let mut row = h_flex()
            .id(("agent-row", ix))
            .debug_selector(move || format!("agent-row-{ix}"))
            .items_center()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .cursor_pointer()
            .when(ix == self.picker.row, |r| r.bg(material.glass_selected))
            .hover(|r| r.bg(material.glass_hover))
            .when_some(detail, |r, detail| {
                r.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(detail.clone()).build(window, cx)
                })
            });
        row = row.child(div().flex_1().min_w_0().child(ui::ellipsis(label)));
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

fn toggled_speed(mut config: AgentConfig) -> AgentConfig {
    config.speed = if config.speed == Speed::Fast {
        Speed::Normal
    } else {
        Speed::Fast
    };
    config
}

/// The model's name, or the agent's when it uses the CLI's own default.
fn model_label(config: &AgentConfig, status: Option<&ProviderStatus>) -> String {
    match &config.model {
        ModelSelection::CliDefault => config.provider.presentation().label.into(),
        ModelSelection::Model { id } => status
            .and_then(|s| picker::selected_model(config, s))
            .map_or(id.clone(), |m| m.label.clone()),
    }
}

fn provider_icon(provider: Provider, cx: &App) -> Icon {
    ui::provider_icon(provider, cx)
}

/// The plan, short, at the right of the provider's header; warm when the API is billed instead
/// of the subscription. Hovering shows the account and CLI version.
fn plan_pill(
    status: &ProviderStatus,
    account: &picker::AccountView,
    cx: &App,
) -> Option<AnyElement> {
    let providers::Availability::Ready {
        account: signed_in,
        warnings,
        ..
    } = &status.availability
    else {
        return None;
    };
    let api = signed_in.billing == Billing::ApiKey
        || warnings
            .iter()
            .any(|w| matches!(w, Warning::ApiKeyEnvironment { .. }));
    let text = if api {
        "API billing".to_string()
    } else {
        picker::effort_label(&EffortSelection::Chosen(
            signed_in
                .plan
                .as_ref()
                .filter(|p| !p.trim().is_empty())?
                .clone(),
        ))
    };
    let facts = [account.email.clone(), account.version.clone()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let color = if api {
        cx.theme().warning
    } else {
        cx.theme().muted_foreground
    };
    Some(
        div()
            .id("provider-account")
            .debug_selector(|| "provider-account".into())
            .flex_none()
            .text_xs()
            .text_color(color)
            .child(text)
            .when(!facts.is_empty(), |pill| {
                pill.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(facts.clone()).build(window, cx)
                })
            })
            .into_any_element(),
    )
}
