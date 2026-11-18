#![cfg(not(target_arch = "wasm32"))]
mod support;

use support::server;
use wreq::Client;
use wreq_util::{Emulation, Platform};

#[tokio::test]
async fn test_client_emulation_device() {
    let server = server::http(move |req| async move {
        for (name, value) in req.headers() {
            if name == "user-agent" {
                assert_eq!(
                    value,
                    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/133.0.0.0 Safari/537.36"
                );
            }
            if name == "sec-ch-ua" {
                assert_eq!(
                    value,
                    r#""Not(A:Brand";v="99", "Google Chrome";v="133", "Chromium";v="133""#
                );
            }
            if name == "sec-ch-ua-mobile" {
                assert_eq!(value, "?0");
            }
            if name == "sec-ch-ua-platform" {
                assert_eq!(value, "\"Linux\"");
            }
        }
        http::Response::default()
    });

    let url = format!("http://{}/ua", server.addr());
    let res = Client::builder()
        .emulation(
            Emulation::builder()
                .profile(Emulation::Chrome133)
                .platform(Platform::Linux)
                .http2(true)
                .build(),
        )
        .build()
        .expect("Unable to build client")
        .get(&url)
        .send()
        .await
        .expect("request");

    assert_eq!(res.status(), wreq::StatusCode::OK);
}
