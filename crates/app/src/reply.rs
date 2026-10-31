use gpui::{App, AppContext as _, Entity};
use gpui_kit::component::text::{TextView, TextViewState};
use newjen_core::Item;
use std::collections::HashMap;

#[derive(Default)]
pub struct Replies {
    entries: HashMap<u64, Reply>,
    settled: usize,
    boundary: Option<u64>,
}

impl Replies {
    pub fn get(&self, id: &u64) -> Option<&Reply> {
        self.entries.get(id)
    }
    /// Parse saved replies only when their row first enters the viewport.
    pub fn ensure(&mut self, id: u64, text: &str, cx: &mut App) {
        self.entries
            .entry(id)
            .or_insert_with(|| Reply::new(text, cx));
    }
    pub fn contains(&self, id: &u64) -> bool {
        self.entries.contains_key(id)
    }
}

pub struct Reply {
    state: Entity<TextViewState>,
    source: String,
    #[cfg(test)]
    updates: usize,
}

impl Reply {
    fn new(text: &str, cx: &mut App) -> Self {
        Self {
            state: cx.new(|cx| TextViewState::markdown(text, cx).selectable(false)),
            source: text.into(),
            #[cfg(test)]
            updates: 0,
        }
    }
    fn update(&mut self, text: &str, cx: &mut App) {
        #[cfg(test)]
        {
            self.updates += 1;
        }
        if self.source == text {
            return;
        }
        self.source = text.into();
        self.state.update(cx, |state, cx| state.set_text(text, cx));
    }
    pub fn view(&self, _: &App) -> TextView {
        TextView::new(&self.state)
            .selectable(false)
            .stream_fade(false)
            .code_block_actions(|block, _, _| {
                crate::ui::CopyButton::new("copy", block.code().to_string())
            })
    }
}

pub fn sync(replies: &mut Replies, items: &[Item], cx: &mut App) {
    if replies.settled > 0 && items.get(replies.settled - 1).map(Item::id) != replies.boundary {
        replies.settled = 0;
    }
    let mut settled = true;
    for (ix, item) in items.iter().enumerate().skip(replies.settled) {
        if let Item::Assistant { id, text, live, .. } = item {
            settled &= !*live;
            if *live || replies.entries.contains_key(id) {
                replies
                    .entries
                    .entry(*id)
                    .or_insert_with(|| Reply::new(text, cx))
                    .update(text, cx);
            }
        }
        if settled {
            replies.settled = ix + 1;
            replies.boundary = Some(item.id());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use gpui::{Context, IntoElement, ParentElement as _, Render, TestAppContext, Window, div};

    struct Example {
        reply: Reply,
        mounted: bool,
    }
    impl Render for Example {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div().child(if self.mounted {
                self.reply.view(cx).into_any_element()
            } else {
                div().into_any_element()
            })
        }
    }

    #[gpui::test]
    fn a_large_reply_keeps_its_parsed_text_while_unmounted(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
        });
        let text =
            "A long saved reply survives scrolling away and switching threads.\n\n".repeat(150);
        let (view, cx) = cx.add_window_view(|_, cx| Example {
            reply: Reply::new(&text, cx),
            mounted: true,
        });
        cx.run_until_parked();
        let state = cx.update(|_, cx| view.read(cx).reply.state.clone());
        let original = cx.update(|_, cx| state.read(cx).rendered_text());
        assert!(original.as_str().len() > 4096);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.mounted = false;
                cx.notify();
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.mounted = true;
                cx.notify();
            })
        });
        let remounted = cx.update(|_, cx| state.read(cx).rendered_text());
        assert_eq!(original, remounted);
        assert_eq!(
            state.entity_id(),
            cx.update(|_, cx| view.read(cx).reply.state.entity_id())
        );
        cx.run_until_parked();
        assert_eq!(original, cx.update(|_, cx| state.read(cx).rendered_text()));
    }

    #[gpui::test]
    fn notifications_only_update_the_unsettled_tail(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
        });
        let mut replies = Replies::default();
        let mut items: Vec<_> = (0..20)
            .map(|id| Item::Assistant {
                id,
                text: "unchanged history ".repeat(600),
                live: false,
                interrupted: false,
            })
            .collect();
        cx.update(|cx| {
            sync(&mut replies, &items, cx);
        });
        assert_eq!(replies.settled, 20);
        assert!(replies.entries.is_empty());
        cx.update(|cx| {
            for item in &items[..2] {
                replies.ensure(item.id(), "unchanged history", cx);
            }
        });
        let previous: Vec<_> = replies
            .entries
            .values()
            .map(|reply| (reply.state.entity_id(), reply.updates))
            .collect();
        items.push(Item::Assistant {
            id: 20,
            text: "new".into(),
            live: true,
            interrupted: false,
        });
        cx.update(|cx| {
            sync(&mut replies, &items, cx);
        });
        for (id, updates) in previous {
            assert!(
                replies
                    .entries
                    .values()
                    .any(|reply| reply.state.entity_id() == id && reply.updates == updates)
            );
        }
        assert_eq!(replies.settled, 20);
        if let Item::Assistant { text, live, .. } = &mut items[20] {
            *text = "end".into();
            *live = false;
        }
        cx.update(|cx| {
            sync(&mut replies, &items, cx);
        });
        cx.run_until_parked();
        assert_eq!(replies.settled, 21);
        assert_eq!(
            cx.update(|cx| replies.get(&20).unwrap().state.read(cx).rendered_text())
                .as_str()
                .trim_end(),
            "end"
        );
    }

    #[gpui::test]
    fn streamed_text_appears_immediately_in_the_same_state(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut reply = cx.update(|cx| Reply::new("hello", cx));
        let state = reply.state.entity_id();
        cx.update(|cx| reply.update("hello world", cx));
        cx.run_until_parked();
        assert_eq!(state, reply.state.entity_id());
        assert_eq!(
            cx.update(|cx| reply.state.read(cx).rendered_text())
                .as_str()
                .trim_end(),
            "hello world"
        );
    }

    #[gpui::test]
    fn kit_markdown_keeps_inline_text_lists_and_links(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
        });
        let text = "A **bold** paragraph with `inline code` and [a link](https://example.com).\n\n3. first\n4. second\n   - nested\n";
        let (view, cx) = cx.add_window_view(|_, cx| Example {
            reply: Reply::new(text, cx),
            mounted: true,
        });
        cx.run_until_parked();
        let rendered = cx.update(|_, cx| view.read(cx).reply.state.read(cx).rendered_text());
        assert!(
            rendered
                .as_str()
                .contains("A bold paragraph with inline code and a link.")
        );
        assert!(rendered.as_str().contains("first\nsecond\nnested"));
    }
}
