use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

struct RobotsServer {
    address: String,
    paths: Arc<Mutex<Vec<String>>>,
}

impl RobotsServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind robots fixture");
        listener
            .set_nonblocking(true)
            .expect("set fixture nonblocking");
        let address = listener.local_addr().expect("fixture address").to_string();
        let paths = Arc::new(Mutex::new(Vec::new()));
        let server_paths = Arc::clone(&paths);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream, &server_paths),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept fixture connection: {error}"),
                }
            }
        });
        Self { address, paths }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }

    fn paths(&self) -> Vec<String> {
        self.paths.lock().expect("fixture paths").clone()
    }
}

fn serve(mut stream: TcpStream, paths: &Arc<Mutex<Vec<String>>>) {
    stream
        .set_nonblocking(false)
        .expect("set fixture connection blocking");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set fixture read timeout");
    let mut request = [0_u8; 4096];
    let count = stream.read(&mut request).expect("read fixture request");
    let first_line = String::from_utf8_lossy(&request[..count])
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");
    paths.lock().expect("fixture paths").push(path.to_string());
    let (content_type, body) = if path == "/robots.txt" {
        ("text/plain", "User-agent: *\nDisallow: /private\n")
    } else {
        ("text/html", "<!doctype html><title>target reached</title><p>private target</p>")
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write fixture response");
}

fn obscura(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(args)
        .output()
        .expect("run obscura CLI")
}

#[test]
fn obey_robots_is_global_and_blocks_fetch_before_target_request() {
    let server = RobotsServer::spawn();
    let url = server.url("/private/page");
    let output = obscura(&[
        "fetch",
        "--obey-robots",
        "--allow-private-network",
        "--quiet",
        "--wait",
        "0",
        &url,
    ]);

    assert!(!output.status.success(), "disallowed fetch must fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Blocked by robots.txt"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.paths(), vec!["/robots.txt"]);
}

#[test]
fn obey_robots_reaches_scrape_worker_and_blocks_target_request() {
    let server = RobotsServer::spawn();
    let url = server.url("/private/page");
    let output = obscura(&[
        "--obey-robots",
        "--allow-private-network",
        "scrape",
        "--quiet",
        "--timeout",
        "5",
        &url,
    ]);

    assert!(output.status.success(), "scrape reports per-URL errors as JSON");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Blocked by robots.txt"), "stdout: {stdout}");
    assert_eq!(server.paths(), vec!["/robots.txt"]);
}

#[test]
fn fetch_without_obey_robots_keeps_existing_navigation_behavior() {
    let server = RobotsServer::spawn();
    let url = server.url("/private/page");
    let output = obscura(&[
        "--allow-private-network",
        "fetch",
        "--quiet",
        "--wait",
        "0",
        &url,
    ]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.paths(), vec!["/private/page"]);
}

#[test]
fn obey_robots_fetches_an_allowed_target_after_loading_policy() {
    let server = RobotsServer::spawn();
    let url = server.url("/public/page");
    let output = obscura(&[
        "--obey-robots",
        "--allow-private-network",
        "fetch",
        "--quiet",
        "--wait",
        "0",
        &url,
    ]);

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.paths(), vec!["/robots.txt", "/public/page"]);
}
