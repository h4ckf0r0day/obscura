use std::path::Path;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use obscura_browser::BrowserContext;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn call(ws: &mut Ws, method: &str, params: Value, session: Option<&str>) -> Value {
    let mut command = json!({"id":1, "method":method, "params":params});
    if let Some(session) = session { command["sessionId"] = json!(session); }
    ws.send(Message::Text(command.to_string().into())).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next()).await
            .unwrap_or_else(|_| panic!("timeout waiting for {method}"))
            .expect("connection closed").unwrap();
        let Message::Text(text) = frame else { continue };
        let reply: Value = serde_json::from_str(&text).unwrap();
        if reply["id"].as_u64() != Some(1) { continue; }
        assert!(reply.get("error").is_none(), "{method}: {reply}");
        return reply["result"].clone();
    }
}

async fn evaluate(ws: &mut Ws, session: &str, expression: &str) -> Value {
    let result = call(ws, "Runtime.evaluate", json!({
        "expression":expression, "returnByValue":true, "awaitPromise":true,
    }), Some(session)).await;
    assert!(result.get("exceptionDetails").is_none(), "{result}");
    result["result"]["value"].clone()
}

async fn page(ws: &mut Ws, url: &str) -> String {
    let created = call(ws, "Target.createTarget", json!({"url":"about:blank"}), None).await;
    let session = format!("{}-session", created["targetId"].as_str().unwrap());
    call(ws, "Page.navigate", json!({"url":url, "waitUntil":"load"}), Some(&session)).await;
    session
}

async fn wait_for_profile(dir: &Path, origin: &str, expected: &[(String, String)]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let loaded = BrowserContext::with_storage("reload".into(), Some(dir.to_owned()));
        let entries = loaded.local_storage.snapshot(origin);
        if entries == expected { return; }
        assert!(tokio::time::Instant::now() < deadline, "profile: {entries:?}, expected: {expected:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_cookie(dir: &Path, name: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match std::fs::read(dir.join("cookies.json")) {
            Ok(bytes) => {
                let cookies: Value = serde_json::from_slice(&bytes).unwrap();
                if cookies.as_array().unwrap().iter().any(|cookie| cookie["name"] == name) { return; }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("reading cookie persistence acknowledgement: {error}"),
        }
        assert!(tokio::time::Instant::now() < deadline, "connection did not persist cookie {name}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn connection_storage_isolation(a_closes_first: bool) {
    let fixture = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", fixture.local_addr().unwrap());
    let url = format!("{origin}/");
    let fixture_task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = fixture.accept().await.unwrap();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                socket.read(&mut request).await.unwrap();
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n<p>storage</p>")
                    .await.unwrap();
            });
        }
    });
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("obscura-connection-storage-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let seed = BrowserContext::with_storage("seed".into(), Some(root.clone()));
    assert!(seed.local_storage.set(&origin, "sid".into(), "old".into()));
    assert!(seed.local_storage.set(&origin, "gone".into(), "x".into()));
    seed.save_local_storage();
    let seed_file = std::fs::read_dir(root.join("localStorage")).unwrap().next().unwrap().unwrap().path();
    let seed_bytes = std::fs::read(&seed_file).unwrap();
    let ready = root.join("ready.json");
    tokio::task::LocalSet::new().run_until(async {
        let storage_dir = root.clone();
        let ready_file = ready.clone();
        tokio::task::spawn_local(async move {
            obscura_cdp::server::start_with_serve_options_limit_and_ready_file(
                0, "127.0.0.1", None, false, None, false, Some(storage_dir), true, 8,
                Some(&ready_file)).await.unwrap();
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !ready.exists() {
            assert!(tokio::time::Instant::now() < deadline, "server did not start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let address: Value = serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
        let endpoint = format!("ws://127.0.0.1:{}/devtools/browser", address["port"].as_u64().unwrap());
        let (mut a, _) = connect_async(&endpoint).await.unwrap();
        let sa = page(&mut a, &url).await;
        evaluate(&mut a, &sa,
            "(() => {localStorage.setItem('sid','newer');localStorage.removeItem('gone');localStorage.setItem('a','A');return true})()").await;
        assert_eq!(evaluate(&mut a, &sa,
            "(async () => {await new Promise((resolve,reject) => {const r=indexedDB.open('private-a',1);r.onupgradeneeded=()=>r.result.createObjectStore('items');r.onsuccess=()=>{r.result.close();resolve()};r.onerror=()=>reject(r.error)});return (await indexedDB.databases()).map(d=>d.name)})()").await,
            json!(["private-a"]));
        let (mut b, _) = connect_async(&endpoint).await.unwrap();
        let sb = page(&mut b, &url).await;
        assert_eq!(evaluate(&mut b, &sb,
            "[localStorage.getItem('sid'),localStorage.getItem('gone'),localStorage.getItem('a')]").await,
            json!(["old", "x", null]));
        assert_eq!(evaluate(&mut b, &sb, "indexedDB.databases().then(d=>d.map(x=>x.name))").await, json!([]));
        evaluate(&mut b, &sb,
            "(() => {localStorage.setItem('first','1');localStorage.setItem('second','2');return true})()").await;
        assert_eq!(evaluate(&mut a, &sa, "localStorage.getItem('first')").await, Value::Null);
        for (ws, session) in [(&mut a, &sa), (&mut b, &sb)] {
            call(ws, "Page.navigate", json!({"url":url, "waitUntil":"load"}), Some(session)).await;
            assert_eq!(std::fs::read(&seed_file).unwrap(), seed_bytes, "navigation persisted a private snapshot");
        }
        let mut first = vec![("sid".into(), if a_closes_first { "newer" } else { "old" }.into())];
        if a_closes_first {
            evaluate(&mut a, &sa, "document.cookie='closeFirst=done'").await;
            a.close(None).await.unwrap();
            first.push(("a".into(), "A".into()));
        } else {
            evaluate(&mut b, &sb, "document.cookie='closeFirst=done'").await;
            b.close(None).await.unwrap();
            first.extend([("gone".into(), "x".into()), ("first".into(), "1".into()), ("second".into(), "2".into())]);
        }
        wait_for_cookie(&root, "closeFirst").await;
        wait_for_profile(&root, &origin, &first).await;
        let (mut c, _) = connect_async(&endpoint).await.unwrap();
        let sc = page(&mut c, &url).await;
        assert_eq!(evaluate(&mut c, &sc,
            "[localStorage.getItem('sid'),localStorage.getItem('gone'),localStorage.getItem('a'),localStorage.getItem('first')]").await,
            json!(["old", "x", null, null]), "new clients must use the startup snapshot");
        let expected = if a_closes_first {
            evaluate(&mut b, &sb, "document.cookie='closeSecond=done'").await;
            b.close(None).await.unwrap();
            vec![("sid".into(), "newer".into()), ("a".into(), "A".into()),
                ("first".into(), "1".into()), ("second".into(), "2".into())]
        } else {
            evaluate(&mut a, &sa, "document.cookie='closeSecond=done'").await;
            a.close(None).await.unwrap();
            vec![("sid".into(), "newer".into()), ("first".into(), "1".into()),
                ("second".into(), "2".into()), ("a".into(), "A".into())]
        };
        wait_for_cookie(&root, "closeSecond").await;
        wait_for_profile(&root, &origin, &expected).await;
        evaluate(&mut c, &sc, "document.cookie='closeLast=done'").await;
        c.close(None).await.unwrap();
        // The final atomic cookie write follows Web Storage persistence, unlike
        // the WebSocket close handshake, which precedes connection teardown.
        wait_for_cookie(&root, "closeLast").await;
        wait_for_profile(&root, &origin, &expected).await;
    }).await;
    fixture_task.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn private_storage_merges_changes_when_a_closes_first() {
    connection_storage_isolation(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn private_storage_merges_changes_when_b_closes_first() {
    connection_storage_isolation(false).await;
}
