use std::io::{BufReader, BufWriter, Write};
use std::num::NonZeroU32;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use anyhow::{ensure, Result};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use super::events::InputState;
use super::protocol::{opaque_pixel, Frame, Packet, MAX_PIXELS};

enum Update { Frame(Frame), Closed }

fn surface_error(error: softbuffer::SoftBufferError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

struct App {
    window: Option<Arc<Window>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    output: mpsc::SyncSender<Packet>,
    frame: Option<Frame>,
    input: InputState,
    is_waiting: bool,
    is_occluded: bool,
    next_capture: Instant,
    error: Option<String>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: impl std::fmt::Display) {
        self.error = Some(error.to_string());
        event_loop.exit();
    }

    fn send(&mut self, event_loop: &ActiveEventLoop, packet: Packet) -> bool {
        if let Err(error) = self.output.try_send(packet) {
            self.fail(event_loop, format!("native input is unavailable or busy: {error}"));
            return false;
        }
        true
    }

    fn draw(&mut self) -> Result<()> {
        let Some(surface) = self.surface.as_mut() else { return Ok(()) };
        let size = surface.window().inner_size();
        if size.width == 0 || size.height == 0 { return Ok(()); }
        ensure!(u64::from(size.width) * u64::from(size.height) <= MAX_PIXELS as u64, "native window exceeds the pixel limit");
        surface.resize(NonZeroU32::new(size.width).unwrap(), NonZeroU32::new(size.height).unwrap()).map_err(surface_error)?;
        let mut buffer = surface.buffer_mut().map_err(surface_error)?;
        let frame = self.frame.as_ref().filter(|frame| !frame.pixels.is_empty());
        for (index, pixel) in buffer.iter_mut().enumerate() {
            *pixel = match frame {
                Some(frame) => {
                    let x = (index as u32 % size.width) as u64 * u64::from(frame.header.width) / u64::from(size.width);
                    let y = (index as u32 / size.width) as u64 * u64::from(frame.header.height) / u64::from(size.height);
                    let offset = (y as usize * frame.header.width as usize + x as usize) * 4;
                    opaque_pixel(&frame.pixels[offset..offset + 4])
                }
                None => 0xf5f3ee,
            };
        }
        buffer.present().map_err(surface_error)?;
        Ok(())
    }
}

impl ApplicationHandler<Update> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() { return; }
        let attributes = Window::default_attributes()
            .with_title(format!("Obscura [{}]", std::process::id()))
            .with_inner_size(winit::dpi::LogicalSize::new(1280, 720))
            .with_min_inner_size(winit::dpi::LogicalSize::new(320, 240));
        let initialized = (|| -> Result<_> {
            let window = Arc::new(event_loop.create_window(attributes)?);
            window.set_ime_allowed(true);
            let context = softbuffer::Context::new(window.clone()).map_err(surface_error)?;
            let surface = softbuffer::Surface::new(&context, window.clone()).map_err(surface_error)?;
            Ok((window, context, surface))
        })();
        match initialized {
            Ok((window, context, surface)) => {
                self.window = Some(window);
                self.context = Some(context);
                self.surface = Some(surface);
                self.send(event_loop, Packet::Ready);
            }
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, update: Update) {
        match update {
            Update::Closed => event_loop.exit(),
            Update::Frame(frame) => {
                self.is_waiting = false;
                let should_redraw = self.frame.as_ref().is_none_or(|previous| {
                    previous.header.width != frame.header.width || previous.header.height != frame.header.height
                        || previous.pixels != frame.pixels
                });
                let has_title_changed = self.frame.as_ref().is_none_or(|previous| {
                    previous.header.title != frame.header.title || previous.header.error != frame.header.error
                });
                if let Some(window) = self.window.as_ref() {
                    if has_title_changed {
                        let title = frame.header.error.as_deref().unwrap_or(&frame.header.title);
                        window.set_title(&format!("Obscura [{}] · {title}", std::process::id()));
                    }
                    if should_redraw { window.request_redraw(); }
                }
                self.frame = Some(frame);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(error) = self.draw() { self.fail(event_loop, error); }
            }
            WindowEvent::Occluded(is_occluded) => self.is_occluded = is_occluded,
            event => {
                let Some(window) = self.window.as_ref() else { return };
                let image = self.frame.as_ref().map_or((0, 0), |frame| (frame.header.width, frame.header.height));
                if let Some(packet) = self.input.event(&event, window, image) { self.send(event_loop, packet); }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else { return };
        let size = window.inner_size();
        if self.is_occluded || size.width == 0 || size.height == 0 {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        if Instant::now() >= self.next_capture {
            self.next_capture = Instant::now() + Duration::from_millis(66);
            if !self.is_waiting {
                let image = self.frame.as_ref().map_or((0, 0), |frame| (frame.header.width, frame.header.height));
                for packet in self.input.motion(&window, image) {
                    if !self.send(event_loop, packet) { return; }
                }
                let size = size.to_logical::<u32>(window.scale_factor());
                self.is_waiting = self.send(event_loop, Packet::Frame { width: size.width, height: size.height });
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_capture));
    }
}

pub fn run_window() -> Result<()> {
    let mut builder = EventLoop::<Update>::with_user_event();
    #[cfg(target_os = "linux")]
    if std::env::var("WINIT_UNIX_BACKEND").as_deref() == Ok("x11") {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let event_loop = builder.build()?;
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(std::io::stdin());
        loop {
            match Frame::read(&mut reader) {
                Ok(Some(frame)) => { if proxy.send_event(Update::Frame(frame)).is_err() { return; } }
                Ok(None) => break,
                Err(error) => { eprintln!("Native frame failed: {error}"); break; }
            }
        }
        let _ = proxy.send_event(Update::Closed);
    });
    let (output, packets) = mpsc::sync_channel(128);
    std::thread::spawn(move || {
        let mut writer = BufWriter::new(std::io::stdout());
        for packet in packets {
            if serde_json::to_writer(&mut writer, &packet).is_err()
                || writer.write_all(b"\n").is_err() || writer.flush().is_err() { break; }
        }
    });
    let mut app = App {
        window: None, context: None, surface: None, output, frame: None,
        input: InputState::default(), is_waiting: false, is_occluded: false,
        next_capture: Instant::now(), error: None,
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error { anyhow::bail!(error); }
    Ok(())
}
