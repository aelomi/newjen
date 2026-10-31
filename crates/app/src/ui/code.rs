use gpui::{App, ElementId, IntoElement, RenderOnce, SharedString, Window};
use gpui_kit::component::text::TextView;

#[derive(IntoElement)]
pub struct CodeBlock {
    id: ElementId,
    text: SharedString,
    language: &'static str,
}

impl CodeBlock {
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            language: "text",
        }
    }
    pub fn language(mut self, language: &'static str) -> Self {
        self.language = language;
        self
    }
}

impl RenderOnce for CodeBlock {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let fence = "`".repeat(
            3.max(
                self.text
                    .lines()
                    .map(|line| line.trim_start().chars().take_while(|c| *c == '`').count() + 1)
                    .max()
                    .unwrap_or(3),
            ),
        );
        TextView::markdown(
            self.id,
            format!("{fence}{}\n{}\n{fence}", self.language, self.text),
        )
        .selectable(false)
        .stream_fade(false)
        .code_block_actions(|block, _, _| super::CopyButton::new("copy", block.code().to_string()))
    }
}
