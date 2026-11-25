use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn reply(ws: &mut Ws, id: u64) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next()).await
            .expect("timed out waiting for response").expect("connection closed").unwrap();
        let Message::Text(text) = frame else { panic!("closed before response: {frame:?}") };
        let response: Value = serde_json::from_str(&text).unwrap();
        if response["id"].as_u64() == Some(id) { return response; }
    }
}

async fn close_connection(check_parameter: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    tokio::task::LocalSet::new().run_until(async {
        let server = tokio::task::spawn_local(async move {
            obscura_cdp::server::start_with_host_and_security(
                port, "127.0.0.1", None, false, None, false, None).await.unwrap();
        });
        let endpoint = format!("ws://127.0.0.1:{port}/devtools/browser");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let (mut ws, _) = loop {
            match connect_async(&endpoint).await {
                Ok(connection) => break connection,
                Err(error) => {
                    assert!(tokio::time::Instant::now() < deadline, "server did not start: {error}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        // A parameter is not a command, even when its value names a method.
        if check_parameter {
            ws.send(Message::Text(json!({"id":1,"method":"Runtime.enable",
                "params":{"note":"Browser.close"}}).to_string().into())).await.unwrap();
            assert_eq!(reply(&mut ws, 1).await, json!({"id":1,"result":{}}));
        }
        ws.send(Message::Text(json!({"id":2,"method":"Browser.getVersion"}).to_string().into()))
            .await.unwrap();
        assert!(reply(&mut ws, 2).await["result"]["product"].is_string());

        for method in [r#""Browser.close""#, r#""Browser.\u0063lose""#] {
            ws.send(Message::Text(format!(r#"{{"id":3,"method":{method}}}"#).into())).await.unwrap();
            assert_eq!(reply(&mut ws, 3).await, json!({"id":3,"result":{}}));
            let closed = tokio::time::timeout(Duration::from_secs(5), ws.next()).await
                .expect("server did not close the connection");
            assert!(matches!(closed, Some(Ok(Message::Close(_))) | None), "{closed:?}");
            (ws, _) = connect_async(&endpoint).await.unwrap();
            ws.send(Message::Text(json!({"id":4,"method":"Browser.getVersion"}).to_string().into()))
                .await.unwrap();
            assert!(reply(&mut ws, 4).await["result"]["product"].is_string());
        }
        ws.close(None).await.unwrap();
        server.abort();
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn browser_close_flushes_its_reply_and_only_closes_its_connection() {
    close_connection(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn browser_close_in_parameters_does_not_close_the_connection() {
    close_connection(true).await;
}
