//! `serve --workers N --worker-max-*` replaces workers that pass a threshold
//! without failing any session: replacements keep the pool at N, and a replaced
//! worker keeps serving the connections it already had until they close.
#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct Server(Child, u16);

impl Server {
    fn start(extra: &[&str]) -> Server {
        let port = TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let child = Command::new(env!("CARGO_BIN_EXE_obscura"))
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string(), "--workers", "2"])
            .args(extra)
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Server(child, port);
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.workers().len() != 2 || std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "balancer did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        server
    }

    fn workers(&self) -> Vec<u32> {
        let mut children = Vec::new();
        for task in std::fs::read_dir(format!("/proc/{}/task", self.0.id())).unwrap() {
            if let Ok(ids) = std::fs::read_to_string(task.unwrap().path().join("children")) {
                children.extend(ids.split_whitespace().map(|id| id.parse::<u32>().unwrap()));
            }
        }
        children.sort_unstable();
        children.dedup();
        children
    }

    /// Wait until exactly `count` workers exist and none of `gone` is alive.
    fn settle(&self, count: usize, gone: &[u32], within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            let now = self.workers();
            if now.len() == count && gone.iter().all(|pid| !now.contains(pid)) {
                return;
            }
            assert!(Instant::now() < deadline, "workers {now:?}, expected {count} without {gone:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.0.wait();
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Session {
    ws: Ws,
    id: u64,
    sid: String,
}

impl Session {
    async fn call(&mut self, method: &str, params: Value, with_session: bool) -> Value {
        self.id += 1;
        let mut msg = json!({"id": self.id, "method": method, "params": params});
        if with_session {
            msg["sessionId"] = json!(self.sid);
        }
        self.ws.send(Message::Text(msg.to_string().into())).await.expect("send");
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(20), self.ws.next())
                .await
                .expect("reply timeout")
                .expect("ws closed")
                .expect("ws error");
            if let Message::Text(text) = frame {
                let reply: Value = serde_json::from_str(&text).unwrap();
                if reply["id"] == self.id {
                    assert!(reply.get("error").is_none(), "{method}: {reply}");
                    return reply;
                }
            }
        }
    }

    /// A full CDP session: connect, open a page, navigate to `url`.
    async fn open(port: u16, url: &str) -> Session {
        let (ws, _) = connect_async(format!("ws://127.0.0.1:{port}/devtools/browser"))
            .await
            .expect("connect");
        let mut s = Session { ws, id: 0, sid: String::new() };
        let target = s.call("Target.createTarget", json!({"url": "about:blank"}), false).await;
        let attached = s
            .call(
                "Target.attachToTarget",
                json!({"targetId": target["result"]["targetId"], "flatten": true}),
                false,
            )
            .await;
        s.sid = attached["result"]["sessionId"].as_str().unwrap().to_string();
        s.call("Page.enable", json!({}), true).await;
        s.call("Page.navigate", json!({"url": url}), true).await;
        s
    }

    async fn eval(&mut self, expression: &str) -> Value {
        let reply = self
            .call("Runtime.evaluate", json!({"expression": expression, "returnByValue": true}), true)
            .await;
        reply["result"]["result"]["value"].clone()
    }
}

const PAGE: &str = "data:text/html,<title>recycle</title><body>ok</body>";

#[tokio::test(flavor = "multi_thread")]
async fn connection_limit_replaces_workers_without_failing_sessions() {
    let server = Server::start(&["--worker-max-connections", "3"]);
    let original = server.workers();
    let mut seen: HashSet<u32> = original.iter().copied().collect();

    // A long-lived session on one worker: it must survive that worker being
    // recycled underneath it.
    let mut held = Session::open(server.1, PAGE).await;

    // Sessions arrive faster than a replacement starts, so keep going until
    // several generations of workers have come and gone.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut i = 0;
    while seen.len() < 8 {
        i += 1;
        assert!(Instant::now() < deadline, "workers were not replaced: {seen:?}");
        let mut s = Session::open(server.1, PAGE).await;
        assert_eq!(s.eval("document.title").await, json!("recycle"), "session {i}");
        drop(s);
        seen.extend(server.workers());
    }
    assert_eq!(held.eval("document.title").await, json!("recycle"), "held session survived");

    // The worker that carried `held` outlives its replacement until it closes.
    let now = server.workers();
    let draining: Vec<u32> = original.iter().copied().filter(|pid| now.contains(pid)).collect();
    assert!(!draining.is_empty(), "the held session's worker was killed: {now:?}");
    drop(held);
    // Pool capacity is unchanged and the replaced originals are gone.
    server.settle(2, &original, Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn rss_limit_replaces_a_bloated_worker_and_keeps_its_session() {
    let server = Server::start(&["--worker-max-rss", "150"]);
    let original = server.workers();
    // ~200 MB held live by the page's isolate, well over the 150 MB limit.
    let bloat = "data:text/html,<title>big</title><script>window.keep=new Uint8Array(200*1024*1024).fill(7)</script>";
    let mut held = Session::open(server.1, bloat).await;

    let deadline = Instant::now() + Duration::from_secs(15);
    let replaced = loop {
        let now = server.workers();
        if let Some(pid) = original.iter().find(|pid| !now.contains(pid)) {
            panic!("worker {pid} was killed while its session was live");
        }
        if now.len() == 3 {
            break now;
        }
        assert!(Instant::now() < deadline, "bloated worker was not replaced: {now:?}");
        std::thread::sleep(Duration::from_millis(100));
    };
    // Draining: the replacement exists, the bloated worker still answers.
    assert_eq!(held.eval("window.keep[1000]").await, json!(7.0));
    // New sessions work and never land on the draining worker's limit.
    for _ in 0..4 {
        let mut s = Session::open(server.1, PAGE).await;
        assert_eq!(s.eval("document.title").await, json!("recycle"));
    }
    drop(held);
    let bloated: Vec<u32> = replaced.into_iter().filter(|pid| original.contains(pid)).collect();
    assert_eq!(bloated.len(), 2, "both originals alive while one drains");
    // Exactly one original (the bloated one) leaves; capacity returns to two.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let now = server.workers();
        if now.len() == 2 && original.iter().filter(|pid| now.contains(pid)).count() == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "drained worker not retired: {now:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_stops_a_worker_that_is_still_draining() {
    let mut server = Server::start(&["--worker-max-connections", "1", "--worker-drain-timeout", "300"]);
    let original = server.workers();
    // The held session's worker reaches its limit and is replaced, but keeps
    // running for up to 300 s while the session stays open.
    let mut held = Session::open(server.1, PAGE).await;
    let deadline = Instant::now() + Duration::from_secs(15);
    let all = loop {
        let now = server.workers();
        if now.len() == 3 {
            break now;
        }
        assert!(Instant::now() < deadline, "worker was not replaced: {now:?}");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(original.iter().all(|pid| all.contains(pid)), "draining worker was killed: {all:?}");
    assert_eq!(held.eval("document.title").await, json!("recycle"));

    // Signal only the balancer: every worker, the draining one included, must go.
    assert!(Command::new("kill").args(["-TERM", &server.0.id().to_string()]).status().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "balancer ignored SIGTERM");
        std::thread::sleep(Duration::from_millis(10));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while all.iter().any(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists()) {
        assert!(Instant::now() < deadline, "workers survived SIGTERM: {all:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(held);
}
