use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

const WS_PATH: &str = "/devtools/browser";

#[derive(Deserialize)]
struct Ready {
    pid: u32,
    host: String,
    port: u16,
    websocket_path: String,
}

struct Server {
    child: Child,
    dir: PathBuf,
}

impl Server {
    fn spawn() -> Self {
        let dir = temp_dir();
        std::fs::create_dir(&dir).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_obscura"))
            .args([
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--workers",
                "1",
                "--max-connections",
                "1",
                "--ready-file",
            ])
            .arg(dir.join("ready.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self { child, dir }
    }

    fn wait_ready(&mut self) -> Ready {
        let path = self.dir.join("ready.json");
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(bytes) = std::fs::read(&path) {
                return serde_json::from_slice(&bytes).unwrap();
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before publishing readiness"
            );
            assert!(Instant::now() < deadline, "server did not become ready");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "obscura-ready-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn discovery(port: u16) -> String {
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn ephemeral_servers_publish_distinct_usable_ports() {
    let mut first = Server::spawn();
    let mut second = Server::spawn();
    let first_ready = first.wait_ready();
    let second_ready = second.wait_ready();

    for (server, ready) in [(&first, &first_ready), (&second, &second_ready)] {
        assert_eq!(ready.pid, server.child.id());
        assert_eq!(ready.host, "127.0.0.1");
        assert_ne!(ready.port, 0);
        assert_eq!(ready.websocket_path, WS_PATH);
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(server.dir.join("ready.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            discovery(ready.port).contains(&format!("ws://127.0.0.1:{}{}", ready.port, WS_PATH)),
            "discovery must advertise the actual bound port"
        );
    }
    assert_ne!(first_ready.port, second_ready.port);
}

#[test]
fn unwritable_ready_path_fails_startup() {
    let missing_parent = temp_dir().join("missing").join("ready.json");
    let status = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["serve", "--port", "0", "--ready-file"])
        .arg(&missing_parent)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!Path::new(&missing_parent).exists());
}

#[test]
fn ready_file_rejects_multiple_workers() {
    let ready_file = temp_dir().join("ready.json");
    let output = Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["serve", "--port", "0", "--workers", "2", "--ready-file"])
        .arg(&ready_file)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--ready-file"));
}
