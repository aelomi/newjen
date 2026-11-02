use gpui::{App, Hsla, Styled as _, rgb};
use gpui_kit::component::{ActiveTheme as _, Icon};
use newjen_core::providers::Provider;

use super::IconName;

/// The provider's mark color: Claude's own orange, Codex in the text color.
fn provider_color(provider: Provider, cx: &App) -> Hsla {
    match provider {
        Provider::Claude => Hsla::from(rgb(0xd97757)),
        Provider::Codex => cx.theme().foreground,
    }
}

/// The provider's mark.
pub fn provider_icon(provider: Provider, cx: &App) -> Icon {
    let name = match provider {
        Provider::Claude => IconName::Sparkles,
        Provider::Codex => IconName::Sparkle,
    };
    Icon::new(name).text_color(provider_color(provider, cx))
}
