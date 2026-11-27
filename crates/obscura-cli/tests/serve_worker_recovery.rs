#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-TERM", "--", &format!("-{}", self.0.id())])
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

fn discovery(port: u16) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    write!(stream, "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[test]
fn killed_workers_are_reaped_and_replaced_without_poisoning_discovery() {
    let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);
    let mut server = Server(Command::new(env!("CARGO_BIN_EXE_obscura"))
        .args(["serve", "--host", "127.0.0.1", "--port", &port.to_string(), "--workers", "2"])
        .process_group(0).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(server.0.try_wait().unwrap().is_none(), "balancer exited");
        if discovery(port).is_ok_and(|response| response.starts_with("HTTP/1.1 200")) { break; }
        assert!(Instant::now() < deadline, "balancer did not become ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    for kill_all in [false, true] {
        let original = workers(server.0.id());
        assert_eq!(original.len(), 2);
        let victims = if kill_all { &original[..] } else { &original[..1] };
        for victim in victims {
            assert!(Command::new("kill").args(["-KILL", &victim.to_string()]).status().unwrap().success());
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let current = workers(server.0.id());
            if current.len() == 2 && victims.iter().all(|id| !current.contains(id))
                && discovery(port).is_ok_and(|response| response.starts_with("HTTP/1.1 200")) { break; }
            assert!(Instant::now() < deadline, "dead workers were not reaped and replaced: {current:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
        for _ in 0..12 {
            assert!(discovery(port).unwrap().starts_with("HTTP/1.1 200"),
                "discovery routed to a dead or unready worker");
        }
    }
}
