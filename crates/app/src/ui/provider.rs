use gpui::{App, Hsla, Styled as _, rgb};
use gpui_kit::component::{ActiveTheme as _, Icon};
use newjen_core::providers::Provider;

use crate::assets::{CLAUDE_MARK, OPENAI_MARK};

/// The provider's mark color: Claude's own orange, Codex in the text color.
pub fn provider_color(provider: Provider, cx: &App) -> Hsla {
    match provider {
        Provider::Claude => Hsla::from(rgb(0xd97757)),
        Provider::Codex => cx.theme().foreground,
    }
}

/// The provider's own mark: Claude's for Claude, OpenAI's for Codex.
pub fn provider_icon(provider: Provider, cx: &App) -> Icon {
    let path = match provider {
        Provider::Claude => CLAUDE_MARK,
        Provider::Codex => OPENAI_MARK,
    };
    Icon::default()
        .path(path)
        .text_color(provider_color(provider, cx))
}
