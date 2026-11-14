//! Translate egui keyboard input into CDP `Input.dispatchKeyEvent` params.

use eframe::egui::{Key, Modifiers};
use serde_json::{json, Value};

/// CDP `Input.Modifier` bitmask: Alt=1, Ctrl=2, Meta=4, Shift=8.
pub fn modifier_mask(modifiers: Modifiers) -> u64 {
    let mut mask = 0;
    if modifiers.alt {
        mask |= 1;
    }
    if modifiers.ctrl {
        mask |= 2;
    }
    if modifiers.mac_cmd {
        mask |= 4;
    }
    if modifiers.shift {
        mask |= 8;
    }
    mask
}

/// DOM `key`, `code` and Windows virtual key code for a non-text key, or for
/// a letter or digit pressed with a shortcut modifier.
fn describe(key: Key, shortcut: bool) -> Option<(String, String, u64)> {
    let named = |key: &str, vk: u64| Some((key.to_string(), key.to_string(), vk));
    match key {
        Key::Enter => named("Enter", 13),
        Key::Tab => named("Tab", 9),
        Key::Backspace => named("Backspace", 8),
        Key::Escape => named("Escape", 27),
        Key::Delete => named("Delete", 46),
        Key::Insert => named("Insert", 45),
        Key::Home => named("Home", 36),
        Key::End => named("End", 35),
        Key::PageUp => named("PageUp", 33),
        Key::PageDown => named("PageDown", 34),
        Key::ArrowLeft => named("ArrowLeft", 37),
        Key::ArrowUp => named("ArrowUp", 38),
        Key::ArrowRight => named("ArrowRight", 39),
        Key::ArrowDown => named("ArrowDown", 40),
        _ if shortcut => {
            let name = key.name();
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if c.is_ascii_alphabetic() {
                let upper = c.to_ascii_uppercase();
                Some((c.to_ascii_lowercase().to_string(), format!("Key{upper}"), upper as u64))
            } else if c.is_ascii_digit() {
                Some((c.to_string(), format!("Digit{c}"), c as u64))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Key down/up params for a key event, or `None` when it arrives as text.
pub fn key_events(key: Key, pressed: bool, modifiers: Modifiers) -> Option<Value> {
    let shortcut = modifiers.ctrl || modifiers.alt || modifiers.mac_cmd;
    let (key, code, vk) = describe(key, shortcut)?;
    let kind = if pressed { "rawKeyDown" } else { "keyUp" };
    Some(json!({
        "type": kind,
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": vk,
        "modifiers": modifier_mask(modifiers),
    }))
}

/// Key down params that type one character of text.
pub fn text_event(c: char, modifiers: Modifiers) -> Value {
    let text = c.to_string();
    let vk = if c.is_ascii_alphanumeric() {
        c.to_ascii_uppercase() as u64
    } else if c == ' ' {
        32
    } else {
        0
    };
    json!({
        "type": "keyDown",
        "key": text,
        "text": text,
        "windowsVirtualKeyCode": vk,
        "modifiers": modifier_mask(modifiers),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_map_to_dom_names() {
        let down = key_events(Key::Enter, true, Modifiers::NONE).unwrap();
        assert_eq!(down["type"], "rawKeyDown");
        assert_eq!(down["key"], "Enter");
        assert_eq!(down["windowsVirtualKeyCode"], 13);
        assert_eq!(key_events(Key::ArrowLeft, false, Modifiers::NONE).unwrap()["type"], "keyUp");
    }

    #[test]
    fn plain_letters_arrive_as_text_but_shortcuts_as_keys() {
        assert!(key_events(Key::A, true, Modifiers::NONE).is_none());
        let ctrl_a = key_events(Key::A, true, Modifiers::CTRL).unwrap();
        assert_eq!(ctrl_a["key"], "a");
        assert_eq!(ctrl_a["code"], "KeyA");
        assert_eq!(ctrl_a["windowsVirtualKeyCode"], 65);
        assert_eq!(ctrl_a["modifiers"], 2);
    }

    #[test]
    fn text_carries_the_character() {
        let event = text_event('é', Modifiers::SHIFT);
        assert_eq!(event["text"], "é");
        assert_eq!(event["modifiers"], 8);
    }
}
