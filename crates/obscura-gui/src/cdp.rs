//! A small Chrome DevTools Protocol client on its own thread.
//!
//! Commands go out through an unbounded channel. Responses and events come
//! back on a std channel, and every arrival wakes the egui loop. Screencast
//! frames are decoded and acknowledged here so JPEG decoding never stalls the
//! UI thread and the engine can stream the next frame right away.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use base64::Engine as _;
use eframe::egui;
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{json, Value};
use tokio::sync::mpsc as async_mpsc;
use tokio_tungstenite::tungstenite::Message;

pub enum Incoming {
    Response { id: u64, result: Result<Value, String> },
    Event { method: String, params: Value, session: Option<String> },
    Frame { session: String, frame: Frame },
    Closed(String),
}

pub struct Frame {
    pub image: egui::ColorImage,
    /// Viewport size in CSS pixels, which may differ from the image size.
    pub css_size: egui::Vec2,
}

pub struct Cdp {
    out: Option<async_mpsc::UnboundedSender<String>>,
    next_id: Arc<AtomicU64>,
    pub inbox: mpsc::Receiver<Incoming>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Cdp {
    pub fn connect(url: String, repaint: egui::Context) -> Self {
        let (out_tx, out_rx) = async_mpsc::unbounded_channel::<String>();
        let (in_tx, in_rx) = mpsc::channel();
        let next_id = Arc::new(AtomicU64::new(1));
        let ids = next_id.clone();
        let thread = std::thread::Builder::new()
            .name("obscura-gui-cdp".into())
            .spawn(move || {
                let reason = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(url, out_rx, &in_tx, &ids, &repaint)),
                    Err(error) => format!("could not start the CDP runtime: {error}"),
                };
                let _ = in_tx.send(Incoming::Closed(reason));
                repaint.request_repaint();
            })
            .expect("spawn CDP thread");
        Cdp {
            out: Some(out_tx),
            next_id,
            inbox: in_rx,
            thread: Some(thread),
        }
    }

    /// Send a command; returns its id so the caller can match the response.
    pub fn send(&self, method: &str, params: Value, session: Option<&str>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = Value::String(session.to_string());
        }
        if let Some(out) = &self.out {
            let _ = out.send(message.to_string());
        }
        id
    }

    /// Close the socket cleanly so the server persists this session's
    /// cookies and storage, then wait for the connection thread.
    pub fn close(mut self) {
        self.out.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn run(
    url: String,
    mut out_rx: async_mpsc::UnboundedReceiver<String>,
    in_tx: &mpsc::Sender<Incoming>,
    ids: &AtomicU64,
    repaint: &egui::Context,
) -> String {
    let socket = match tokio_tungstenite::connect_async(url.as_str()).await {
        Ok((socket, _)) => socket,
        Err(error) => return format!("could not connect to the engine: {error}"),
    };
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            outgoing = out_rx.recv() => match outgoing {
                Some(text) => {
                    if let Err(error) = sink.send(Message::Text(text.into())).await {
                        return format!("engine connection lost: {error}");
                    }
                }
                None => {
                    // The app is closing: hang up and wait for the server's close.
                    let _ = sink.close().await;
                    let _ = tokio::time::timeout(Duration::from_secs(2), async {
                        while let Some(Ok(_)) = stream.next().await {}
                    })
                    .await;
                    return "closed".into();
                }
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let Ok(message) = serde_json::from_str::<Value>(&text) else { continue };
                    if let Some(item) = route(message, &mut sink, ids).await {
                        if in_tx.send(item).is_err() {
                            return "closed".into();
                        }
                        repaint.request_repaint();
                    }
                }
                Some(Ok(Message::Close(_))) | None => return "the engine closed the connection".into(),
                Some(Err(error)) => return format!("engine connection lost: {error}"),
                Some(Ok(_)) => {}
            },
        }
    }
}

async fn route<S>(mut message: Value, sink: &mut S, ids: &AtomicU64) -> Option<Incoming>
where
    S: futures_util::Sink<Message> + Unpin,
{
    if let Some(id) = message.get("id").and_then(Value::as_u64) {
        let result = match message.get_mut("error") {
            Some(error) => Err(error["message"].as_str().unwrap_or("CDP error").to_string()),
            None => Ok(message.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
        };
        return Some(Incoming::Response { id, result });
    }
    let method = message.get("method")?.as_str()?.to_string();
    let session = message
        .get("sessionId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut params = message.get_mut("params").map(Value::take).unwrap_or(Value::Null);

    if method == "Page.screencastFrame" {
        let session = session?;
        if let Some(stream_id) = params.get("sessionId").and_then(Value::as_i64) {
            let ack = json!({
                "id": ids.fetch_add(1, Ordering::Relaxed),
                "method": "Page.screencastFrameAck",
                "params": { "sessionId": stream_id },
                "sessionId": session,
            });
            let _ = sink.send(Message::Text(ack.to_string().into())).await;
        }
        let data = params.get_mut("data").map(Value::take)?;
        let frame = decode_frame(data.as_str()?, &params["metadata"])?;
        return Some(Incoming::Frame { session, frame });
    }
    Some(Incoming::Event { method, params, session })
}

fn decode_frame(data: &str, metadata: &Value) -> Option<Frame> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).ok()?;
    let rgba = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = rgba.dimensions();
    let image = egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        rgba.as_raw(),
    );
    let css = |key: &str, fallback: u32| {
        metadata
            .get(key)
            .and_then(Value::as_f64)
            .filter(|value| *value > 0.0)
            .unwrap_or(f64::from(fallback)) as f32
    };
    Some(Frame {
        image,
        css_size: egui::vec2(css("deviceWidth", width), css("deviceHeight", height)),
    })
}
