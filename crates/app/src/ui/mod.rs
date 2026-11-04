//! GPUI Kit components and Newjen’s approval card and tool card.

mod button;
mod chrome;
mod monogram;
mod status;
mod text;

pub use button::{Look, Size, button, button_with_icon, icon_button};
pub use chrome::{card, card_title_bar, glass_title_bar};
pub use gpui_kit::assets::IconName;
#[allow(unused_imports)]
pub use monogram::{monogram, monogram_color, monogram_large};
pub use status::{Tone, status_badge};
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
mod popover;
mod provider;
mod ring;
mod step;
mod tool_card;
pub use code::CodeBlock;
pub use copy::CopyButton;
pub use message::{ErrorNotice, Pending, assistant, user, user_pending};
pub use motion::Chevron;
pub use popover::{popover_surface, solid_popover_surface};
pub use provider::{provider_color, provider_icon};
pub use ring::{Tick, ring, ring_with_tick};
pub use step::{
    Detail, Mark, Narration, Part, QuietMark, StepState, StepView, WorkBlockView, mark, step_mark,
};
pub use tool_card::{ToolBody, ToolCard};

mod gauge;
pub use gauge::{GaugeLabel, GaugeLook, UsageGauge};

mod message_list;
pub use message_list::MessageList;

mod composer;
pub use composer::{Composer, Footer, composer_field};

mod approval_card;
pub use approval_card::{ApprovalCard, DOCK_METRICS, EXIT_FADE, approval_dock};
