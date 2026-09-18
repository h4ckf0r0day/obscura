#![cfg(feature = "stealth")]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

use obscura::{Browser, StealthPlatform};

// The non-stealth control row: OBSCURA_PROFILE=0 pins the first PROFILES entry.
const ORDINARY_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

fn spawn_server() -> (String, mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = mpsc::sync_channel(1);

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();

        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
        }
        request_tx
            .send(String::from_utf8(request).unwrap())
            .unwrap();

        let body = "<!doctype html><html><body>ok</body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body,
        );
        stream.write_all(response.as_bytes()).unwrap();
    });

    (format!("http://{}", addr), request_rx)
}

/// Navigate and return the wire identity: the `User-Agent` and
/// `Sec-CH-UA-Platform` headers the request actually carried. The
/// `Sec-CH-UA-Platform` value includes the quoted string Chrome sends,
/// e.g. `"Windows"`.
async fn navigate_identity(
    stealth: bool,
    platform: Option<StealthPlatform>,
) -> (String, Option<String>) {
    let (url, request_rx) = spawn_server();

    let mut builder = Browser::builder().stealth(stealth);
    if let Some(platform) = platform {
        builder = builder.stealth_platform(platform);
    }
    let browser = builder.build().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&url).await.unwrap();

    let request = request_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut user_agent = None;
    let mut sec_ch_platform = None;
    for line in request.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "user-agent" => user_agent = Some(value.to_string()),
            "sec-ch-ua-platform" => sec_ch_platform = Some(value.to_string()),
            _ => {}
        }
    }
    (
        user_agent.expect("request should include a user-agent header"),
        sec_ch_platform,
    )
}

#[tokio::test(flavor = "current_thread")]
async fn stealth_transport_requires_compile_time_and_runtime_opt_in() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    std::env::set_var("OBSCURA_PROFILE", "0");

    // The default stealth identity is the host OS, not the historical
    // Windows constant.
    assert_eq!(
        navigate_identity(true, None).await.0,
        StealthPlatform::host().user_agent(),
        "default stealth identity must match the host OS"
    );
    assert_eq!(
        navigate_identity(false, None).await.0,
        ORDINARY_USER_AGENT,
        "non-stealth control keeps the ordinary profile UA"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stealth_platform_windows_identity() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (user_agent, sec_ch) = navigate_identity(true, Some(StealthPlatform::Windows)).await;
    assert_eq!(user_agent, StealthPlatform::Windows.user_agent());
    assert_eq!(
        sec_ch.as_deref(),
        Some("\"Windows\""),
        "wire Sec-CH-UA-Platform must match the Windows identity"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stealth_platform_macos_identity() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (user_agent, sec_ch) = navigate_identity(true, Some(StealthPlatform::MacOS)).await;
    assert_eq!(user_agent, StealthPlatform::MacOS.user_agent());
    assert_eq!(
        sec_ch.as_deref(),
        Some("\"macOS\""),
        "wire Sec-CH-UA-Platform must match the macOS identity"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stealth_platform_linux_identity() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let (user_agent, sec_ch) = navigate_identity(true, Some(StealthPlatform::Linux)).await;
    assert_eq!(user_agent, StealthPlatform::Linux.user_agent());
    assert_eq!(
        sec_ch.as_deref(),
        Some("\"Linux\""),
        "wire Sec-CH-UA-Platform must match the Linux identity"
    );
}
