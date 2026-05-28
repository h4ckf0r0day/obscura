use std::sync::Arc;
use std::time::Duration;

use obscura_browser::context::BrowserContext;
use obscura_browser::lifecycle::WaitUntil;
use obscura_browser::page::Page;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const INDEX: &str = include_str!("fixtures/hh-login-minimized/index.html");
const REGULAR_HANGS: &str = include_str!("fixtures/hh-login-minimized/assets/regular-hangs.js");
const REGULAR_AFTER: &str = include_str!("fixtures/hh-login-minimized/assets/regular-after.js");
const DEFERRED: &str = include_str!("fixtures/hh-login-minimized/assets/deferred.js");
const ASYNC: &str = include_str!("fixtures/hh-login-minimized/assets/async.js");

async fn serve_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = [0_u8; 2048];
                let Ok(n) = socket.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");

                let (status, content_type, body) = match path {
                    "/" | "/account/login?role=applicant&backurl=%2F" => {
                        ("200 OK", "text/html; charset=utf-8", INDEX)
                    }
                    "/assets/regular-hangs.js" => {
                        ("200 OK", "application/javascript", REGULAR_HANGS)
                    }
                    "/assets/regular-after.js" => {
                        ("200 OK", "application/javascript", REGULAR_AFTER)
                    }
                    "/assets/deferred.js" => ("200 OK", "application/javascript", DEFERRED),
                    "/assets/async.js" => ("200 OK", "application/javascript", ASYNC),
                    _ => ("404 Not Found", "text/plain", "not found"),
                };

                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.as_bytes().len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });

    format!("http://{addr}/account/login?role=applicant&backurl=%2F")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_load_invokes_all_hh_login_fixture_scripts_without_hanging() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");

    let url = serve_fixture().await;
    let context = Arc::new(BrowserContext::new("hh-login-fixture".to_string()));
    let mut page = Page::new("hh-login-page".to_string(), context);

    tokio::time::timeout(
        Duration::from_secs(5),
        page.navigate_with_wait(&url, WaitUntil::Load),
    )
    .await
    .expect("full-load navigation should be bounded")
    .expect("fixture navigation should succeed");

    let invoked = page
        .evaluate("window.__hhLoginFixtureInvoked && window.__hhLoginFixtureInvoked.join(',')")
        .as_str()
        .unwrap_or_default()
        .to_string();

    for expected in [
        "inline-init",
        "regular-hangs",
        "regular-after",
        "deferred",
        "async",
    ] {
        assert!(
            invoked.split(',').any(|actual| actual == expected),
            "missing script invocation {expected}; got {invoked:?}"
        );
    }
}
