//! The browser window: tab strip, toolbar, page view and input forwarding.
//!
//! Each tab is a CDP target in one engine connection, so tabs share cookies
//! like a normal browser profile. Only the visible tab streams frames.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Id, Key, Modifiers,
    PointerButton, Pos2, Rect, ResizeDirection, Sense, Stroke, Vec2, ViewportCommand,
};
use serde_json::{json, Value};

use crate::cdp::{Cdp, Frame, Incoming};
use crate::engine::Engine;
use crate::look::{self, Palette};
use crate::{keys, omnibox};

const STRIP_HEIGHT: f32 = 40.0;
const TOOLBAR_HEIGHT: f32 = 44.0;
const RESIZE_BORDER: f32 = 5.0;
const CAPTION_BUTTON_WIDTH: f32 = 46.0;
const VIEWPORT_DEBOUNCE: Duration = Duration::from_millis(90);
const HOVER_PROBE_INTERVAL: Duration = Duration::from_millis(70);
/// A coalesced input command is resent if its response never arrives.
const IN_FLIGHT_TIMEOUT: Duration = Duration::from_millis(800);
const DOUBLE_CLICK: Duration = Duration::from_millis(450);

enum Pending {
    CreateTarget(u64),
    Navigate(u64),
    History(u64),
    Title(u64),
    Hover(u64),
    PointerMove,
    Wheel,
    CopySelection,
}

enum Link {
    Starting(mpsc::Receiver<anyhow::Result<Engine>>),
    Ready { engine: Engine, cdp: Cdp },
    Failed(String),
}

struct Tab {
    uid: u64,
    target: Option<String>,
    session: Option<String>,
    url: String,
    title: String,
    loading: bool,
    error: Option<String>,
    texture: Option<egui::TextureHandle>,
    css_size: Vec2,
    history: Vec<i64>,
    history_index: usize,
    queued_url: Option<String>,
    viewport: Option<(u32, u32, u32)>,
    streaming: bool,
    hover_link: String,
    cursor: CursorIcon,
}

impl Tab {
    fn new(uid: u64, url: Option<String>) -> Self {
        Tab {
            uid,
            target: None,
            session: None,
            url: url.clone().unwrap_or_else(|| omnibox::NEW_TAB_URL.into()),
            title: String::new(),
            loading: url.is_some(),
            error: None,
            texture: None,
            css_size: Vec2::ZERO,
            history: Vec::new(),
            history_index: 0,
            queued_url: url,
            viewport: None,
            streaming: false,
            hover_link: String::new(),
            cursor: CursorIcon::Default,
        }
    }

    fn is_new_tab_page(&self) -> bool {
        self.url == omnibox::NEW_TAB_URL && self.queued_url.is_none()
    }

    fn label(&self) -> String {
        if self.title.trim().is_empty() {
            omnibox::fallback_title(&self.url)
        } else {
            self.title.trim().to_string()
        }
    }

    fn can_go_back(&self) -> bool {
        self.history_index > 0 && self.history_index < self.history.len()
    }

    fn can_go_forward(&self) -> bool {
        self.history_index + 1 < self.history.len()
    }
}

#[derive(Default)]
struct Pointer {
    pending_move: Option<Pos2>,
    move_sent: Option<Instant>,
    wheel: Vec2,
    wheel_at: Pos2,
    wheel_sent: Option<Instant>,
    hover_due: Option<Pos2>,
    hover_sent: Option<Instant>,
    buttons: u64,
    last_press: Option<(Instant, Pos2, u64)>,
    press_count: u64,
}

pub struct App {
    ctx: egui::Context,
    link: Link,
    tabs: Vec<Tab>,
    active: usize,
    next_uid: u64,
    pending: HashMap<u64, Pending>,
    closed: Vec<String>,
    pointer: Pointer,
    omnibox_text: String,
    omnibox_editing: bool,
    focus_omnibox: bool,
    menu_open: bool,
    viewport_goal: Option<((u32, u32, u32), Instant)>,
    window_title: String,
    palette: Palette,
    dark: Option<bool>,
}

impl App {
    pub fn new(ctx: &egui::Context, initial_url: Option<String>) -> Self {
        look::install_fonts(ctx);
        let mut app = App {
            ctx: ctx.clone(),
            link: Link::Failed(String::new()),
            tabs: Vec::new(),
            active: 0,
            next_uid: 1,
            pending: HashMap::new(),
            closed: Vec::new(),
            pointer: Pointer::default(),
            omnibox_text: String::new(),
            omnibox_editing: false,
            focus_omnibox: initial_url.is_none(),
            menu_open: false,
            viewport_goal: None,
            window_title: String::new(),
            palette: Palette::new(true),
            dark: None,
        };
        app.tabs.push(Tab::new(0, initial_url));
        app.start_engine();
        app.refresh_omnibox();
        app
    }

    // ----- engine connection ------------------------------------------------

    fn start_engine(&mut self) {
        let (tx, rx) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Engine::launch());
            ctx.request_repaint();
        });
        self.link = Link::Starting(rx);
        self.pending.clear();
        for tab in &mut self.tabs {
            tab.target = None;
            tab.session = None;
            tab.streaming = false;
            tab.viewport = None;
            if tab.queued_url.is_none() && tab.url != omnibox::NEW_TAB_URL {
                tab.queued_url = Some(tab.url.clone());
            }
        }
    }

    fn cdp(&self) -> Option<&Cdp> {
        match &self.link {
            Link::Ready { cdp, .. } => Some(cdp),
            _ => None,
        }
    }

    fn send(&mut self, method: &str, params: Value, session: Option<&str>, pending: Option<Pending>) {
        let Some(cdp) = self.cdp() else { return };
        let id = cdp.send(method, params, session);
        if let Some(pending) = pending {
            self.pending.insert(id, pending);
        }
    }

    fn send_to_tab(&mut self, index: usize, method: &str, params: Value, pending: Option<Pending>) {
        let Some(session) = self.tabs.get(index).and_then(|t| t.session.clone()) else { return };
        self.send(method, params, Some(&session), pending);
    }

    fn evaluate(&mut self, index: usize, expression: String, pending: Pending) {
        self.send_to_tab(
            index,
            "Runtime.evaluate",
            json!({ "expression": expression, "returnByValue": true }),
            Some(pending),
        );
    }

    fn create_target(&mut self, index: usize) {
        let uid = self.tabs[index].uid;
        self.send("Target.createTarget", json!({ "url": "about:blank" }), None, Some(Pending::CreateTarget(uid)));
    }

    fn poll(&mut self) {
        if let Link::Starting(rx) = &self.link {
            match rx.try_recv() {
                Ok(Ok(engine)) => {
                    let cdp = Cdp::connect(engine.ws_url.clone(), self.ctx.clone());
                    self.link = Link::Ready { engine, cdp };
                    for index in 0..self.tabs.len() {
                        self.create_target(index);
                    }
                }
                Ok(Err(error)) => self.link = Link::Failed(format!("{error:#}")),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.link = Link::Failed("the engine launcher stopped unexpectedly".into())
                }
            }
        }
        let mut items = Vec::new();
        if let Some(cdp) = self.cdp() {
            while let Ok(item) = cdp.inbox.try_recv() {
                items.push(item);
            }
        }
        // Only the newest frame per tab is worth uploading.
        let mut frames: HashMap<String, Frame> = HashMap::new();
        for item in items {
            match item {
                Incoming::Frame { session, frame } => {
                    frames.insert(session, frame);
                }
                Incoming::Response { id, result } => self.on_response(id, result),
                Incoming::Event { method, params, session } => self.on_event(&method, &params, session.as_deref()),
                Incoming::Closed(reason) => {
                    self.link = Link::Failed(reason);
                    for tab in &mut self.tabs {
                        tab.loading = false;
                    }
                }
            }
        }
        for (session, frame) in frames {
            let Some(tab) = self.tabs.iter_mut().find(|t| t.session.as_deref() == Some(session.as_str())) else {
                continue;
            };
            tab.css_size = frame.css_size;
            match &mut tab.texture {
                Some(texture) => texture.set(frame.image, egui::TextureOptions::LINEAR),
                None => {
                    tab.texture = Some(self.ctx.load_texture(
                        format!("page-{}", tab.uid),
                        frame.image,
                        egui::TextureOptions::LINEAR,
                    ))
                }
            }
        }
    }

    fn tab_index(&self, uid: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid)
    }

    fn on_response(&mut self, id: u64, result: Result<Value, String>) {
        let Some(pending) = self.pending.remove(&id) else { return };
        match pending {
            Pending::CreateTarget(uid) => {
                let Some(index) = self.tab_index(uid) else {
                    // The tab was closed while its target was being created.
                    if let Some(target) = result.ok().and_then(|r| r["targetId"].as_str().map(str::to_string)) {
                        self.send("Target.closeTarget", json!({ "targetId": target }), None, None);
                    }
                    return;
                };
                let target = match result {
                    Ok(result) => result["targetId"].as_str().map(str::to_string),
                    Err(error) => {
                        self.tabs[index].error = Some(error);
                        None
                    }
                };
                let Some(target) = target else { return };
                let session = format!("{target}-session");
                self.tabs[index].target = Some(target);
                self.tabs[index].session = Some(session);
                self.send_to_tab(index, "Page.enable", json!({}), None);
                self.sync_tab_viewport(index);
                if let Some(url) = self.tabs[index].queued_url.take() {
                    self.navigate(index, url);
                }
            }
            Pending::Navigate(uid) => {
                let Some(index) = self.tab_index(uid) else { return };
                let error = match &result {
                    Ok(value) => value["errorText"].as_str().filter(|e| !e.is_empty()).map(str::to_string),
                    Err(error) => Some(error.clone()),
                };
                let tab = &mut self.tabs[index];
                tab.loading = false;
                tab.error = error;
                self.after_navigation(index);
            }
            Pending::History(uid) => {
                let Some(index) = self.tab_index(uid) else { return };
                let Ok(value) = result else { return };
                let tab = &mut self.tabs[index];
                tab.history = value["entries"]
                    .as_array()
                    .map(|entries| entries.iter().filter_map(|e| e["id"].as_i64()).collect())
                    .unwrap_or_default();
                tab.history_index = value["currentIndex"].as_u64().unwrap_or(0) as usize;
            }
            Pending::Title(uid) => {
                let Some(index) = self.tab_index(uid) else { return };
                if let Some(title) = result.ok().and_then(|v| v["result"]["value"].as_str().map(str::to_string)) {
                    self.tabs[index].title = title;
                }
            }
            Pending::Hover(uid) => {
                self.pointer.hover_sent = None;
                let Some(index) = self.tab_index(uid) else { return };
                let value = result.ok().map(|mut v| v["result"]["value"].take()).unwrap_or(Value::Null);
                let tab = &mut self.tabs[index];
                tab.hover_link = value["href"].as_str().unwrap_or("").to_string();
                tab.cursor = match value["cursor"].as_str().unwrap_or("") {
                    "pointer" => CursorIcon::PointingHand,
                    "text" => CursorIcon::Text,
                    "move" => CursorIcon::Move,
                    "grab" => CursorIcon::Grab,
                    "grabbing" => CursorIcon::Grabbing,
                    "not-allowed" => CursorIcon::NotAllowed,
                    "crosshair" => CursorIcon::Crosshair,
                    "help" => CursorIcon::Help,
                    "wait" | "progress" => CursorIcon::Progress,
                    _ if !tab.hover_link.is_empty() => CursorIcon::PointingHand,
                    _ if value["editable"].as_bool() == Some(true) => CursorIcon::Text,
                    _ => CursorIcon::Default,
                };
            }
            Pending::PointerMove => self.pointer.move_sent = None,
            Pending::Wheel => self.pointer.wheel_sent = None,
            Pending::CopySelection => {
                if let Some(text) = result.ok().and_then(|v| v["result"]["value"].as_str().map(str::to_string)) {
                    if !text.is_empty() {
                        self.ctx.copy_text(text);
                    }
                }
            }
        }
    }

    fn on_event(&mut self, method: &str, params: &Value, session: Option<&str>) {
        if method == "Target.targetInfoChanged" {
            let info = &params["targetInfo"];
            let target = info["targetId"].as_str();
            if let Some(index) = self.tabs.iter().position(|t| t.target.as_deref() == target) {
                if let Some(title) = info["title"].as_str().filter(|t| !t.is_empty()) {
                    self.tabs[index].title = title.to_string();
                }
                if let Some(url) = info["url"].as_str().filter(|u| !u.is_empty()) {
                    self.set_url(index, url.to_string());
                }
            }
            return;
        }
        if method == "Target.targetDestroyed" {
            let target = params["targetId"].as_str();
            if let Some(index) = self.tabs.iter().position(|t| t.target.as_deref() == target) {
                self.tabs[index].target = None;
                self.close_tab(index);
            }
            return;
        }
        let Some(index) = self.tabs.iter().position(|t| t.session.is_some() && t.session.as_deref() == session) else {
            return;
        };
        match method {
            "Page.frameNavigated" => {
                let frame = &params["frame"];
                if frame.get("parentId").is_none_or(Value::is_null) {
                    if let Some(url) = frame["url"].as_str() {
                        self.tabs[index].error = None;
                        self.tabs[index].title.clear();
                        self.set_url(index, url.to_string());
                    }
                }
            }
            "Page.navigatedWithinDocument" => {
                if let Some(url) = params["url"].as_str() {
                    self.set_url(index, url.to_string());
                }
                self.after_navigation(index);
            }
            "Page.lifecycleEvent" => {
                if params["name"] == "init" {
                    self.tabs[index].loading = true;
                }
            }
            "Page.loadEventFired" | "Page.frameStoppedLoading" => {
                self.tabs[index].loading = false;
                self.after_navigation(index);
            }
            _ => {}
        }
    }

    fn set_url(&mut self, index: usize, url: String) {
        self.tabs[index].url = url;
        if index == self.active && !self.omnibox_editing {
            self.refresh_omnibox();
        }
    }

    fn after_navigation(&mut self, index: usize) {
        let uid = self.tabs[index].uid;
        self.send_to_tab(index, "Page.getNavigationHistory", json!({}), Some(Pending::History(uid)));
        self.evaluate(index, "document.title".into(), Pending::Title(uid));
    }

    // ----- tab and navigation actions ---------------------------------------

    fn navigate(&mut self, index: usize, url: String) {
        let tab = &mut self.tabs[index];
        tab.error = None;
        tab.url = url.clone();
        tab.title.clear();
        tab.hover_link.clear();
        if tab.session.is_none() {
            tab.queued_url = Some(url);
            tab.loading = true;
            return;
        }
        tab.loading = true;
        let uid = tab.uid;
        self.send_to_tab(index, "Page.navigate", json!({ "url": url }), Some(Pending::Navigate(uid)));
        if index == self.active && !self.omnibox_editing {
            self.refresh_omnibox();
        }
    }

    fn go_history(&mut self, index: usize, delta: isize) {
        let tab = &self.tabs[index];
        let target = tab.history_index as isize + delta;
        let Some(entry) = usize::try_from(target).ok().and_then(|i| tab.history.get(i)).copied() else {
            return;
        };
        let uid = tab.uid;
        self.tabs[index].loading = true;
        self.tabs[index].error = None;
        self.send_to_tab(index, "Page.navigateToHistoryEntry", json!({ "entryId": entry }), Some(Pending::Navigate(uid)));
    }

    fn reload(&mut self, index: usize) {
        let tab = &mut self.tabs[index];
        if tab.is_new_tab_page() {
            return;
        }
        if tab.session.is_none() {
            return;
        }
        tab.loading = true;
        tab.error = None;
        let uid = tab.uid;
        self.send_to_tab(index, "Page.reload", json!({}), Some(Pending::Navigate(uid)));
    }

    fn stop(&mut self, index: usize) {
        self.tabs[index].loading = false;
        self.send_to_tab(index, "Page.stopLoading", json!({}), None);
    }

    fn new_tab(&mut self, url: Option<String>) {
        let uid = self.next_uid;
        self.next_uid += 1;
        let focus = url.is_none();
        self.tabs.push(Tab::new(uid, url));
        let index = self.tabs.len() - 1;
        if self.cdp().is_some() {
            self.create_target(index);
        }
        self.activate(index);
        self.focus_omnibox = focus;
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        if let Some(target) = tab.target {
            self.send("Target.closeTarget", json!({ "targetId": target }), None, None);
        }
        if tab.url != omnibox::NEW_TAB_URL {
            self.closed.push(tab.url);
        }
        if self.tabs.is_empty() {
            self.ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.omnibox_editing = false;
        self.refresh_omnibox();
    }

    fn activate(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.active = index;
            self.omnibox_editing = false;
            self.menu_open = false;
            self.refresh_omnibox();
        }
    }

    fn refresh_omnibox(&mut self) {
        if let Some(tab) = self.tabs.get(self.active) {
            self.omnibox_text = omnibox::display(&tab.url);
        }
    }

    /// Only the visible tab streams frames; background tabs keep their last
    /// frame as a texture and cost nothing to display again.
    fn sync_streams(&mut self) {
        for index in 0..self.tabs.len() {
            let want = index == self.active;
            let tab = &self.tabs[index];
            if tab.session.is_none() || tab.streaming == want {
                continue;
            }
            self.tabs[index].streaming = want;
            if want {
                self.sync_tab_viewport(index);
                self.send_to_tab(index, "Page.startScreencast", json!({ "format": "jpeg", "quality": 90 }), None);
            } else {
                self.send_to_tab(index, "Page.stopScreencast", json!({}), None);
            }
        }
    }

    fn sync_tab_viewport(&mut self, index: usize) {
        let Some((goal, _)) = self.viewport_goal else { return };
        if self.tabs[index].session.is_none() || self.tabs[index].viewport == Some(goal) {
            return;
        }
        self.tabs[index].viewport = Some(goal);
        let (width, height, scale) = goal;
        self.send_to_tab(
            index,
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": f64::from(scale) / 100.0,
                "mobile": false,
            }),
            None,
        );
    }

    fn update_viewport_goal(&mut self, content: Rect, pixels_per_point: f32) {
        let goal = (
            content.width().round().max(1.0) as u32,
            content.height().round().max(1.0) as u32,
            (pixels_per_point * 100.0).round() as u32,
        );
        let now = Instant::now();
        match self.viewport_goal {
            Some((current, _)) if current == goal => {}
            Some(_) => self.viewport_goal = Some((goal, now)),
            None => {
                // The first size is applied right away.
                self.viewport_goal = Some((goal, now - VIEWPORT_DEBOUNCE));
            }
        }
        let Some((_, changed)) = self.viewport_goal else { return };
        let settled = now.duration_since(changed) >= VIEWPORT_DEBOUNCE;
        if settled {
            if self.active < self.tabs.len() {
                self.sync_tab_viewport(self.active);
            }
        } else {
            self.ctx.request_repaint_after(VIEWPORT_DEBOUNCE);
        }
    }

    // ----- input ------------------------------------------------------------

    fn handle_shortcuts(&mut self) {
        let command = Modifiers::COMMAND;
        let pressed = |app: &App, modifiers: Modifiers, key: Key| {
            app.ctx.input_mut(|i| i.consume_key(modifiers, key))
        };
        if pressed(self, command | Modifiers::SHIFT, Key::T) {
            if let Some(url) = self.closed.pop() {
                self.new_tab(Some(url));
            }
        }
        if pressed(self, command, Key::T) {
            self.new_tab(None);
        }
        if pressed(self, command, Key::W) {
            self.close_tab(self.active);
        }
        if pressed(self, command, Key::L) || pressed(self, Modifiers::ALT, Key::D) || pressed(self, Modifiers::NONE, Key::F6) {
            self.focus_omnibox = true;
        }
        if pressed(self, command, Key::R) || pressed(self, Modifiers::NONE, Key::F5) {
            self.reload(self.active);
        }
        if pressed(self, Modifiers::ALT, Key::ArrowLeft) {
            self.go_history(self.active, -1);
        }
        if pressed(self, Modifiers::ALT, Key::ArrowRight) {
            self.go_history(self.active, 1);
        }
        if pressed(self, command | Modifiers::SHIFT, Key::Tab) {
            let count = self.tabs.len();
            self.activate((self.active + count - 1) % count);
        }
        if pressed(self, command, Key::Tab) {
            self.activate((self.active + 1) % self.tabs.len());
        }
        let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8];
        for (position, key) in digits.into_iter().enumerate() {
            if pressed(self, command, key) && position < self.tabs.len() {
                self.activate(position);
            }
        }
        if pressed(self, command, Key::Num9) {
            self.activate(self.tabs.len() - 1);
        }
    }

    /// Forward pointer and keyboard input that belongs to the page.
    fn forward_input(&mut self, content: Rect, page_focused: bool, blocked: bool) {
        let index = self.active;
        if self.tabs.get(index).and_then(|t| t.session.as_ref()).is_none() {
            return;
        }
        let to_css = |p: Pos2| p - content.min.to_vec2();
        let events = self.ctx.input(|i| i.events.clone());
        for event in events {
            match event {
                egui::Event::PointerMoved(pos) => {
                    if content.contains(pos) || self.pointer.buttons != 0 {
                        let css = to_css(pos);
                        self.pointer.pending_move = Some(css);
                        self.pointer.hover_due = Some(css);
                    } else if !self.tabs[index].hover_link.is_empty() {
                        self.tabs[index].hover_link.clear();
                    }
                }
                egui::Event::PointerGone => self.tabs[index].hover_link.clear(),
                egui::Event::PointerButton { pos, button, pressed, modifiers } => {
                    let bit = button_mask(button);
                    let inside = content.contains(pos) && !blocked && !self.menu_open;
                    if pressed && !inside {
                        continue;
                    }
                    if !pressed && self.pointer.buttons & bit == 0 {
                        continue;
                    }
                    match button {
                        PointerButton::Extra1 if pressed => self.go_history(index, -1),
                        PointerButton::Extra2 if pressed => self.go_history(index, 1),
                        PointerButton::Extra1 | PointerButton::Extra2 => {}
                        _ => self.send_mouse_button(to_css(pos), button, pressed, modifiers),
                    }
                    if pressed {
                        self.pointer.buttons |= bit;
                    } else {
                        self.pointer.buttons &= !bit;
                    }
                }
                egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                    let Some(pos) = self.ctx.input(|i| i.pointer.hover_pos()) else { continue };
                    if !content.contains(pos) || blocked {
                        continue;
                    }
                    let pixels = match unit {
                        egui::MouseWheelUnit::Point => delta,
                        // Chromium scrolls 100px per wheel notch on Windows.
                        egui::MouseWheelUnit::Line => delta * 100.0,
                        egui::MouseWheelUnit::Page => delta * content.height(),
                    };
                    let pixels = if modifiers.shift && pixels.x == 0.0 { vec2(pixels.y, 0.0) } else { pixels };
                    self.pointer.wheel -= pixels;
                    self.pointer.wheel_at = to_css(pos);
                }
                egui::Event::Key { key, pressed, modifiers, .. } if page_focused => {
                    if let Some(params) = keys::key_events(key, pressed, modifiers) {
                        self.send_to_tab(index, "Input.dispatchKeyEvent", params, None);
                    }
                    if pressed && matches!(key, Key::PageDown | Key::PageUp) {
                        let page = content.height() * 0.85;
                        self.pointer.wheel.y += if key == Key::PageDown { page } else { -page };
                        self.pointer.wheel_at = pos2(content.width() / 2.0, content.height() / 2.0);
                    }
                }
                egui::Event::Text(text) if page_focused => {
                    let modifiers = self.ctx.input(|i| i.modifiers);
                    for c in text.chars() {
                        self.send_to_tab(index, "Input.dispatchKeyEvent", keys::text_event(c, modifiers), None);
                        self.send_to_tab(
                            index,
                            "Input.dispatchKeyEvent",
                            json!({ "type": "keyUp", "key": c.to_string() }),
                            None,
                        );
                    }
                }
                egui::Event::Paste(text) if page_focused => {
                    self.send_to_tab(index, "Input.insertText", json!({ "text": text }), None);
                }
                egui::Event::Copy | egui::Event::Cut if page_focused => {
                    self.evaluate(index, SELECTION_JS.into(), Pending::CopySelection);
                }
                _ => {}
            }
        }
        self.flush_pointer(index);
    }

    fn send_mouse_button(&mut self, css: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) {
        // Deliver the move first so the press lands where the pointer is.
        if self.pointer.pending_move.take().is_some() {
            self.send_mouse_move(css);
        }
        if pressed {
            let now = Instant::now();
            self.pointer.press_count = match self.pointer.last_press {
                Some((at, at_pos, count))
                    if now.duration_since(at) < DOUBLE_CLICK && at_pos.distance(css) < 5.0 =>
                {
                    (count % 3) + 1
                }
                _ => 1,
            };
            self.pointer.last_press = Some((now, css, self.pointer.press_count));
        }
        let name = match button {
            PointerButton::Secondary => "right",
            PointerButton::Middle => "middle",
            _ => "left",
        };
        let buttons = if pressed { self.pointer.buttons | button_mask(button) } else { self.pointer.buttons & !button_mask(button) };
        self.send_to_tab(
            self.active,
            "Input.dispatchMouseEvent",
            json!({
                "type": if pressed { "mousePressed" } else { "mouseReleased" },
                "x": css.x,
                "y": css.y,
                "button": name,
                "buttons": buttons,
                "clickCount": self.pointer.press_count.max(1),
                "modifiers": keys::modifier_mask(modifiers),
            }),
            None,
        );
    }

    fn send_mouse_move(&mut self, css: Pos2) {
        self.pointer.move_sent = Some(Instant::now());
        let buttons = self.pointer.buttons;
        self.send_to_tab(
            self.active,
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": css.x,
                "y": css.y,
                "button": if buttons & 1 != 0 { "left" } else { "none" },
                "buttons": buttons,
            }),
            Some(Pending::PointerMove),
        );
    }

    /// Moves, wheel deltas and hover probes are coalesced: at most one of
    /// each is in flight, so a fast mouse never queues stale work.
    fn flush_pointer(&mut self, index: usize) {
        let now = Instant::now();
        let idle = |sent: Option<Instant>| sent.is_none_or(|at| now.duration_since(at) > IN_FLIGHT_TIMEOUT);
        if idle(self.pointer.move_sent) {
            if let Some(css) = self.pointer.pending_move.take() {
                self.send_mouse_move(css);
            }
        }
        if idle(self.pointer.wheel_sent) && self.pointer.wheel != Vec2::ZERO {
            let delta = std::mem::take(&mut self.pointer.wheel);
            let at = self.pointer.wheel_at;
            self.pointer.wheel_sent = Some(now);
            self.send_to_tab(
                index,
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseWheel", "x": at.x, "y": at.y, "deltaX": delta.x, "deltaY": delta.y }),
                Some(Pending::Wheel),
            );
        }
        let probe_ready = self
            .pointer
            .hover_sent
            .is_none_or(|at| now.duration_since(at) > HOVER_PROBE_INTERVAL.max(IN_FLIGHT_TIMEOUT));
        if probe_ready && self.pointer.move_sent.is_none() {
            if let Some(css) = self.pointer.hover_due.take() {
                self.pointer.hover_sent = Some(now);
                let uid = self.tabs[index].uid;
                self.evaluate(index, hover_js(css), Pending::Hover(uid));
            }
        }
        if self.pointer.pending_move.is_some() || self.pointer.wheel != Vec2::ZERO || self.pointer.hover_due.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(16));
        }
    }

    // ----- drawing ----------------------------------------------------------

    fn resize_edge(&self, window: Rect) -> Option<(ResizeDirection, CursorIcon)> {
        let maximized = self.ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let fullscreen = self.ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        if maximized || fullscreen {
            return None;
        }
        let pos = self.ctx.input(|i| i.pointer.hover_pos())?;
        let left = pos.x <= window.min.x + RESIZE_BORDER;
        let right = pos.x >= window.max.x - RESIZE_BORDER;
        let top = pos.y <= window.min.y + RESIZE_BORDER;
        let bottom = pos.y >= window.max.y - RESIZE_BORDER;
        Some(match (left, right, top, bottom) {
            (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest),
            (_, true, true, _) => (ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast),
            (true, _, _, true) => (ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest),
            (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast),
            (true, ..) => (ResizeDirection::West, CursorIcon::ResizeWest),
            (_, true, ..) => (ResizeDirection::East, CursorIcon::ResizeEast),
            (_, _, true, _) => (ResizeDirection::North, CursorIcon::ResizeNorth),
            (_, _, _, true) => (ResizeDirection::South, CursorIcon::ResizeSouth),
            _ => return None,
        })
    }

    fn icon_button(&self, ui: &mut egui::Ui, rect: Rect, id: &str, enabled: bool) -> (egui::Response, Color32) {
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let response = ui.interact(rect, Id::unique(id), sense);
        let p = &self.palette;
        if enabled && response.hovered() {
            let fill = if response.is_pointer_button_down_on() { p.separator } else { p.button_hover };
            ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
        }
        let color = if enabled { p.text } else { p.text_muted.gamma_multiply(0.45) };
        (response, color)
    }

    fn draw_tab_strip(&mut self, ui: &mut egui::Ui, strip: Rect) {
        let p = self.palette;
        let painter = ui.painter().clone();
        let time = ui.input(|i| i.time);

        // The empty strip drags the window, like a native title bar.
        let titlebar = ui.interact(strip, Id::unique("titlebar"), Sense::click_and_drag());
        if titlebar.drag_started_by(PointerButton::Primary) {
            self.ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if titlebar.double_clicked() {
            let maximized = self.ctx.input(|i| i.viewport().maximized.unwrap_or(false));
            self.ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }

        let controls_width = CAPTION_BUTTON_WIDTH * 3.0;
        let left = strip.min.x + 10.0;
        let available = (strip.width() - 10.0 - controls_width - 36.0 - 48.0).max(80.0);
        let tab_width = (available / self.tabs.len() as f32).clamp(56.0, 232.0);
        let top = strip.min.y + 7.0;
        let bottom = strip.max.y;

        let mut activate = None;
        let mut close = None;
        for (index, tab) in self.tabs.iter().enumerate() {
            let rect = Rect::from_min_max(pos2(left + tab_width * index as f32, top), pos2(left + tab_width * (index + 1) as f32, bottom));
            let response = ui.interact(rect, Id::unique(("tab", tab.uid)), Sense::click());
            let active = index == self.active;
            let hovered = response.hovered();
            if active {
                let radius = CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 };
                painter.rect_filled(rect, radius, p.toolbar);
                // Concave feet join the tab smoothly into the toolbar.
                for (corner, outside) in [(rect.left_bottom(), -1.0), (rect.right_bottom(), 1.0)] {
                    let foot = Rect::from_two_pos(corner, corner + vec2(8.0 * outside, -8.0));
                    painter.rect_filled(foot, 0.0, p.toolbar);
                    painter.circle_filled(corner + vec2(8.0 * outside, -8.0), 8.0, p.frame);
                }
            } else if hovered {
                painter.rect_filled(rect.shrink2(vec2(2.0, 0.0)).with_max_y(bottom - 5.0), CornerRadius::same(8), p.tab_hover);
            } else if index + 1 != self.active && index + 1 < self.tabs.len() {
                let x = rect.max.x;
                painter.line_segment([pos2(x, top + 9.0), pos2(x, bottom - 12.0)], Stroke::new(1.0, p.separator));
            }

            let mid_y = (top + bottom - 5.0) / 2.0 + if active { 2.0 } else { 0.0 };
            let icon = pos2(rect.min.x + 19.0, mid_y);
            let compact = tab_width < 80.0;
            if tab.loading {
                look::spinner(&painter, icon, 6.5, p.accent, time);
            } else if tab.is_new_tab_page() {
                look::search(&painter, icon + vec2(1.0, 1.0), p.text_muted);
            } else {
                look::monogram(&painter, icon, &tab.label());
            }
            let show_close = !compact || active;
            let close_rect = Rect::from_center_size(pos2(rect.max.x - 18.0, mid_y), vec2(20.0, 20.0));
            let text_right = if show_close { close_rect.min.x - 4.0 } else { rect.max.x - 8.0 };
            let text_rect = Rect::from_min_max(pos2(rect.min.x + 33.0, top), pos2(text_right, bottom));
            if !compact {
                let color = if active { p.text } else { p.text_muted };
                painter.with_clip_rect(text_rect).text(
                    pos2(text_rect.min.x, mid_y),
                    Align2::LEFT_CENTER,
                    tab.label(),
                    FontId::proportional(12.5),
                    color,
                );
            }
            if show_close && (hovered || active || ui.rect_contains_pointer(close_rect)) {
                let close_response = ui.interact(close_rect, Id::unique(("close", tab.uid)), Sense::click());
                if close_response.hovered() {
                    painter.circle_filled(close_rect.center(), 9.0, p.button_hover);
                }
                look::cross(&painter, close_rect.center(), 4.0, if close_response.hovered() { p.text } else { p.text_muted });
                if close_response.clicked() {
                    close = Some(index);
                }
            }
            if response.clicked() && close.is_none() {
                activate = Some(index);
            }
            if response.middle_clicked() {
                close = Some(index);
            }
        }

        let plus_rect = Rect::from_center_size(pos2(left + tab_width * self.tabs.len() as f32 + 20.0, (top + bottom - 5.0) / 2.0), vec2(28.0, 28.0));
        let (plus, color) = self.icon_button(ui, plus_rect, "new-tab", true);
        look::plus(&painter, plus_rect.center(), 6.0, color);
        if plus.clicked() {
            self.new_tab(None);
        }

        self.draw_caption_buttons(ui, strip);

        if let Some(index) = close {
            self.close_tab(index);
        } else if let Some(index) = activate {
            self.activate(index);
        }
    }

    fn draw_caption_buttons(&mut self, ui: &mut egui::Ui, strip: Rect) {
        let p = self.palette;
        let painter = ui.painter().clone();
        let maximized = self.ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let buttons = ["minimize", "maximize", "close"];
        for (slot, name) in buttons.iter().enumerate() {
            let x = strip.max.x - CAPTION_BUTTON_WIDTH * (3 - slot) as f32;
            let rect = Rect::from_min_size(pos2(x, strip.min.y), vec2(CAPTION_BUTTON_WIDTH, 32.0));
            let response = ui.interact(rect, Id::unique(("caption", *name)), Sense::click());
            let closing = *name == "close";
            let mut color = p.text;
            if response.hovered() {
                let fill = if closing { p.danger } else { p.button_hover };
                painter.rect_filled(rect, 0.0, fill);
                if closing {
                    color = Color32::WHITE;
                }
            }
            match *name {
                "minimize" => look::minimize(&painter, rect.center(), color),
                "maximize" => look::maximize(&painter, rect.center(), color, maximized),
                _ => look::cross(&painter, rect.center(), 5.0, color),
            }
            if response.clicked() {
                let command = match *name {
                    "minimize" => ViewportCommand::Minimized(true),
                    "maximize" => ViewportCommand::Maximized(!maximized),
                    _ => ViewportCommand::Close,
                };
                self.ctx.send_viewport_cmd(command);
            }
        }
    }

    fn draw_toolbar(&mut self, ui: &mut egui::Ui, bar: Rect) {
        let p = self.palette;
        let painter = ui.painter().clone();
        painter.rect_filled(bar, 0.0, p.toolbar);
        painter.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, p.separator));
        let index = self.active;
        let mid = bar.center().y;
        let button = |slot: f32| Rect::from_center_size(pos2(bar.min.x + 24.0 + slot * 36.0, mid), vec2(32.0, 32.0));

        let (back, color) = self.icon_button(ui, button(0.0), "back", self.tabs[index].can_go_back());
        look::arrow(&painter, back.rect.center(), color, true);
        if back.clicked() {
            self.go_history(index, -1);
        }
        let (forward, color) = self.icon_button(ui, button(1.0), "forward", self.tabs[index].can_go_forward());
        look::arrow(&painter, forward.rect.center(), color, false);
        if forward.clicked() {
            self.go_history(index, 1);
        }
        let loading = self.tabs[index].loading;
        let (reload, color) = self.icon_button(ui, button(2.0), "reload", !self.tabs[index].is_new_tab_page() || loading);
        if loading {
            look::cross(&painter, reload.rect.center(), 5.0, color);
        } else {
            look::reload(&painter, reload.rect.center(), color);
        }
        if reload.clicked() {
            if loading {
                self.stop(index);
            } else {
                self.reload(index);
            }
        }

        let menu_rect = Rect::from_center_size(pos2(bar.max.x - 24.0, mid), vec2(32.0, 32.0));
        let (menu, color) = self.icon_button(ui, menu_rect, "menu", true);
        look::dots(&painter, menu_rect.center(), color);
        if menu.clicked() {
            self.menu_open = !self.menu_open;
        }

        let field = Rect::from_min_max(pos2(button(2.0).max.x + 8.0, mid - 16.0), pos2(menu_rect.min.x - 8.0, mid + 16.0));
        self.draw_omnibox(ui, field);

        if loading {
            let width = bar.width();
            let t = (ui.input(|i| i.time) * 0.8).fract() as f32;
            let start = bar.min.x + width * (t * 1.4 - 0.4);
            let line = Rect::from_min_max(pos2(start.max(bar.min.x), bar.max.y - 2.0), pos2((start + width * 0.4).min(bar.max.x), bar.max.y));
            painter.rect_filled(line, 1.0, p.accent);
        }
    }

    fn draw_omnibox(&mut self, ui: &mut egui::Ui, field: Rect) {
        let p = self.palette;
        let id = Id::unique("omnibox");
        let focused = ui.memory(|m| m.has_focus(id));
        let hovered = ui.rect_contains_pointer(field);
        let painter = ui.painter().clone();
        let radius = CornerRadius::same(16);
        if focused {
            painter.rect_filled(field, radius, p.toolbar);
            painter.rect_stroke(field, radius, Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        } else {
            painter.rect_filled(field, radius, if hovered { p.field_hover } else { p.field });
        }
        let tab = &self.tabs[self.active];
        let icon = pos2(field.min.x + 18.0, field.center().y);
        if focused || tab.is_new_tab_page() {
            look::search(&painter, icon, p.text_muted);
        } else if omnibox::is_secure(&tab.url) {
            look::lock(&painter, icon, p.text_muted);
        } else {
            look::info(&painter, icon, p.text_muted);
        }

        let text_rect = Rect::from_min_max(pos2(field.min.x + 36.0, field.min.y), pos2(field.max.x - 14.0, field.max.y));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        let output = egui::TextEdit::singleline(&mut self.omnibox_text)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .hint_text("Search or enter address")
            .font(FontId::proportional(14.0))
            .desired_width(text_rect.width())
            .show(&mut child);
        let response = output.response;

        let mut select_all = false;
        if self.focus_omnibox {
            self.focus_omnibox = false;
            response.request_focus();
            select_all = true;
        }
        if response.gained_focus() || (select_all && !self.omnibox_editing) {
            self.omnibox_editing = true;
            let url = &self.tabs[self.active].url;
            if url != omnibox::NEW_TAB_URL {
                self.omnibox_text = url.clone();
            }
            select_all = true;
        }
        if select_all {
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                let end = self.omnibox_text.chars().count();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(end),
                )));
                state.store(ui.ctx(), id);
            }
        }
        if response.lost_focus() {
            let submitted = ui.input(|i| i.key_pressed(Key::Enter));
            self.omnibox_editing = false;
            if submitted {
                if let Some(url) = omnibox::resolve(&self.omnibox_text) {
                    self.navigate(self.active, url);
                    // Hand keyboard focus to the page, as browsers do.
                    ui.memory_mut(|m| m.request_focus(Id::unique("page")));
                }
            }
            self.refresh_omnibox();
        }
    }

    fn draw_content(&mut self, ui: &mut egui::Ui, content: Rect, blocked: bool) {
        let p = self.palette;
        let painter = ui.painter().with_clip_rect(content);
        let page_id = Id::unique("page");
        let response = ui.interact(content, page_id, Sense::click_and_drag());
        if !blocked && !self.menu_open && response.is_pointer_button_down_on() && !response.has_focus() {
            response.request_focus();
        }
        if response.has_focus() {
            // Keep Tab, arrows and Escape for the page instead of egui focus moves.
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    page_id,
                    egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true },
                )
            });
        }
        let page_focused = response.has_focus();

        match &self.link {
            Link::Starting(_) => {
                painter.rect_filled(content, 0.0, p.page);
                look::spinner(&painter, content.center() - vec2(0.0, 18.0), 10.0, p.accent, ui.input(|i| i.time));
                painter.text(content.center() + vec2(0.0, 12.0), Align2::CENTER_CENTER, "Starting the engine", FontId::proportional(14.0), p.text_muted);
                ui.ctx().request_repaint_after(Duration::from_millis(16));
                return;
            }
            Link::Failed(reason) => {
                let reason = reason.clone();
                painter.rect_filled(content, 0.0, p.page);
                let center = content.center();
                painter.text(center - vec2(0.0, 40.0), Align2::CENTER_CENTER, "The engine stopped", FontId::proportional(20.0), p.text);
                painter.text(center - vec2(0.0, 12.0), Align2::CENTER_CENTER, reason, FontId::proportional(13.0), p.text_muted);
                let button = Rect::from_center_size(center + vec2(0.0, 28.0), vec2(120.0, 34.0));
                let restart = ui.interact(button, Id::unique("restart"), Sense::click());
                painter.rect_filled(button, CornerRadius::same(17), if restart.hovered() { p.accent } else { p.accent.gamma_multiply(0.85) });
                painter.text(button.center(), Align2::CENTER_CENTER, "Restart", FontId::proportional(14.0), Color32::WHITE);
                if restart.clicked() {
                    self.start_engine();
                }
                return;
            }
            Link::Ready { .. } => {}
        }

        let tab = &self.tabs[self.active];
        if let Some(error) = &tab.error {
            painter.rect_filled(content, 0.0, p.page);
            let center = content.center() - vec2(0.0, 40.0);
            painter.text(center, Align2::CENTER_CENTER, "This page could not be loaded", FontId::proportional(22.0), p.text);
            painter.text(center + vec2(0.0, 30.0), Align2::CENTER_CENTER, omnibox::display(&tab.url), FontId::proportional(14.0), p.text_muted);
            painter.text(center + vec2(0.0, 54.0), Align2::CENTER_CENTER, error, FontId::proportional(12.5), p.text_muted);
        } else if tab.is_new_tab_page() {
            painter.rect_filled(content, 0.0, p.page);
            let center = content.center() - vec2(0.0, content.height() * 0.12);
            painter.text(center, Align2::CENTER_CENTER, "Obscura", FontId::proportional(44.0), p.text);
            painter.text(center + vec2(0.0, 40.0), Align2::CENTER_CENTER, "Type a search or an address above", FontId::proportional(14.0), p.text_muted);
        } else {
            painter.rect_filled(content, 0.0, Color32::WHITE);
            if let Some(texture) = &tab.texture {
                let size = if tab.css_size.x > 0.0 { tab.css_size } else { content.size() };
                let rect = Rect::from_min_size(content.min, size);
                painter.image(texture.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            if response.hovered() && !blocked {
                ui.ctx().set_cursor_icon(tab.cursor);
            }
            if !tab.hover_link.is_empty() {
                let galley = painter.layout_no_wrap(tab.hover_link.clone(), FontId::proportional(12.0), p.text);
                let width = (galley.size().x + 16.0).min(content.width() * 0.6);
                let bubble = Rect::from_min_size(pos2(content.min.x, content.max.y - 24.0), vec2(width, 24.0));
                painter.rect_filled(bubble, CornerRadius { nw: 0, ne: 8, sw: 0, se: 0 }, p.toolbar);
                painter.rect_stroke(bubble, CornerRadius { nw: 0, ne: 8, sw: 0, se: 0 }, Stroke::new(1.0, p.separator), egui::StrokeKind::Inside);
                painter.with_clip_rect(bubble.shrink(4.0)).galley(pos2(bubble.min.x + 8.0, bubble.center().y - galley.size().y / 2.0), galley, p.text);
            }
        }

        if !self.tabs[self.active].is_new_tab_page() {
            self.forward_input(content, page_focused, blocked);
        }
    }

    fn draw_menu(&mut self, ctx: &egui::Context, anchor: Pos2) {
        if !self.menu_open {
            return;
        }
        let p = self.palette;
        let mut choice = None;
        let area = egui::Area::new(Id::unique("menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .pivot(Align2::RIGHT_TOP)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(p.toolbar)
                    .stroke(Stroke::new(1.0, p.separator))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(6))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(70) })
                    .show(ui, |ui| {
                        let items = [
                            ("New tab", "Ctrl+T"),
                            ("Reopen closed tab", "Ctrl+Shift+T"),
                            ("Reload", "F5"),
                            ("Copy page address", ""),
                            ("Close tab", "Ctrl+W"),
                            ("Quit", ""),
                        ];
                        for (position, (label, shortcut)) in items.iter().enumerate() {
                            let (rect, response) = ui.allocate_exact_size(vec2(236.0, 30.0), Sense::click());
                            if response.hovered() {
                                ui.painter().rect_filled(rect, CornerRadius::same(6), p.button_hover);
                            }
                            ui.painter().text(pos2(rect.min.x + 10.0, rect.center().y), Align2::LEFT_CENTER, *label, FontId::proportional(13.5), p.text);
                            ui.painter().text(pos2(rect.max.x - 10.0, rect.center().y), Align2::RIGHT_CENTER, *shortcut, FontId::proportional(12.0), p.text_muted);
                            if response.clicked() {
                                choice = Some(position);
                            }
                        }
                    });
            });
        let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
            && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|pos| !area.response.rect.contains(pos))
            && !ctx.input(|i| i.pointer.interact_pos()).is_some_and(|pos| Rect::from_center_size(anchor + vec2(-16.0, -20.0), vec2(34.0, 34.0)).contains(pos));
        if clicked_outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.menu_open = false;
        }
        let Some(choice) = choice else { return };
        self.menu_open = false;
        match choice {
            0 => self.new_tab(None),
            1 => {
                if let Some(url) = self.closed.pop() {
                    self.new_tab(Some(url));
                }
            }
            2 => self.reload(self.active),
            3 => ctx.copy_text(self.tabs[self.active].url.clone()),
            4 => self.close_tab(self.active),
            _ => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }

    fn sync_theme(&mut self) {
        let dark = self.ctx.input(|i| i.raw.system_theme).is_none_or(|t| t == egui::Theme::Dark);
        if self.dark != Some(dark) {
            self.dark = Some(dark);
            self.palette = Palette::new(dark);
            look::apply_style(&self.ctx, &self.palette);
        }
    }

    fn sync_window_title(&mut self) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let title = format!("{} - Obscura", tab.label());
        if title != self.window_title {
            self.ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    pub fn shutdown(&mut self) {
        if let Link::Ready { engine, cdp } = std::mem::replace(&mut self.link, Link::Failed(String::new())) {
            cdp.close();
            engine.shutdown_after_disconnect();
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Runs even while minimized, so frames never pile up undecoded.
        self.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.sync_theme();
        if self.tabs.is_empty() {
            return;
        }
        self.handle_shortcuts();
        if self.tabs.is_empty() {
            return;
        }

        let window = ui.max_rect();
        let p = self.palette;
        ui.painter().rect_filled(window, 0.0, p.frame);
        let strip = Rect::from_min_size(window.min, vec2(window.width(), STRIP_HEIGHT));
        let toolbar = Rect::from_min_max(pos2(window.min.x, strip.max.y), pos2(window.max.x, strip.max.y + TOOLBAR_HEIGHT));
        let content = Rect::from_min_max(pos2(window.min.x, toolbar.max.y), window.max);

        let edge = self.resize_edge(window);
        if let Some((direction, cursor)) = edge {
            ui.ctx().set_cursor_icon(cursor);
            if ui.input(|i| i.pointer.primary_pressed()) {
                ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(direction));
            }
        }

        self.draw_tab_strip(ui, strip);
        if self.tabs.is_empty() {
            return;
        }
        self.draw_toolbar(ui, toolbar);
        self.update_viewport_goal(content, ui.ctx().pixels_per_point());
        self.sync_streams();
        self.draw_content(ui, content, edge.is_some());
        self.draw_menu(&ui.ctx().clone(), pos2(toolbar.max.x - 8.0, toolbar.max.y - 2.0));
        self.sync_window_title();

        let maximized = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        if !maximized {
            ui.painter().rect_stroke(window, 0.0, Stroke::new(1.0, p.separator), egui::StrokeKind::Inside);
        }
        if self.tabs.iter().any(|t| t.loading) {
            ui.ctx().request_repaint_after(Duration::from_millis(16));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.shutdown();
    }
}

fn button_mask(button: PointerButton) -> u64 {
    match button {
        PointerButton::Primary => 1,
        PointerButton::Secondary => 2,
        PointerButton::Middle => 4,
        PointerButton::Extra1 => 8,
        PointerButton::Extra2 => 16,
    }
}

/// Reports the link under the pointer, the CSS cursor, and whether the
/// element takes text, so the window can show a link preview and cursor.
fn hover_js(css: Pos2) -> String {
    format!(
        "(function(){{var e=document.elementFromPoint({x},{y});if(!e)return null;\
         var a=e.closest&&e.closest('a[href]');var c='';\
         try{{c=getComputedStyle(e).cursor||''}}catch(_){{}}\
         var t=e.closest&&e.closest('input,textarea,[contenteditable]');\
         return {{href:a?String(a.href):'',cursor:c==='auto'?'':c,editable:!!t}};}})()",
        x = css.x,
        y = css.y,
    )
}

const SELECTION_JS: &str = "(function(){var e=document.activeElement;\
    if(e&&(e.localName==='input'||e.localName==='textarea')&&e.selectionStart!=null)\
    return (e.value||'').slice(e.selectionStart,e.selectionEnd);\
    var s=window.getSelection&&window.getSelection();return s?String(s):'';})()";
