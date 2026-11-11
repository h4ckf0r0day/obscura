use serde_json::json;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::Window;

use super::Packet;

#[derive(Default)]
pub(super) struct InputState {
    pointer: PhysicalPosition<f64>,
    last_pointer: Option<(f64, f64)>,
    wheel: (f64, f64),
    buttons: u32,
    modifiers: ModifiersState,
    is_composing: bool,
}

fn coordinates(pointer: PhysicalPosition<f64>, surface: (u32, u32), image: (u32, u32)) -> (f64, f64) {
    let x = pointer.x * f64::from(image.0) / f64::from(surface.0.max(1));
    let y = pointer.y * f64::from(image.1) / f64::from(surface.1.max(1));
    (x, y)
}

impl InputState {
    fn modifiers(&self) -> u32 {
        u32::from(self.modifiers.alt_key()) | u32::from(self.modifiers.control_key()) << 1
            | u32::from(self.modifiers.super_key()) << 2 | u32::from(self.modifiers.shift_key()) << 3
    }

    fn mouse(&self, kind: &str, position: (f64, f64), button: &str) -> Packet {
        Packet::Input { method: "dispatchMouseEvent".into(), params: json!({
            "type": kind, "x": position.0, "y": position.1, "button": button,
            "buttons": self.buttons, "clickCount": 1, "modifiers": self.modifiers(),
        }) }
    }

    pub fn event(&mut self, event: &WindowEvent, window: &Window, image: (u32, u32)) -> Option<Packet> {
        let size = window.inner_size();
        let position = coordinates(self.pointer, (size.width, size.height), image);
        match event {
            WindowEvent::CursorMoved { position, .. } => { self.pointer = *position; None }
            WindowEvent::ModifiersChanged(modifiers) => { self.modifiers = modifiers.state(); None }
            WindowEvent::MouseInput { state, button, .. } => {
                let (button, mask) = match button {
                    MouseButton::Left => ("left", 1), MouseButton::Right => ("right", 2),
                    MouseButton::Middle => ("middle", 4), MouseButton::Back => ("back", 8),
                    MouseButton::Forward => ("forward", 16), _ => return None,
                };
                let is_pressed = *state == ElementState::Pressed;
                if is_pressed { self.buttons |= mask; } else { self.buttons &= !mask; }
                Some(self.mouse(if is_pressed { "mousePressed" } else { "mouseReleased" }, position, button))
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (f64::from(*x) * 40.0, f64::from(*y) * 40.0),
                    MouseScrollDelta::PixelDelta(point) => (point.x / window.scale_factor(), point.y / window.scale_factor()),
                };
                self.wheel.0 = (self.wheel.0 - x).clamp(-4096.0, 4096.0);
                self.wheel.1 = (self.wheel.1 - y).clamp(-4096.0, 4096.0);
                None
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let (key, virtual_code) = key_name(&event.logical_key);
                let code = match event.physical_key { PhysicalKey::Code(code) => format!("{code:?}"), _ => String::new() };
                let text = if self.is_composing || self.modifiers.control_key() || self.modifiers.super_key() {
                    ""
                } else {
                    event.text.as_deref().filter(|text| text.chars().all(|character| !character.is_control())).unwrap_or("")
                };
                Some(Packet::Input { method: "dispatchKeyEvent".into(), params: json!({
                    "type": if event.state == ElementState::Pressed { "keyDown" } else { "keyUp" },
                    "key": key, "code": code, "text": text, "windowsVirtualKeyCode": virtual_code,
                    "modifiers": self.modifiers(),
                }) })
            }
            WindowEvent::Ime(Ime::Preedit(text, _)) => { self.is_composing = !text.is_empty(); None }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.is_composing = false;
                Some(Packet::Input { method: "insertText".into(), params: json!({ "text": text }) })
            }
            WindowEvent::Focused(false) => { self.buttons = 0; self.modifiers = ModifiersState::empty(); None }
            _ => None,
        }
    }

    pub fn motion(&mut self, window: &Window, image: (u32, u32)) -> Vec<Packet> {
        let size = window.inner_size();
        let position = coordinates(self.pointer, (size.width, size.height), image);
        let mut packets = Vec::new();
        if self.last_pointer != Some(position) {
            self.last_pointer = Some(position);
            packets.push(self.mouse("mouseMoved", position, "none"));
        }
        if self.wheel != (0.0, 0.0) {
            packets.push(Packet::Input { method: "dispatchMouseEvent".into(), params: json!({
                "type": "mouseWheel", "x": position.0, "y": position.1,
                "deltaX": self.wheel.0, "deltaY": self.wheel.1, "modifiers": self.modifiers(),
            }) });
            self.wheel = (0.0, 0.0);
        }
        packets
    }
}

fn key_name(key: &Key) -> (String, u32) {
    match key {
        Key::Character(text) => (text.to_string(), text.chars().next().filter(char::is_ascii).map_or(0, |character| character.to_ascii_uppercase() as u32)),
        Key::Named(key) => {
            let code = match key {
                NamedKey::Backspace => 8, NamedKey::Tab => 9, NamedKey::Enter => 13,
                NamedKey::Escape => 27, NamedKey::Space => 32, NamedKey::PageUp => 33,
                NamedKey::PageDown => 34, NamedKey::End => 35, NamedKey::Home => 36,
                NamedKey::ArrowLeft => 37, NamedKey::ArrowUp => 38, NamedKey::ArrowRight => 39,
                NamedKey::ArrowDown => 40, NamedKey::Delete => 46, _ => 0,
            };
            (if *key == NamedKey::Space { " ".into() } else { format!("{key:?}") }, code)
        }
        _ => ("Unidentified".into(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_pointer_mapping_tracks_hidpi_and_a_resized_previous_frame() {
        assert_eq!(coordinates(PhysicalPosition::new(400.0, 200.0), (2560, 1440), (1280, 720)), (200.0, 100.0));
        assert_eq!(coordinates(PhysicalPosition::new(400.0, 200.0), (800, 400), (1200, 600)), (600.0, 300.0));
        assert_eq!(key_name(&Key::Named(NamedKey::Enter)), ("Enter".into(), 13));
        assert_eq!(key_name(&Key::Character("ñ".into())), ("ñ".into(), 0));
    }
}
