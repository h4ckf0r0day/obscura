use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

use super::FrameRequest;

const MAX_HEADERS: usize = 8 * 1024;

async fn request(stream: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 1024];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0 && bytes.len() + count <= MAX_HEADERS, "invalid request headers");
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(String::from_utf8(bytes)?);
        }
    }
}

fn allowed(headers: &str, authority: &str) -> bool {
    let mut hosts = Vec::new();
    let mut origins = Vec::new();
    for line in headers.split("\r\n").skip(1).take_while(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else { return false };
        if name.eq_ignore_ascii_case("host") { hosts.push(value.trim()); }
        if name.eq_ignore_ascii_case("origin") { origins.push(value.trim()); }
    }
    hosts == [authority]
        && (origins.is_empty() || origins == [format!("http://{authority}").as_str()])
}

pub(super) async fn serve(
    mut stream: TcpStream,
    authority: &str,
    route: &str,
    sender: mpsc::Sender<FrameRequest>,
) -> Result<()> {
    let headers = match tokio::time::timeout(std::time::Duration::from_secs(5), request(&mut stream)).await {
        Ok(Ok(headers)) => headers,
        _ => return respond(&mut stream, 400, "text/plain", b"Invalid request").await,
    };
    if !allowed(&headers, authority) {
        return respond(&mut stream, 403, "text/plain", b"Forbidden").await;
    }
    let mut parts = headers.lines().next().unwrap_or_default().split_whitespace();
    if parts.next() != Some("GET") {
        return respond(&mut stream, 405, "text/plain", b"Read-only viewer").await;
    }
    let path = parts.next().unwrap_or_default();
    if path == route {
        return respond(&mut stream, 200, "text/html; charset=utf-8", include_bytes!("page.html")).await;
    }
    if path != format!("{route}frame") {
        return respond(&mut stream, 404, "text/plain", b"Not found").await;
    }
    let (reply, response) = oneshot::channel();
    if sender.try_send(reply).is_err() {
        return respond(&mut stream, 503, "text/plain", b"Viewer busy").await;
    }
    match tokio::time::timeout(std::time::Duration::from_secs(10), response).await {
        Ok(Ok(Ok(frame))) => respond(&mut stream, 200, "application/json", &serde_json::to_vec(&frame)?).await,
        _ => respond(&mut stream, 503, "text/plain", b"Frame unavailable").await,
    }
}

async fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK", 400 => "Bad Request", 403 => "Forbidden", 404 => "Not Found",
        405 => "Method Not Allowed", _ => "Service Unavailable",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'\r\n\
         Connection: close\r\n\r\n",
        body.len(),
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn viewer_rejects_foreign_origins_and_rebound_hosts() {
        let authority = "127.0.0.1:9876";
        assert!(allowed("GET / HTTP/1.1\r\nHost: 127.0.0.1:9876\r\n\r\n", authority));
        for header in ["Host: evil.example", "Host: 127.0.0.1:9876\r\nOrigin: https://evil.example", "Host: 127.0.0.1:9876\r\nOrigin: null", "Host: 127.0.0.1:9876\r\nHost: 127.0.0.1:9876"] {
            assert!(!allowed(&format!("GET / HTTP/1.1\r\n{header}\r\n\r\n"), authority));
        }
    }
}
