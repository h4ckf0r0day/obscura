//! Client WebSocket connections for page script.
//!
//! A connection runs on its own task. The page talks to it through two
//! channels: [`WsCommand`] out, [`WsEvent`] in. Dropping the command sender
//! closes the socket, so a page that goes away never leaks a connection.
//!
//! The rustls/tungstenite path here honours the context proxy (HTTP CONNECT
//! or SOCKS5) and the private-network gate. Stealth builds route through the
//! wreq client instead so the handshake carries the Chrome TLS fingerprint
//! (see `StealthHttpClient::connect_websocket`).

use std::sync::{Arc, OnceLock};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, WebSocketConfig};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

/// Largest message a page may receive. Chromium has no hard cap, but a
/// headless scraper should not buffer unbounded frames.
const MAX_MESSAGE_BYTES: usize = 64 << 20;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug)]
pub enum WsCommand {
    Text(String),
    Binary(Vec<u8>),
    Close { code: u16, reason: String },
}

#[derive(Debug)]
pub enum WsEvent {
    Text(String),
    Binary(Vec<u8>),
    /// The connection is finished. `clean` is false when it dropped without a
    /// closing handshake (code 1006).
    Close { code: u16, reason: String, clean: bool },
}

pub struct WsHandle {
    /// Subprotocol the server selected, or "".
    pub protocol: String,
    /// Negotiated extensions, or "".
    pub extensions: String,
    /// `Set-Cookie` values from the handshake response.
    pub set_cookies: Vec<String>,
    pub commands: mpsc::UnboundedSender<WsCommand>,
    pub events: mpsc::UnboundedReceiver<WsEvent>,
}

pub struct WsConnectOptions {
    /// `ws:` or `wss:` URL.
    pub url: Url,
    pub protocols: Vec<String>,
    /// Serialized origin of the document opening the socket.
    pub origin: String,
    pub user_agent: String,
    /// `Cookie` header value for the handshake, or "".
    pub cookie_header: String,
    pub proxy: Option<String>,
    pub allow_private_network: bool,
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The http(s) URL a ws(s) URL maps to, for cookie scoping and the SSRF gate.
pub fn http_equivalent(url: &Url) -> Option<Url> {
    let mut http = url.clone();
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    http.set_scheme(scheme).ok()?;
    Some(http)
}

fn forbidden(ip: std::net::IpAddr, allow_private_network: bool) -> bool {
    !(allow_private_network || crate::env_allows_private_network()) && crate::is_forbidden_ip(ip)
}

async fn connect_direct(host: &str, port: u16, allow_private_network: bool) -> Result<TcpStream, String> {
    let addrs: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS lookup for {host} failed: {e}"))?
        .collect();
    let allowed: Vec<_> = addrs
        .iter()
        .filter(|a| !forbidden(a.ip(), allow_private_network))
        .collect();
    if allowed.is_empty() {
        return Err(if addrs.is_empty() {
            format!("{host} did not resolve")
        } else {
            format!("Access to private/internal address for {host} is not allowed")
        });
    }
    let mut last = None;
    for addr in allowed {
        match TcpStream::connect(addr).await {
            Ok(stream) => return Ok(stream),
            Err(e) => last = Some(e),
        }
    }
    Err(format!("connect to {host}:{port} failed: {}", last.map(|e| e.to_string()).unwrap_or_default()))
}

async fn connect_http_proxy(proxy: &Url, host: &str, port: u16) -> Result<TcpStream, String> {
    let proxy_host = proxy.host_str().ok_or("proxy URL has no host")?;
    let proxy_port = proxy.port_or_known_default().unwrap_or(8080);
    let mut stream = TcpStream::connect((proxy_host, proxy_port))
        .await
        .map_err(|e| format!("connect to proxy {proxy_host}:{proxy_port} failed: {e}"))?;
    let authority = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if !proxy.username().is_empty() {
        let user = percent_decode(proxy.username());
        let pass = percent_decode(proxy.password().unwrap_or(""));
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).await.map_err(|e| e.to_string())?;
    // Read the proxy's reply byte by byte so nothing past the header block is
    // consumed from what becomes the tunnelled stream.
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err("proxy CONNECT response too large".into());
        }
        let n = stream.read(&mut byte).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("proxy closed the connection during CONNECT".into());
        }
        head.push(byte[0]);
    }
    let status_line = String::from_utf8_lossy(&head);
    let status = status_line.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        return Err(format!("proxy CONNECT refused: {}", status_line.lines().next().unwrap_or("")));
    }
    Ok(stream)
}

async fn connect_socks_proxy(proxy: &Url, host: &str, port: u16) -> Result<TcpStream, String> {
    let proxy_host = proxy.host_str().ok_or("proxy URL has no host")?;
    let proxy_addr = (proxy_host, proxy.port().unwrap_or(1080));
    let stream = if proxy.username().is_empty() {
        tokio_socks::tcp::Socks5Stream::connect(proxy_addr, (host, port)).await
    } else {
        let user = percent_decode(proxy.username());
        let pass = percent_decode(proxy.password().unwrap_or(""));
        tokio_socks::tcp::Socks5Stream::connect_with_password(proxy_addr, (host, port), &user, &pass).await
    }
    .map_err(|e| format!("SOCKS5 proxy connect failed: {e}"))?;
    Ok(stream.into_inner())
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls::pki_types::pem::PemObject;
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            // Same extra trust anchors the HTTP client honours.
            for path in crate::client::configured_root_paths() {
                match rustls::pki_types::CertificateDer::pem_file_iter(&path) {
                    Ok(certs) => {
                        for cert in certs.flatten() {
                            let _ = roots.add(cert);
                        }
                    }
                    Err(_) => {
                        if let Ok(der) = std::fs::read(&path) {
                            let _ = roots.add(rustls::pki_types::CertificateDer::from(der));
                        }
                    }
                }
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("ring supports the default TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

/// Open a WebSocket through the rustls/tungstenite transport.
pub async fn connect(opts: WsConnectOptions) -> Result<WsHandle, String> {
    tokio::time::timeout(CONNECT_TIMEOUT, connect_inner(opts))
        .await
        .map_err(|_| "WebSocket connection timed out".to_string())?
}

async fn connect_inner(opts: WsConnectOptions) -> Result<WsHandle, String> {
    let url = &opts.url;
    let secure = url.scheme() == "wss";
    let host = url.host_str().ok_or("WebSocket URL has no host")?.trim_matches(|c| c == '[' || c == ']').to_string();
    let port = url.port_or_known_default().unwrap_or(if secure { 443 } else { 80 });

    let tcp = match opts.proxy.as_deref().map(Url::parse) {
        Some(Ok(proxy)) => match proxy.scheme() {
            "http" => connect_http_proxy(&proxy, &host, port).await?,
            "socks5" | "socks5h" => connect_socks_proxy(&proxy, &host, port).await?,
            other => return Err(format!("unsupported proxy scheme for WebSocket: {other}")),
        },
        Some(Err(e)) => return Err(format!("invalid proxy URL: {e}")),
        None => connect_direct(&host, port, opts.allow_private_network).await?,
    };
    let _ = tcp.set_nodelay(true);

    let stream: Box<dyn Io> = if secure {
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| format!("invalid TLS server name {host}: {e}"))?;
        let tls = tokio_rustls::TlsConnector::from(tls_config())
            .connect(server_name, tcp)
            .await
            .map_err(|e| format!("TLS handshake with {host} failed: {e}"))?;
        Box::new(tls)
    } else {
        Box::new(tcp)
    };

    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("invalid WebSocket request: {e}"))?;
    let headers = request.headers_mut();
    let mut put = |name: &'static str, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            headers.insert(name, v);
        }
    };
    put("Pragma", "no-cache");
    put("Cache-Control", "no-cache");
    if !opts.user_agent.is_empty() {
        put("User-Agent", &opts.user_agent);
    }
    if !opts.origin.is_empty() && opts.origin != "null" {
        put("Origin", &opts.origin);
    }
    if !opts.protocols.is_empty() {
        put("Sec-WebSocket-Protocol", &opts.protocols.join(", "));
    }
    if !opts.cookie_header.is_empty() {
        put("Cookie", &opts.cookie_header);
    }

    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE_BYTES);
    config.max_frame_size = Some(MAX_MESSAGE_BYTES);
    let (ws, response) = tokio_tungstenite::client_async_with_config(request, stream, Some(config))
        .await
        .map_err(|e| format!("WebSocket handshake failed: {e}"))?;

    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let protocol = header("sec-websocket-protocol");
    if !protocol.is_empty() && !opts.protocols.iter().any(|p| p == &protocol) {
        return Err(format!("server selected unrequested subprotocol '{protocol}'"));
    }
    let extensions = header("sec-websocket-extensions");
    let set_cookies = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect();

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<WsCommand>();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel::<WsEvent>();
    tokio::spawn(async move {
        let mut ws = ws;
        let mut closing = false;
        loop {
            tokio::select! {
                cmd = cmd_rx.recv(), if !closing => {
                    let sent = match cmd {
                        Some(WsCommand::Text(t)) => ws.send(Message::text(t)).await,
                        Some(WsCommand::Binary(b)) => ws.send(Message::binary(b)).await,
                        Some(WsCommand::Close { code, reason }) => {
                            closing = true;
                            ws.close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.into() })).await
                        }
                        // The page went away: close without waiting for a reply.
                        None => {
                            let _ = ws.close(None).await;
                            return;
                        }
                    };
                    if let Err(e) = sent {
                        tracing::debug!("WebSocket send failed: {e}");
                    }
                }
                msg = ws.next() => {
                    let event = match msg {
                        Some(Ok(Message::Text(t))) => WsEvent::Text(t.to_string()),
                        Some(Ok(Message::Binary(b))) => WsEvent::Binary(b.to_vec()),
                        Some(Ok(Message::Close(frame))) => {
                            let (code, reason) = frame
                                .map(|f| (u16::from(f.code), f.reason.to_string()))
                                .unwrap_or((1005, String::new()));
                            // tungstenite answers the close frame; drain until
                            // the stream ends so the reply is flushed.
                            while let Some(Ok(_)) = ws.next().await {}
                            let _ = ev_tx.send(WsEvent::Close { code, reason, clean: true });
                            return;
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            tracing::debug!("WebSocket receive failed: {e}");
                            let _ = ev_tx.send(WsEvent::Close { code: 1006, reason: String::new(), clean: false });
                            return;
                        }
                        None => {
                            let _ = ev_tx.send(WsEvent::Close { code: 1006, reason: String::new(), clean: false });
                            return;
                        }
                    };
                    if ev_tx.send(event).is_err() {
                        let _ = ws.close(None).await;
                        return;
                    }
                }
            }
        }
    });

    Ok(WsHandle { protocol, extensions, set_cookies, commands: cmd_tx, events: ev_rx })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_ws_schemes_to_http() {
        let u = Url::parse("wss://a.test/x?y=1").unwrap();
        assert_eq!(http_equivalent(&u).unwrap().as_str(), "https://a.test/x?y=1");
        let u = Url::parse("ws://a.test:8080/").unwrap();
        assert_eq!(http_equivalent(&u).unwrap().as_str(), "http://a.test:8080/");
        assert!(http_equivalent(&Url::parse("https://a.test/").unwrap()).is_none());
    }

    #[test]
    fn decodes_percent_escaped_proxy_credentials() {
        assert_eq!(percent_decode("us%40er"), "us@er");
        assert_eq!(percent_decode("p%3Ass"), "p:ss");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("trail%4"), "trail%4");
    }

    async fn echo_server() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let callback = |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                                    mut resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        if let Some(p) = req.headers().get("sec-websocket-protocol") {
                            let first = p.to_str().unwrap().split(',').next().unwrap().trim().to_string();
                            resp.headers_mut().insert("sec-websocket-protocol", first.parse().unwrap());
                        }
                        let cookie = req.headers().get("cookie").map(|c| c.to_str().unwrap().to_string()).unwrap_or_default();
                        resp.headers_mut().insert("x-seen-cookie", cookie.parse().unwrap());
                        Ok(resp)
                    };
                    let mut ws = tokio_tungstenite::accept_hdr_async(stream, callback).await.unwrap();
                    while let Some(Ok(msg)) = ws.next().await {
                        match msg {
                            Message::Text(t) if t.as_str() == "bye" => {
                                let _ = ws.close(Some(CloseFrame { code: CloseCode::from(4001), reason: "done".into() })).await;
                            }
                            Message::Text(_) | Message::Binary(_) => {
                                let _ = ws.send(msg).await;
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn echoes_text_and_binary_and_reports_server_close() {
        let addr = echo_server().await;
        let mut handle = connect(WsConnectOptions {
            url: Url::parse(&format!("ws://{addr}/socket")).unwrap(),
            protocols: vec!["chat".into(), "v2".into()],
            origin: "http://127.0.0.1".into(),
            user_agent: "test".into(),
            cookie_header: String::new(),
            proxy: None,
            allow_private_network: true,
        })
        .await
        .unwrap();
        assert_eq!(handle.protocol, "chat");

        handle.commands.send(WsCommand::Text("hi".into())).unwrap();
        handle.commands.send(WsCommand::Binary(vec![1, 2, 3])).unwrap();
        assert!(matches!(handle.events.recv().await, Some(WsEvent::Text(t)) if t == "hi"));
        assert!(matches!(handle.events.recv().await, Some(WsEvent::Binary(b)) if b == vec![1, 2, 3]));

        handle.commands.send(WsCommand::Text("bye".into())).unwrap();
        match handle.events.recv().await {
            Some(WsEvent::Close { code, reason, clean }) => {
                assert_eq!((code, reason.as_str(), clean), (4001, "done", true));
            }
            other => panic!("expected close, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn private_address_is_refused_without_opt_in() {
        if crate::env_allows_private_network() {
            return;
        }
        let addr = echo_server().await;
        let err = connect(WsConnectOptions {
            url: Url::parse(&format!("ws://localhost:{}/", addr.port())).unwrap(),
            protocols: vec![],
            origin: String::new(),
            user_agent: String::new(),
            cookie_header: String::new(),
            proxy: None,
            allow_private_network: false,
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains("not allowed"), "{err}");
    }
}
