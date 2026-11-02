use std::sync::Arc;
use obscura_net::{CookieJar, ObscuraHttpClient};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn request(socket: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut data = Vec::new(); let mut chunk = [0; 4096];
    loop {
        let n = socket.read(&mut chunk).await.unwrap(); assert!(n > 0);
        data.extend_from_slice(&chunk[..n]);
        if let Some(end) = data.windows(4).position(|v| v == b"\r\n\r\n") {
            let head = String::from_utf8(data[..end].to_vec()).unwrap();
            let size = head.lines().filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
            if data.len() >= end + 4 + size { return (head, data[end+4..end+4+size].to_vec()); }
        }
    }
}

async fn redirects_preserve_method_and_body(stealth: bool) {
    for cross_origin in [false, true] {
    for (status, method, expected) in [(301,"PUT","PUT"),(302,"PUT","PUT"),(303,"HEAD","HEAD"),(303,"PUT","GET"),(302,"POST","GET"),(307,"POST","POST"),(308,"POST","POST")] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = url::Url::parse(&format!("http://{}/start", listener.local_addr().unwrap())).unwrap();
        let other = if cross_origin { Some(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()) } else { None };
        let target = if let Some(other) = &other { format!("http://{}/finish", other.local_addr().unwrap()) } else { "/finish".into() };
        let fixture = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let (head, _) = request(&mut first).await; assert!(head.starts_with(method));
            assert!(head.to_ascii_lowercase().contains("x-routed: retained"));
            first.write_all(format!("HTTP/1.1 {status} Redirect\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            let (mut second, _) = other.as_ref().unwrap_or(&listener).accept().await.unwrap();
            let (head, bytes) = request(&mut second).await;
            assert!(head.starts_with(&format!("{expected} /finish ")), "{status}: {head}");
            let lower = head.to_ascii_lowercase();
            assert!(lower.contains("x-routed: retained"));
            assert_eq!(lower.contains("authorization: bearer test"), !cross_origin, "credentials must not cross origins: {head}");
            assert_eq!(lower.contains("content-type: application/octet-stream"), expected != "GET", "body headers follow the redirect method: {head}");
            assert_eq!(bytes, if expected == "GET" || expected == "HEAD" { vec![] } else { b"abc".to_vec() });
            second.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await.unwrap();
        });
        let body = (method != "HEAD").then(|| b"abc".to_vec());
        let headers = Some([("x-routed".into(),"retained".into()),("Authorization".into(),"Bearer test".into()),
            ("content-type".into(),"application/octet-stream".into())].into());
        #[cfg(feature = "stealth")]
        let response = if stealth {
            obscura_net::StealthHttpClient::with_proxy(Arc::new(CookieJar::new()),None,true)
                .fetch_navigation_request(&url, method, body, headers, None).await.unwrap()
        } else {
            ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()),None,true)
                .fetch_navigation_request(&url, method, body, headers, None).await.unwrap()
        };
        #[cfg(not(feature = "stealth"))]
        let response = {
            assert!(!stealth);
            ObscuraHttpClient::with_full_options(Arc::new(CookieJar::new()),None,true)
                .fetch_navigation_request(&url, method, body, headers, None).await.unwrap()
        };
        assert_eq!(response.status,200); fixture.await.unwrap();
    }
    }
}

#[tokio::test]
async fn navigation_interception_redirects_preserve_method_and_body() {
    tokio::time::timeout(std::time::Duration::from_secs(10),redirects_preserve_method_and_body(false)).await.unwrap();
}

#[cfg(feature = "stealth")]
#[tokio::test]
async fn navigation_interception_redirects_preserve_method_and_body_in_stealth() {
    tokio::time::timeout(std::time::Duration::from_secs(10),redirects_preserve_method_and_body(true)).await.unwrap();
}
