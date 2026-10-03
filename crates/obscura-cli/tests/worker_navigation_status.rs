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
    let (status, headers, body) = match path {
        "/redirect" => (302, "Location: /missing\r\n", ""),
        "/chain" => (301, "Location: /redirect\r\n", ""),
        "/missing" => (404, "", "<title>Missing</title><p>Not found</p>"),
        "/server-error" => (503, "", "<title>Unavailable</title><p>Try later</p>"),
        "/empty" => (204, "", ""),
        "/assets" => (200, "", "<title>Main document</title><script src='/server-error'></script><iframe src='/missing'></iframe><p>Main body</p>"),
        "/client-redirect" => (200, "", "<script>location.href = '/missing';</script>"),
        "/same-document" => (201, "", "<title>Same document</title><script>history.pushState({}, '', '/changed');</script>"),
        _ => (200, "", "<title>Success</title><p>Worker body</p>"),
    };
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

#[test]
fn navigate_reports_success_and_http_errors_as_responses() {
    let server = Server::new();
    let mut worker = Worker::new();
    for (path, status) in [
        ("/ok", 200),
        ("/missing", 404),
        ("/server-error", 503),
        ("/empty", 204),
    ] {
        let url = server.url(path);
        let result = worker.navigate(&url);
        assert_eq!(result["status"], status, "{path}: {result}");
        assert_eq!(result["url"], url);
    }
}

#[test]
fn navigate_reports_final_redirect_status_and_url() {
    let server = Server::new();
    let mut worker = Worker::new();
    for path in ["/redirect", "/chain"] {
        let result = worker.navigate(&server.url(path));
        assert_eq!(result["status"], 404, "{result}");
        assert_eq!(result["url"], server.url("/missing"));
        assert_eq!(result["title"], "Missing");
    }
}

#[test]
fn subresource_and_child_frame_statuses_do_not_replace_the_main_response() {
    let server = Server::new();
    let mut worker = Worker::new();
    let result = worker.navigate(&server.url("/assets"));
    assert_eq!(result["status"], 200, "{result}");
    assert_eq!(result["title"], "Main document");
}

#[test]
fn reused_worker_reports_each_navigation_independently() {
    let server = Server::new();
    let mut worker = Worker::new();
    for (path, status) in [
        ("/server-error", 503),
        ("/ok", 200),
        ("/missing", 404),
        ("/ok", 200),
    ] {
        let result = worker.navigate(&server.url(path));
        assert_eq!(result["status"], status, "{result}");
    }
}

#[test]
fn client_redirect_reports_the_replacement_document_status() {
    let server = Server::new();
    let mut worker = Worker::new();
    let result = worker.navigate(&server.url("/client-redirect"));
    assert_eq!(result["status"], 404, "{result}");
    assert_eq!(result["url"], server.url("/missing"));
}

#[test]
fn same_document_url_changes_keep_the_loaded_document_status() {
    let server = Server::new();
    let mut worker = Worker::new();
    let result = worker.navigate(&server.url("/same-document"));
    assert_eq!(result["status"], 201, "{result}");
    let location = worker.send(json!({ "cmd": "evaluate", "expression": "location.href" }));
    assert_eq!(location["result"], server.url("/changed"));
}

#[test]
fn non_http_navigation_has_an_explicit_null_status() {
    let server = Server::new();
    let mut worker = Worker::new();
    assert_eq!(worker.navigate(&server.url("/ok"))["status"], 200);
    for url in [
        "about:blank",
        "data:text/html,<title>Inline</title><p>Data</p>",
    ] {
        let result = worker.navigate(url);
        assert!(
            result.as_object().unwrap().contains_key("status"),
            "{result}"
        );
        assert_eq!(result["status"], Value::Null, "{result}");
    }
}

#[test]
fn transport_failure_preserves_error_envelope_and_worker_recovers() {
    let server = Server::new();
    let mut worker = Worker::new();
    assert_eq!(worker.navigate(&server.url("/ok"))["status"], 200);
    let failed = worker.send(json!({ "cmd": "navigate", "url": server.url("/disconnect") }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(failed["error"].as_str().is_some());
    assert!(failed.get("result").is_none(), "{failed}");
    assert_eq!(worker.navigate(&server.url("/missing"))["status"], 404);
    assert_eq!(worker.send(json!({ "cmd": "title" }))["result"], "Missing");
    assert!(worker.send(json!({ "cmd": "dump_html" }))["result"]
        .as_str()
        .unwrap()
        .contains("Not found"));
    assert!(worker.send(json!({ "cmd": "dump_text" }))["result"]
        .as_str()
        .unwrap()
        .contains("Not found"));
    assert_eq!(
        worker.send(json!({ "cmd": "evaluate", "expression": "document.title" }))["result"],
        "Missing"
    );
    assert_eq!(worker.send(json!({ "cmd": "shutdown" }))["result"], "bye");
}

#[cfg(feature = "stealth")]
#[test]
fn stealth_transport_reports_http_status_and_recovers_after_failure() {
    let server = Server::new();
    let mut worker = Worker::with_stealth(true);
    for (path, status) in [("/ok", 200), ("/chain", 404), ("/server-error", 503)] {
        let result = worker.navigate(&server.url(path));
        assert_eq!(result["status"], status, "{result}");
    }
    let failed = worker.send(json!({ "cmd": "navigate", "url": server.url("/disconnect") }));
    assert_eq!(failed["ok"], false, "{failed}");
    assert!(failed.get("result").is_none(), "{failed}");
    assert_eq!(worker.navigate(&server.url("/ok"))["status"], 200);
    assert_eq!(worker.navigate("about:blank")["status"], Value::Null);
}

#[test]
fn blocked_document_has_no_fabricated_http_status() {
    // The blocklist short-circuits before DNS or HTTP, so this stays offline.
    assert!(obscura_net::blocklist::is_blocked("google-analytics.com"));
    let mut worker = Worker::with_stealth(true);
    let result = worker.navigate("https://google-analytics.com/worker-status-test");
    assert!(
        result.as_object().unwrap().contains_key("status"),
        "{result}"
    );
    assert_eq!(result["status"], Value::Null, "{result}");
}
