//! Keyboard shortcuts.
//!
//! GPUI turns a key press into an *action* (a small named struct, like a Redux action) and
//! delivers it to whichever element handles it. `bind` maps keys to actions; the chat view
//! handles them with `on_action`.

use gpui::{App, KeyBinding, actions};

// Each name becomes a unit struct, e.g. `pub struct AllowOnce;`.
actions!(
    newjen,
    [
        Quit,
        Hide,
        HideOthers,
        Minimize,
        CloseWindow,
        AllowOnce,
        Deny,
        Stop
    ]
);

/// The key context the chat view sets on its root element. Bindings with this context only fire
/// while focus is inside the chat view.
pub const CONTEXT: &str = "Newjen";

/// Registers every shortcut. Call once at startup, after `ely_gpui_component::init`.
pub fn bind(cx: &mut App) {
    cx.bind_keys([
        // Global, the standard Mac app and window keys: GPUI and Ely install none of them.
        // The menu bar shows these next to its items.
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        // While an approval is pending the text field is disabled and the view's root has focus,
        // so these reach the view instead of the field (which would treat ⌘↵ as its own Submit).
        KeyBinding::new("cmd-enter", AllowOnce, Some(CONTEXT)),
        KeyBinding::new("escape", Deny, Some(CONTEXT)),
        // Works from the text field too: it stops the running turn.
        KeyBinding::new("cmd-.", Stop, Some(CONTEXT)),
    ]);
}
