//! Keyboard shortcuts.
//!
//! GPUI turns a key press into an *action* (a small named struct, like a Redux action) and
//! delivers it to whichever element handles it. `bind` maps keys to actions; the chat view
//! handles them with `on_action`. The menu bar shows the same keys next to its items.
//!
//! The approval keys (⌘↵ allow once, Esc deny) and Esc closing the thread drawer are the
//! exception: they are read as raw key presses, see `approval_key` and `key_route`.

use gpui::{Action, App, KeyBinding, KeyDownEvent, actions};
use newjen_core::PermissionAnswer;

// Each name becomes a unit struct, e.g. `pub struct Stop;`.
actions!(
    newjen,
    [
        Quit,
        Hide,
        HideOthers,
        Minimize,
        CloseWindow,
        Stop,
        NewThread,
        PreviousThread,
        NextThread,
        ToggleSidebar,
    ]
);

/// ⌘1 to ⌘9: show the thread at this place in the list, counting from 0. `no_json`: it is only
/// made here, never read from a keymap file.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = newjen, no_json)]
pub struct JumpToThread(pub usize);

/// ⌘1 to ⌘9: `JumpToThread(0)` to `JumpToThread(8)`.
pub const JUMP_KEYS: [&str; 9] = [
    "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9",
];

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
        KeyBinding::new("cmd-n", NewThread, Some(CONTEXT)),
        // ⌘⇧[ and ⌘⇧]. macOS reports shift-[ as `{` (and drops the shift), so these are the
        // keys GPUI actually sees; see `gpui-pre-macos`'s `events.rs`.
        KeyBinding::new("cmd-{", PreviousThread, Some(CONTEXT)),
        KeyBinding::new("cmd-}", NextThread, Some(CONTEXT)),
        KeyBinding::new("cmd-b", ToggleSidebar, Some(CONTEXT)),
    ]);
    cx.bind_keys(
        JUMP_KEYS
            .iter()
            .enumerate()
            .map(|(place, keys)| KeyBinding::new(keys, JumpToThread(place), Some(CONTEXT))),
    );
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

/// What a key press in the chat view does (see `key_route`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    /// Esc while the thread drawer is open: close it.
    CloseDrawer,
    /// ⌘↵ or Esc while an approval waits: answer it.
    Answer(PermissionAnswer),
    /// ⌘↵ or Esc held down while an approval waits: ours, but it answers nothing.
    Ignore,
    /// Not ours: the key goes on to whatever else handles it.
    Pass,
}

/// Where a key press goes. Esc first closes the thread drawer; otherwise ⌘↵ and Esc answer
/// a pending approval. Neither happens while a popover or menu holds the keyboard (the "N
/// allowed" list, a row's "Archive thread"): there Esc closes that popover or menu, and ⌘↵
/// must not allow something the user is not looking at.
pub fn key_route(
    event: &KeyDownEvent,
    drawer_open: bool,
    overlay_focused: bool,
    approval_pending: bool,
) -> KeyRoute {
    if overlay_focused {
        return KeyRoute::Pass;
    }
    if drawer_open && event.keystroke.key == "escape" {
        return KeyRoute::CloseDrawer;
    }
    if !approval_pending {
        return KeyRoute::Pass;
    }
    match approval_key(event) {
        Some(ApprovalKey::Answer(answer)) => KeyRoute::Answer(answer),
        Some(ApprovalKey::Repeat) => KeyRoute::Ignore,
        None => KeyRoute::Pass,
    }
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
    fn previous_and_next_thread_match_what_macos_sends() {
        // macOS turns ⌘⇧[ into `{` with only ⌘ held (see `bind`), so that is what the
        // bindings must name.
        for (keys, key) in [("cmd-{", "{"), ("cmd-}", "}")] {
            let parsed = Keystroke::parse(keys).unwrap();
            assert_eq!(parsed.key, key);
            assert_eq!(parsed.modifiers, gpui::Modifiers::command());
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

    #[test]
    fn an_open_popover_or_menu_keeps_its_keys() {
        // The dock is up and the user opened the "N allowed" list (or a row's menu): Esc
        // closes that, it never denies the request, and ⌘↵ allows nothing.
        for key in ["escape", "cmd-enter"] {
            assert_eq!(
                key_route(&press(key, false), false, true, true),
                KeyRoute::Pass
            );
        }
        // A menu open in the drawer: its Esc closes the menu, not the whole drawer.
        assert_eq!(
            key_route(&press("escape", false), true, true, false),
            KeyRoute::Pass
        );
    }

    #[test]
    fn esc_closes_the_drawer_before_it_answers_anything() {
        assert_eq!(
            key_route(&press("escape", false), true, false, true),
            KeyRoute::CloseDrawer
        );
        // With the drawer shut, the same press denies the waiting request.
        assert_eq!(
            key_route(&press("escape", false), false, false, true),
            KeyRoute::Answer(PermissionAnswer::Deny)
        );
        // The drawer does not use ⌘↵, so it still allows once.
        assert_eq!(
            key_route(&press("cmd-enter", false), true, false, true),
            KeyRoute::Answer(PermissionAnswer::AllowOnce)
        );
    }

    #[test]
    fn approval_keys_are_ours_only_while_a_request_waits() {
        // Nothing waits: Esc goes on to the text field (a half-typed IME character).
        assert_eq!(
            key_route(&press("escape", false), false, false, false),
            KeyRoute::Pass
        );
        assert_eq!(
            key_route(&press("escape", true), false, false, true),
            KeyRoute::Ignore
        );
        assert_eq!(
            key_route(&press("a", false), false, false, true),
            KeyRoute::Pass
        );
    }
}
