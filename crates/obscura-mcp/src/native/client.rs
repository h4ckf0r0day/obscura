use anyhow::{ensure, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use super::protocol::{FrameHeader, Packet, MAX_HEADER};
use crate::BrowserState;

pub(crate) struct NativeClient {
    child: Child,
    frames: ChildStdin,
    packets: mpsc::Receiver<Packet>,
}

async fn read_packet(reader: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> Result<Option<Packet>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            ensure!(bytes.is_empty(), "partial native input packet");
            return Ok(None);
        }
        let length = available.iter().position(|byte| *byte == b'\n').map_or(available.len(), |index| index + 1);
        ensure!(bytes.len() + length <= MAX_HEADER, "native input packet exceeds the limit");
        bytes.extend_from_slice(&available[..length]);
        reader.consume(length);
        if bytes.last() == Some(&b'\n') { return Ok(Some(serde_json::from_slice(&bytes)?)); }
    }
}

impl NativeClient {
    pub async fn start() -> Result<Self> {
        ensure!(std::env::var_os("OBSCURA_NATIVE_CHILD").is_none(), "the native window helper cannot start another MCP session");
        let mut child = Command::new(std::env::current_exe()?)
            .arg("__native_window").env("OBSCURA_NATIVE_CHILD", "1")
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit()).kill_on_drop(true).spawn()?;
        let frames = child.stdin.take().context("native window stdin is unavailable")?;
        let mut reader = BufReader::new(child.stdout.take().context("native window stdout is unavailable")?);
        let ready = tokio::time::timeout(std::time::Duration::from_secs(10), read_packet(&mut reader)).await??;
        ensure!(matches!(ready, Some(Packet::Ready)), "native window could not start; check the desktop display connection");
        let (sender, packets) = mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                match read_packet(&mut reader).await {
                    Ok(Some(packet)) => { if sender.send(packet).await.is_err() { break; } }
                    Ok(_) => break,
                    Err(error) => { eprintln!("Native window input failed: {error}"); break; }
                }
            }
        });
        eprintln!("Obscura native window: process {}", child.id().unwrap_or(0));
        Ok(Self { child, frames, packets })
    }

    pub fn process_id(&self) -> Option<u32> { self.child.id() }
    pub async fn receive(&mut self) -> Option<Packet> { self.packets.recv().await }

    async fn send_frame(&mut self, state: &mut BrowserState, width: u32, height: u32) -> Result<()> {
        let capture = capture(state, width, height);
        let (header, pixels) = match capture {
            Ok(frame) => frame,
            Err(error) => (FrameHeader { width: 0, height: 0, title: "Capture unavailable".into(), error: Some(error.to_string()) }, Vec::new()),
        };
        let mut encoded = serde_json::to_vec(&header)?;
        encoded.push(b'\n');
        self.frames.write_all(&encoded).await?;
        self.frames.write_all(&pixels).await?;
        self.frames.flush().await?;
        Ok(())
    }

    pub async fn apply(&mut self, packet: Packet, state: &mut BrowserState) -> Result<()> {
        match packet {
            Packet::Frame { width, height } => self.send_frame(state, width, height).await?,
            Packet::Input { method, params } => {
                ensure!(["dispatchMouseEvent", "dispatchKeyEvent", "insertText"].contains(&method.as_str()), "unsupported native input method");
                if state.active_tab.is_none() { return Ok(()); }
                obscura_cdp::domains::input::handle(&method, &params, state.page_mut(), &None).await
                    .map_err(anyhow::Error::msg)?;
                state.settle_synthetic_navigation().await.map_err(anyhow::Error::msg)?;
            }
            Packet::Ready => anyhow::bail!("unexpected native ready packet"),
        }
        Ok(())
    }
}

fn capture(state: &mut BrowserState, width: u32, height: u32) -> Result<(FrameHeader, Vec<u8>)> {
    let size = FrameHeader { width, height, title: String::new(), error: None };
    size.byte_len()?;
    ensure!(width > 0 && height > 0, "native viewport must be positive");
    if state.active_tab.is_none() {
        return Ok((FrameHeader { width: 0, height: 0, title: "Waiting for the agent".into(), error: None }, Vec::new()));
    }
    let page = state.page_mut();
    let viewport = (width as f32, height as f32);
    if page.viewport != viewport { page.set_viewport(viewport); }
    let (left, top) = page.screenshot_scroll_offset();
    let region = obscura_browser::CaptureRegion::new(left, top, viewport.0, viewport.1, 1.0);
    let image = page.screenshot_region_raster_with_animation_sample(region, page.live_animation_sample())
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let title = page.url_string().chars().take(512).collect();
    Ok((FrameHeader { width: image.width(), height: image.height(), title, error: None }, image.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn native_capture_preserves_empty_sessions_and_follows_scroll_in_rgba() {
        let mut state = BrowserState::new(None, None, false);
        assert!(capture(&mut state, 40, 40).unwrap().1.is_empty());
        assert!(state.tabs.is_empty());
        state.page_mut().navigate("data:text/html,<body style='margin:0'><div style='height:100px;background:red'></div><div style='height:300px;background:blue'></div>").await.unwrap();
        let (_, initial) = capture(&mut state, 40, 40).unwrap();
        assert_eq!(&initial[..4], &[255, 0, 0, 255]);
        state.page_mut().evaluate("window.scrollTo(0, 150)");
        let (_, scrolled) = capture(&mut state, 40, 40).unwrap();
        assert_eq!(&scrolled[..4], &[0, 0, 255, 255]);
    }
}
