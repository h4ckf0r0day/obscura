use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub async fn serve() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = requests.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let recorded = recorded.clone();
            tokio::spawn(async move {
                let mut bytes = [0; 4096];
                let count = socket.read(&mut bytes).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&bytes[..count]);
                let path = request.split_ascii_whitespace().nth(1).unwrap_or("/").to_string();
                recorded.lock().unwrap().push(path.clone());
                if path == "/noheaders" {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    return;
                }
                if path == "/redirect" || path == "/redirect-file" {
                    let target = if path == "/redirect" { "/slow" } else { "file:///not-allowed" };
                    let _ = socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
                    return;
                }
                let prefix = br#"<!doctype html><title>partial</title><main id=arrived>received</main><script>globalThis.prefixRan=true;</script><iframe srcdoc="<script>parent.frameRan=true;</script>"></iframe><script>globalThis.eofRan=true;"#;
                if path == "/healthy" {
                    let body = "<!doctype html><title>healthy</title><script>globalThis.healthyRan=true;</script>";
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                    return;
                }
                if path == "/oversize" || path == "/protocol" {
                    let size = if path == "/oversize" { 64 * 1024 * 1024 + 1 } else { prefix.len() + 100 };
                    let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n").as_bytes()).await;
                    let _ = socket.write_all(prefix).await;
                    return;
                }
                let content_type = if path == "/json" { "application/json" } else { "text/html" };
                let _ = socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").as_bytes()).await;
                if path != "/nobytes" {
                    let body = if path == "/late" { b"<!doctype html><title>late</title><main id=arrived>received</main>".as_slice() }
                        else if path == "/json" { b"{\"partial\":".as_slice() } else { prefix.as_slice() };
                    let _ = socket.write_all(format!("{:x}\r\n", body.len()).as_bytes()).await;
                    let _ = socket.write_all(body).await;
                    let _ = socket.write_all(b"\r\n").await;
                }
                tokio::time::sleep(if path == "/late" { Duration::from_millis(1700) } else { Duration::from_secs(5) }).await;
                if path == "/late" {
                    let tail = b"<p id=tail>finished</p>";
                    let _ = socket.write_all(format!("{:x}\r\n", tail.len()).as_bytes()).await;
                    let _ = socket.write_all(tail).await;
                    let _ = socket.write_all(b"\r\n0\r\n\r\n").await;
                }
            });
        }
    });
    (origin, requests)
}
