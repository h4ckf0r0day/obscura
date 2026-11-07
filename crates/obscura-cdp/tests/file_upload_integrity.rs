use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{connect_async, tungstenite::Message};

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn call(ws: &mut Ws, id: u64, method: &str, params: Value,
    session: Option<&str>) -> Value {
    let mut command = json!({"id":id, "method":method, "params":params});
    if let Some(session) = session { command["sessionId"] = json!(session); }
    ws.send(Message::Text(command.to_string().into())).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next()).await
            .unwrap_or_else(|_| panic!("timeout waiting for {method}"))
            .expect("connection closed").unwrap();
        let Message::Text(text) = frame else { continue };
        let reply: Value = serde_json::from_str(&text).unwrap();
        if reply["id"].as_u64() == Some(id) { return reply; }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn file_uploads_validate_before_reading_and_preserve_frame_identity() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let path = std::env::temp_dir().join(format!("obscura-upload-integrity-{}.bin", std::process::id()));
    std::fs::write(&path, [0_u8, 128, 255, 13, 10]).unwrap();
    let missing = path.with_extension("missing");
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    tokio::task::LocalSet::new().run_until(async {
        tokio::task::spawn_local(async move {
            obscura_cdp::server::start_with_host_and_security(
                port, "127.0.0.1", None, false, None, true, None).await.unwrap();
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let (mut ws, _) = loop {
            match connect_async(format!("ws://127.0.0.1:{port}/devtools/browser")).await {
                Ok(connection) => break connection,
                Err(error) => {
                    assert!(tokio::time::Instant::now() < deadline, "server did not start: {error}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        let created = call(&mut ws, 1, "Target.createTarget", json!({"url":"about:blank"}), None).await;
        let target = created["result"]["targetId"].as_str().expect("target id");
        let session = format!("{target}-session");
        let sid = Some(session.as_str());
        let html = "<input id=f type=file multiple><input id=s type=file>\
            <input id=t type=text><div id=d></div>\
            <iframe srcdoc=\"<input id=child type=file>\"></iframe>";
        let navigated = call(&mut ws, 2, "Page.navigate",
            json!({"url":format!("data:text/html,{html}"), "waitUntil":"load"}), sid).await;
        assert!(navigated.get("error").is_none(), "{navigated}");
        let mut nodes = Vec::new();
        for (index, selector) in ["#f", "#s", "#t", "#d"].into_iter().enumerate() {
            let reply = call(&mut ws, 3 + index as u64, "DOM.querySelector",
                json!({"selector":selector}), sid).await;
            nodes.push(reply["result"]["nodeId"].as_u64().expect("node id"));
        }
        let selected = call(&mut ws, 7, "DOM.setFileInputFiles",
            json!({"backendNodeId":nodes[0], "files":[path, path]}), sid).await;
        assert!(selected.get("error").is_none(), "{selected}");
        for (index, (node, expected)) in [
            (999_999, "Could not find node"), (nodes[2], "not a file input"),
            (nodes[3], "not a file input"),
        ].into_iter().enumerate() {
            let reply = call(&mut ws, 8 + index as u64, "DOM.setFileInputFiles",
                json!({"nodeId":node, "files":[missing]}), sid).await;
            let message = reply["error"]["message"].as_str().expect("target error");
            assert!(message.contains(expected), "{reply}");
            assert!(!message.contains("cannot read"), "read paths before validating: {reply}");
        }
        for (index, files) in [json!(null), json!("invalid"), json!([42]), json!([null])]
            .into_iter().enumerate() {
            let reply = call(&mut ws, 11 + index as u64, "DOM.setFileInputFiles",
                json!({"nodeId":nodes[0], "files":files}), sid).await;
            assert!(reply.get("error").is_some(), "{reply}");
        }
        let omitted = call(&mut ws, 15, "DOM.setFileInputFiles",
            json!({"nodeId":nodes[0]}), sid).await;
        assert!(omitted.get("error").is_some(), "{omitted}");
        let state = call(&mut ws, 16, "Runtime.evaluate",
            json!({"expression":"document.getElementById('f').files.length", "returnByValue":true}), sid).await;
        assert_eq!(state["result"]["result"]["value"].as_f64(), Some(2.0), "{state}");

        let single = call(&mut ws, 17, "DOM.setFileInputFiles",
            json!({"nodeId":nodes[1], "files":[path, path]}), sid).await;
        assert!(single.get("error").is_none(), "{single}");
        let state = call(&mut ws, 18, "Runtime.evaluate",
            json!({"expression":"document.getElementById('s').files.length", "returnByValue":true}), sid).await;
        assert_eq!(state["result"]["result"]["value"].as_f64(), Some(1.0), "{state}");

        let remote = call(&mut ws, 19, "Runtime.evaluate",
            json!({"expression":"document.querySelector('iframe').contentDocument.getElementById('child')"}), sid).await;
        let object = remote["result"]["result"]["objectId"].as_str().expect("child node handle");
        let child = call(&mut ws, 20, "DOM.setFileInputFiles",
            json!({"objectId":object, "files":[path]}), sid).await;
        assert!(child.get("error").is_none(), "{child}");
        let state = call(&mut ws, 21, "Runtime.evaluate", json!({
            "expression":"(async () => {const f=document.querySelector('iframe').contentDocument.getElementById('child').files[0];return [f.name,Array.from(new Uint8Array(await f.arrayBuffer()))]})()",
            "awaitPromise":true, "returnByValue":true,
        }), sid).await;
        assert_eq!(state["result"]["result"]["value"], json!([name, [0,128,255,13,10]]), "{state}");

        call(&mut ws, 22, "Runtime.evaluate", json!({
            "expression":"(() => {const original=__obscura_setInputFiles;globalThis.__obscura_setInputFiles=(el,specs,dry)=>dry ? original(el,specs,true) : false})()",
        }), sid).await;
        let rejected = call(&mut ws, 23, "DOM.setFileInputFiles",
            json!({"nodeId":nodes[0], "files":[path]}), sid).await;
        assert!(rejected["error"]["message"].as_str().unwrap().contains("Could not set files"), "{rejected}");
        for (index, mut params) in [json!({"nodeId":null}), json!({"backendNodeId":"invalid"})]
            .into_iter().enumerate() {
            params["objectId"] = json!(object);
            params["files"] = json!([missing]);
            let reply = call(&mut ws, 24 + index as u64, "DOM.setFileInputFiles", params, sid).await;
            assert!(reply["error"]["message"].as_str().unwrap().contains("valid nodeId"), "{reply}");
        }
        ws.close(None).await.unwrap();
    }).await;
    std::fs::remove_file(path).unwrap();
}
