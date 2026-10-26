//! Keyboard shortcuts.
//!
//! GPUI turns a key press into an *action* (a small named struct, like a Redux action) and
//! delivers it to whichever element handles it. `bind` maps keys to actions; the chat view
//! handles them with `on_action`.
//!
//! The approval keys (⌘↵ allow once, Esc deny) are the exception: they are read as raw key
//! presses, see `approval_key`.

use gpui::{App, KeyBinding, KeyDownEvent, actions};
use newjen_core::PermissionAnswer;

// Each name becomes a unit struct, e.g. `pub struct Stop;`.
actions!(
    newjen,
    [Quit, Hide, HideOthers, Minimize, CloseWindow, Stop]
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
        // Works from the text field too: it stops the running turn.
        KeyBinding::new("cmd-.", Stop, Some(CONTEXT)),
    ]);
}

/// What a key press on the approval dock means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKey {
    /// A fresh press of ⌘↵ (allow once) or Esc (deny).
    Answer(PermissionAnswer),
    /// The same keys repeating because they are held down. These must answer nothing.
    Repeat,
}

/// ⌘↵ and Esc while an approval is pending; `None` for any other key.
///
/// Why raw key presses instead of a key binding: holding a key makes macOS repeat it, and only
/// the raw `KeyDownEvent` says so (`is_held`). A key binding's action carries no event, and
/// GPUI runs actions before key listeners, so an action handler cannot tell a repeat from a
/// fresh press. Without this, holding ⌘↵ would allow one request and then, once the next
/// queued request's short delay ran out, allow that one too without the user seeing it.
pub fn approval_key(event: &KeyDownEvent) -> Option<ApprovalKey> {
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    let others = modifiers.control || modifiers.alt || modifiers.shift;
    let answer = match keystroke.key.as_str() {
        "enter" if modifiers.platform && !others => PermissionAnswer::AllowOnce,
        "escape" if !modifiers.platform && !others => PermissionAnswer::Deny,
        _ => return None,
    };
    Some(if event.is_held {
        ApprovalKey::Repeat
    } else {
        ApprovalKey::Answer(answer)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Keystroke;

    fn press(key: &'static str, is_held: bool) -> KeyDownEvent {
        KeyDownEvent {
            keystroke: Keystroke::parse(key).unwrap(),
            is_held,
            prefer_character_input: false,
        }
    }

    #[test]
    fn approval_keys_answer_only_on_a_fresh_press() {
        assert_eq!(
            approval_key(&press("cmd-enter", false)),
            Some(ApprovalKey::Answer(PermissionAnswer::AllowOnce))
        );
        assert_eq!(
            approval_key(&press("escape", false)),
            Some(ApprovalKey::Answer(PermissionAnswer::Deny))
        );
        // Held down: macOS repeats the key, and a repeat answers nothing.
        assert_eq!(
            approval_key(&press("cmd-enter", true)),
            Some(ApprovalKey::Repeat)
        );
        assert_eq!(
            approval_key(&press("escape", true)),
            Some(ApprovalKey::Repeat)
        );
        // Other keys, and the same keys with other modifiers, are not approval keys.
        assert_eq!(approval_key(&press("enter", false)), None);
        assert_eq!(approval_key(&press("cmd-shift-enter", false)), None);
        assert_eq!(approval_key(&press("cmd-escape", false)), None);
        assert_eq!(approval_key(&press("a", false)), None);
    }
}
