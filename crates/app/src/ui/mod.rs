//! GPUI Kit components and Newjen’s approval card, tool card and pulse.

mod button;
mod chrome;
mod monogram;
mod status;
mod text;

pub use button::{Look, Size, button, button_with_icon, icon_button};
pub use chrome::{ListRow, card, card_title_bar, glass_title_bar, list_row};
pub use gpui_kit::assets::IconName;
#[allow(unused_imports)]
pub use monogram::{monogram, monogram_color};
pub use status::{AgentState, Pulse, Tone, agent_pill, status_badge};
pub use text::{caption, ellipsis, key_caps, middle_ellipsis, title};

use gpui::{Hsla, Styled};
use gpui_kit::component::{Icon, Sizable};

/// A Lucide icon, `size` square, in `color`. Kit icons otherwise take the surrounding text color.
pub fn icon(name: IconName, size: gpui_kit::component::Size, color: Hsla) -> Icon {
    Icon::new(name).with_size(size).text_color(color)
}

mod code;
mod copy;
mod message;
mod motion;
mod step;
mod tool_card;
pub use code::CodeBlock;
pub use copy::CopyButton;
pub use message::{ErrorNotice, assistant, user};
pub use motion::Chevron;
pub use step::{StepState, step_mark};
pub use tool_card::ToolCard;

mod message_list;
pub use message_list::MessageList;

mod composer;
pub use composer::{Composer, Footer, composer_field};

mod approval_card;
pub use approval_card::{ApprovalCard, DOCK_METRICS, EXIT_FADE, approval_dock};
