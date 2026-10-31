use gpui::{App, ElementId, IntoElement, RenderOnce, SharedString, Window};
use gpui_kit::component::clipboard::Clipboard;

#[derive(IntoElement)]
pub struct CopyButton {
    id: ElementId,
    text: SharedString,
}

impl CopyButton {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
        }
    }
}

impl RenderOnce for CopyButton {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Clipboard::new(self.id).value(self.text).tooltip("Copy")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Context, Modifiers, Render, TestAppContext, div, point, prelude::*, px};
    struct Example;
    impl Render for Example {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(CopyButton::new(
                "copy",
                "full source\nincluding hidden lines",
            ))
        }
    }
    #[gpui::test]
    fn copy_keeps_the_full_source(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Example);
        cx.run_until_parked();
        cx.simulate_click(point(px(12.), px(12.)), Modifiers::none());
        assert_eq!(
            cx.update(|_, cx| cx.read_from_clipboard().unwrap().text().unwrap()),
            "full source\nincluding hidden lines"
        );
    }
}
