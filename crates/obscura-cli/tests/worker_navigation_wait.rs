use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

struct Server {
    address: String,
    stop: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let task = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept fixture: {error}"),
                }
            }
        });
        Self {
            address,
            stop,
            task: Some(task),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn serve(mut stream: TcpStream) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(_) => return,
        };
        if count == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..count]);
        assert!(request.len() < 8192, "fixture request too large");
    }
    let request = String::from_utf8_lossy(&request);
    let path = request
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    if path == "/disconnect" {
        return;
    }
    let (status, headers) = match path {
        "/redirect" => (302, "Location: /missing\r\n"),
        "/missing" => (404, ""),
        _ => (200, ""),
    };
    let body = "<title>Ready</title><script>globalThis.inlineRan = true; setTimeout(() => { globalThis.laterRan = true; }, 350);</script><p>Body</p>";
    let response = format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: text/html\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()
    );
    // A navigation cancellation may close the connection before this write.
    let _ = stream.write_all(response.as_bytes());
}

struct Worker {
    child: Child,
    input: ChildStdin,
    responses: mpsc::Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}

impl Worker {
    fn new() -> Self {
        Self::with_stealth(false)
    }

    fn with_stealth(stealth: bool) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_obscura-worker"))
            .env("OBSCURA_ALLOW_PRIVATE_NETWORK", "1")
            .env("OBSCURA_STEALTH", if stealth { "1" } else { "0" })
            .env("OBSCURA_OBEY_ROBOTS", "0")
            .env("OBSCURA_NAV_TIMEOUT_MS", "5000")
            .env_remove("OBSCURA_PROXY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let response = serde_json::from_str(&line.unwrap()).unwrap();
                if sender.send(response).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            responses,
            reader: Some(reader),
        }
    }

    fn send(&mut self, command: Value) -> Value {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        self.responses
            .recv_timeout(Duration::from_secs(10))
            .expect("worker response before deadline")
    }

    fn navigate(&mut self, url: &str) -> Value {
        let response = self.send(json!({ "cmd": "navigate", "url": url }));
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

fn check_modes(stealth: bool) {
    let server = Server::new();
    let mut worker = Worker::with_stealth(stealth);
    for mode in ["load", "domcontentloaded", "networkidle0", "networkidle2"] {
        let response =
            worker.send(json!({"cmd": "navigate", "url": server.url("/"), "waitUntil": mode}));
        assert_eq!(response["ok"], true, "{mode}: {response}");
        assert_eq!(response["result"]["title"], "Ready");
        assert_eq!(response["result"]["status"], 200);
        let inline = worker.send(json!({"cmd": "evaluate", "expression": "globalThis.inlineRan"}));
        assert_eq!(inline["result"], true, "parser script must run with {mode}");
        if mode.starts_with("networkidle") {
            let later = worker
                .send(json!({"cmd": "evaluate", "expression": "globalThis.laterRan === true"}));
            assert_eq!(
                later["result"], true,
                "{mode} must drive the event loop during its idle window"
            );
        }
        let error_page =
            worker.send(json!({"cmd":"navigate","url":server.url("/redirect"),"waitUntil":mode}));
        assert_eq!(
            error_page["ok"], true,
            "{mode}: HTTP error remains a response"
        );
        assert_eq!(
            error_page["result"]["status"], 404,
            "{mode}: final document status after redirect"
        );
        assert_eq!(error_page["result"]["url"], server.url("/missing"));
    }
    assert_eq!(worker.navigate(&server.url("/"))["title"], "Ready");
}

#[test]
fn every_readiness_mode_runs_parser_scripts_and_idle_modes_drive_timers() {
    check_modes(false);
}

#[cfg(feature = "stealth")]
#[test]
fn readiness_modes_work_with_the_stealth_transport() {
    check_modes(true);
}

#[test]
fn omitted_mode_keeps_load_default_after_a_request_with_an_explicit_mode() {
    let server = Server::new();
    let mut worker = Worker::new();
    worker.send(json!({"cmd": "navigate", "url": server.url("/"), "waitUntil": "networkidle0"}));
    worker.navigate(&server.url("/"));
    let later =
        worker.send(json!({"cmd": "evaluate", "expression": "globalThis.laterRan === true"}));
    assert_eq!(
        later["result"], false,
        "network-idle selection must not leak to the next request"
    );
}

#[test]
fn invalid_modes_are_errors_and_do_not_navigate_or_poison_the_worker() {
    let mut worker = Worker::new();
    worker.navigate("data:text/html,<title>Original</title>");
    for mode in [
        json!("domContentLoaded"),
        json!("networkIdle"),
        json!(""),
        json!(null),
        json!(1),
        json!(true),
    ] {
        let response = worker.send(json!({"cmd": "navigate", "url": "data:text/html,<title>Changed</title>", "waitUntil": mode}));
        assert_eq!(response["ok"], false, "{mode}: {response}");
        assert!(response["error"]
            .as_str()
            .unwrap()
            .starts_with("Invalid command:"));
        assert_eq!(worker.send(json!({"cmd": "title"}))["result"], "Original");
    }
    assert_eq!(
        worker.navigate("data:text/html,<title>Recovered</title>")["title"],
        "Recovered"
    );
}

#[test]
fn readiness_modes_preserve_non_http_navigation() {
    let mut worker = Worker::new();
    for mode in ["load", "domcontentloaded", "networkidle0", "networkidle2"] {
        for url in [
            "about:blank",
            "data:text/html,<title>Data</title><p>Local</p>",
        ] {
            let response = worker.send(json!({"cmd": "navigate", "url": url, "waitUntil": mode}));
            assert_eq!(response["ok"], true, "{mode}: {response}");
            assert_eq!(
                response["result"]["status"],
                Value::Null,
                "{mode}: non-HTTP navigation has no status"
            );
        }
    }
}
