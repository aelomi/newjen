use std::rc::Rc;

use gpui::Focusable as _;
use std::cell::Cell;

use gpui::{
    AnyElement, App, Context, Entity, IntoElement, ParentElement, Pixels, RenderOnce, Styled,
    Window, canvas, div, prelude::*,
};
use gpui_kit::base::input::{Enter, TextareaState};
use gpui_kit::component::{ActiveTheme as _, input::Textarea};

use super::button::icon_button_with_look;
use super::{IconName, Look, ellipsis};
use crate::sidebar::OnPress;
use crate::theme;

pub fn composer_field(window: &mut Window, cx: &mut Context<TextareaState>) -> TextareaState {
    TextareaState::new(window, cx).auto_grow(1, 8)
}

pub enum Footer {
    Send(OnPress),
    Stop(OnPress),
    /// While a worktree is made.
    Cancel(OnPress),
    /// Disabled, with this hint.
    Wait(&'static str),
    Disabled(Option<&'static str>),
}

#[derive(IntoElement)]
pub struct Composer {
    field: Entity<TextareaState>,
    footer: Footer,
    chips: Option<AnyElement>,
    works_in: Option<AnyElement>,
    meter: Option<AnyElement>,
    top: Option<Rc<Cell<Option<Pixels>>>>,
}

impl Composer {
    pub fn new(field: Entity<TextareaState>, footer: Footer) -> Self {
        Self {
            field,
            footer,
            chips: None,
            works_in: None,
            meter: None,
            top: None,
        }
    }

    /// Where the card's top edge is written as it paints, for what opens above the card.
    pub fn top_sink(mut self, top: Rc<Cell<Option<Pixels>>>) -> Self {
        self.top = Some(top);
        self
    }

    /// The agent and effort chips, at the left of the bottom row.
    pub fn chips(mut self, chips: Option<AnyElement>) -> Self {
        self.chips = chips;
        self
    }

    /// The Local | worktree control, left of the send button.
    pub fn works_in(mut self, works_in: Option<AnyElement>) -> Self {
        self.works_in = works_in;
        self
    }

    /// The context chip, between the Local | worktree control and the send button.
    pub fn meter(mut self, meter: Option<AnyElement>) -> Self {
        self.meter = meter;
        self
    }
}

impl RenderOnce for Composer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ready = !self.field.read(cx).value().trim().is_empty();
        let name = self.field.read(cx).presentation().placeholder().clone();
        let send = match &self.footer {
            Footer::Send(send) => Some(send.clone()),
            Footer::Stop(_) | Footer::Cancel(_) | Footer::Wait(_) | Footer::Disabled(_) => None,
        };
        let waiting = matches!(self.footer, Footer::Wait(_) | Footer::Disabled(_));
        let focused = self
            .field
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        let (status, button) = match self.footer {
            Footer::Cancel(cancel) => (
                None,
                Some((
                    "cancel-copy",
                    icon_button_with_look(
                        "cancel-copy",
                        IconName::X,
                        "Cancel the copy (⌘.)",
                        Look::Secondary,
                        false,
                        cx,
                    )
                    .rounded_full()
                    .on_click(move |_, window, cx| cancel(window, cx)),
                )),
            ),
            Footer::Stop(stop) => (
                None,
                Some((
                    "stop",
                    icon_button_with_look(
                        "stop",
                        IconName::Square,
                        "Stop (⌘.)",
                        Look::Secondary,
                        false,
                        cx,
                    )
                    .rounded_full()
                    .on_click(move |_, window, cx| stop(window, cx)),
                )),
            ),
            Footer::Send(send) => (
                None,
                Some((
                    "send",
                    icon_button_with_look(
                        "send",
                        IconName::ArrowUp,
                        "Send (Enter)",
                        Look::Primary,
                        !ready,
                        cx,
                    )
                    .rounded_full()
                    .on_click(move |_, window, cx| send(window, cx)),
                )),
            ),
            Footer::Wait(why) => (Some(why), None),
            Footer::Disabled(why) => (
                why,
                Some((
                    "send",
                    icon_button_with_look(
                        "send",
                        IconName::ArrowUp,
                        "Send (Enter)",
                        Look::Primary,
                        true,
                        cx,
                    )
                    .rounded_full(),
                )),
            ),
        };
        let material = theme::material(cx);
        let footer = div()
            .flex()
            .items_center()
            .gap_2()
            .children(self.chips)
            .child(div().flex_1())
            .children(status.map(|why| {
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(ellipsis(why))
            }))
            .children(self.works_in)
            .children(self.meter)
            .children(button.map(|(name, button)| {
                div().debug_selector(move || name.to_string()).child(button)
            }));
        div()
            .id("composer")
            .debug_selector(|| "composer".to_string())
            .relative()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_xl()
            .border_1()
            .border_color(if focused {
                cx.theme().ring
            } else {
                material.card_stroke
            })
            .bg(material.glass_hover)
            .child(
                div()
                    .capture_action(move |action: &Enter, window, cx| {
                        if action.shift {
                            return;
                        }
                        cx.stop_propagation();
                        if let Some(send) = send.as_ref().filter(|_| !action.secondary && ready) {
                            send(window, cx);
                        }
                    })
                    .child(
                        Textarea::new(&self.field)
                            .appearance(false)
                            .aria_label(name)
                            .disabled(waiting),
                    ),
            )
            .child(footer)
            .children(self.top.map(|top| {
                canvas(
                    |_, _, _| {},
                    move |bounds, _, _, _| top.set(Some(bounds.top())),
                )
                .absolute()
                .inset_0()
                .size_full()
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;
    use gpui::{AppContext, FocusHandle, Render, TestAppContext, VisualTestContext};
    use gpui_kit::base::Root;

    struct Example {
        field: Entity<TextareaState>,
        root: FocusHandle,
        busy: bool,
        sent: Vec<String>,
        stopped: bool,
    }

    impl Render for Example {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let view = cx.entity();
            let send: OnPress = Rc::new(move |window, cx| {
                view.update(cx, |view, cx| {
                    view.sent.push(view.field.read(cx).value().to_string());
                    view.field.update(cx, |field, cx| field.clean(window, cx));
                    cx.notify();
                })
            });
            let view = cx.entity();
            let stop: OnPress = Rc::new(move |_, cx| {
                view.update(cx, |view, cx| {
                    view.stopped = true;
                    cx.notify();
                })
            });
            div()
                .key_context(keys::CONTEXT)
                .track_focus(&self.root)
                .w(gpui::px(600.))
                .on_action(cx.listener(|view, _: &keys::Stop, _, cx| {
                    view.stopped = true;
                    cx.notify();
                }))
                .child(Composer::new(
                    self.field.clone(),
                    if self.busy {
                        Footer::Stop(stop)
                    } else {
                        Footer::Send(send)
                    },
                ))
        }
    }

    fn setup(cx: &mut TestAppContext) -> (Entity<Example>, &mut VisualTestContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);

            keys::bind(cx);
        });
        let mut example = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| {
                let field = cx.new(|cx| composer_field(window, cx));
                field.read(cx).focus_handle(cx).focus(window, cx);
                Example {
                    field,
                    root: cx.focus_handle(),
                    busy: false,
                    sent: vec![],
                    stopped: false,
                }
            });
            example = Some(view.clone());
            Root::new(view, window, cx)
        });
        (example.unwrap(), cx)
    }

    fn value(view: &Entity<Example>, cx: &mut VisualTestContext) -> String {
        cx.update(|_, cx| view.read(cx).field.read(cx).value().to_string())
    }

    #[gpui::test]
    fn enter_sends_and_clean_is_undoable(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.simulate_keystrokes("enter");
        assert_eq!(value(&view, cx), "");
        assert!(cx.update(|_, cx| view.read(cx).sent.is_empty()));
        cx.simulate_input("hello");
        cx.simulate_keystrokes("cmd-enter");
        assert_eq!(value(&view, cx), "hello");
        assert!(cx.update(|_, cx| view.read(cx).sent.is_empty()));
        cx.simulate_keystrokes("enter");
        assert_eq!(value(&view, cx), "");
        assert_eq!(cx.update(|_, cx| view.read(cx).sent.clone()), ["hello"]);
        cx.simulate_keystrokes("cmd-z");
        assert_eq!(value(&view, cx), "hello");
    }

    #[gpui::test]
    fn shift_enter_edits_and_busy_enter_never_edits(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.simulate_input("hello");
        cx.simulate_keystrokes("shift-enter");
        assert_eq!(value(&view, cx), "hello\n");
        assert!(cx.update(|_, cx| view.read(cx).sent.is_empty()));
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.busy = true;
                cx.notify();
            })
        });
        cx.simulate_keystrokes("enter cmd-enter");
        assert_eq!(value(&view, cx), "hello\n");
        assert!(cx.update(|_, cx| view.read(cx).sent.is_empty()));
        cx.simulate_keystrokes("cmd-.");
        assert!(cx.update(|_, cx| view.read(cx).stopped));
    }

    #[gpui::test]
    fn cmd_alt_arrows_never_add_a_second_cursor(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.simulate_input("alpha");
        cx.simulate_keystrokes("shift-enter");
        cx.simulate_input("beta");
        cx.simulate_keystrokes("cmd-alt-up");
        cx.simulate_input("X");
        cx.simulate_keystrokes("cmd-alt-down");
        cx.simulate_input("Y");
        assert_eq!(value(&view, cx), "alpha\nbetaXY");
    }

    #[gpui::test]
    fn tab_leaves_the_field_without_inserting_text(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.simulate_input("hello");
        cx.simulate_keystrokes("tab");
        assert_eq!(value(&view, cx), "hello");
        assert!(!cx.update(|window, cx| {
            view.read(cx)
                .field
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        }));
    }

    #[gpui::test]
    fn kit_edit_actions_keep_cut_copy_paste_and_redo(cx: &mut TestAppContext) {
        use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
        let (view, cx) = setup(cx);
        cx.simulate_input("draft text");
        cx.dispatch_action(SelectAll);
        cx.dispatch_action(Copy);
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
            "draft text"
        );
        cx.dispatch_action(Cut);
        assert_eq!(value(&view, cx), "");
        cx.dispatch_action(Paste);
        assert_eq!(value(&view, cx), "draft text");
        cx.dispatch_action(Undo);
        assert_eq!(value(&view, cx), "");
        cx.dispatch_action(Redo);
        assert_eq!(value(&view, cx), "draft text");
    }

    #[gpui::test]
    fn grows_to_eight_rows_then_scrolls(cx: &mut TestAppContext) {
        let (view, cx) = setup(cx);
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let one = cx.debug_bounds("composer").unwrap().size.height;
        for _ in 1..8 {
            cx.simulate_keystrokes("shift-enter");
        }
        let eight = cx.debug_bounds("composer").unwrap().size.height;
        assert!(eight > one * 2.);
        cx.simulate_keystrokes("shift-enter shift-enter shift-enter");
        assert_eq!(cx.debug_bounds("composer").unwrap().size.height, eight);
        assert_ne!(
            cx.update(|_, cx| view.read(cx).field.read(cx).scroll_offset().y),
            gpui::px(0.)
        );
    }
}
