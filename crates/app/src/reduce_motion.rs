//! macOS's Reduce Motion setting (System Settings › Accessibility › Display), passed on to Ely
//! and GPUI. Both have a switch for it: Ely shortens its transitions to nothing and drops the
//! working dot's pulse (`Theme::reduced_motion`), and GPUI stops repeating animations such as
//! spinners (`App::set_reduce_motion`). Neither reads the system setting itself.

use ely_gpui_component::theme::{ActiveTheme, Theme};
use gpui::App;
use objc2_app_kit::NSWorkspace;

/// Reads the setting and, when it differs from what Ely and GPUI have, tells both (which
/// redraws the window). Reading it is one cheap call, so the chat view's clock calls this every
/// second and a change in System Settings applies within a second.
pub fn sync(cx: &mut App) {
    let reduce = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
    if cx.theme().reduced_motion != reduce {
        Theme::update(cx, |theme| theme.reduced_motion = reduce);
    }
    // Does nothing when it is already set this way.
    cx.set_reduce_motion(reduce);
}
