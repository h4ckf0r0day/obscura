#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", self.0.id())])
            .stdout(Stdio::null()).stderr(Stdio::null()).status();
        let _ = self.0.wait();
    }
}

fn workers(parent: u32) -> Vec<u32> {
    let root = format!("/proc/{parent}/task");
    let mut children = Vec::new();
    for task in std::fs::read_dir(root).unwrap() {
        if let Ok(ids) = std::fs::read_to_string(task.unwrap().path().join("children")) {
            children.extend(ids.split_whitespace().map(|id| id.parse::<u32>().unwrap()));
        }
    }
    children.sort_unstable();
    children.dedup();
    children
}

fn discovery_ok(port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else { return false };
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 200")
}

#[test]
fn balancer_signal_stops_workers_and_workers_get_max_connections() {
    for signal in ["-TERM", "-INT"] {
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let mut server = Server(Command::new(env!("CARGO_BIN_EXE_obscura"))
            .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string(),
                "--workers", "2", "--max-connections", "7"])
            .process_group(0).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while !discovery_ok(port) {
            assert!(server.0.try_wait().unwrap().is_none(), "balancer exited");
            assert!(Instant::now() < deadline, "balancer did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
        let pids = workers(server.0.id());
        assert_eq!(pids.len(), 2);
        for pid in &pids {
            let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
            let argv: Vec<&[u8]> = argv.split(|b| *b == 0).collect();
            assert!(argv.windows(2).any(|w| w == [&b"--max-connections"[..], b"7"]),
                "worker {pid} missing --max-connections 7: {argv:?}");
        }

        // Signal only the balancer, not the process group.
        assert!(Command::new("kill").args([signal, &server.0.id().to_string()]).status().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.0.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "balancer ignored {signal}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while pids.iter().any(|pid| Path::new(&format!("/proc/{pid}")).exists()) {
            assert!(Instant::now() < deadline, "workers survived {signal}: {pids:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
