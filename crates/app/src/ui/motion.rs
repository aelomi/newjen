use super::IconName;
use gpui::{prelude::*, *};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};

#[derive(IntoElement)]
pub struct Chevron {
    open: bool,
    size: Option<Pixels>,
}

impl Chevron {
    pub fn new(open: bool) -> Self {
        Self { open, size: None }
    }
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }
}

impl RenderOnce for Chevron {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        Icon::new(if self.open {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        })
        .text_color(cx.theme().muted_foreground)
        .when_some(self.size, |icon, size| icon.with_size(size))
    }
}
