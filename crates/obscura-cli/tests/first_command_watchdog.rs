use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::{connect_async, tungstenite::Message};

struct Server(Child, std::path::PathBuf);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn first_script_command_on_a_blank_page_has_a_watchdog() {
    for (method, await_promise) in [
        ("Runtime.evaluate", false), ("Runtime.evaluate", true),
        ("Runtime.callFunctionOn", false), ("Runtime.callFunctionOn", true),
        ("Page.addScriptToEvaluateOnNewDocument", false),
    ] {
        let root = std::env::temp_dir().join(format!(
            "obscura-first-watchdog-{}-{}-{await_promise}", std::process::id(), method,
        ));
        std::fs::create_dir(&root).expect("fixture directory");
        let ready = root.join("ready.json");
        let mut server = Server(Command::new(env!("CARGO_BIN_EXE_obscura"))
            .args(["serve", "--host", "127.0.0.1", "--port", "0", "--workers", "1", "--ready-file"])
            .arg(&ready).env("OBSCURA_CDP_COMMAND_TIMEOUT_MS", "100")
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("server"), root);
        let deadline = Instant::now() + Duration::from_secs(10);
        let address: Value = loop {
            if let Ok(bytes) = std::fs::read(&ready) { break serde_json::from_slice(&bytes).expect("ready JSON"); }
            assert!(server.0.try_wait().unwrap().is_none(), "server exited");
            assert!(Instant::now() < deadline, "server readiness timeout");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), connect_async(format!(
            "ws://127.0.0.1:{}/devtools/browser", address["port"],
        ))).await.expect("CDP connection timeout").expect("CDP connection");

        // No Runtime.enable or warm-up evaluation: those would hide the bug.
        socket.send(Message::Text(json!({"id":1,"method":"Target.createTarget","params":{"url":"about:blank"}}).to_string().into())).await.unwrap();
        let created = response(&mut socket, 1).await;
        socket.send(Message::Text(json!({"id":2,"method":"Target.attachToTarget","params":{"targetId":created["result"]["targetId"],"flatten":true}}).to_string().into())).await.unwrap();
        let attached = response(&mut socket, 2).await;
        let session = attached["result"]["sessionId"].as_str().expect("session");
        let mut params = json!({"returnByValue":true,"awaitPromise":await_promise,"timeout":10000});
        let source = "globalThis.started=true;while(true){}";
        if method == "Runtime.evaluate" { params["expression"] = json!(source); }
        else if method == "Runtime.callFunctionOn" { params["functionDeclaration"] = json!(format!("function(){{{source}}}")); }
        else { params = json!({"source":source,"runImmediately":true}); }
        socket.send(Message::Text(json!({"id":3,"sessionId":session,"method":method,"params":params}).to_string().into())).await.unwrap();
        let interrupted = response(&mut socket, 3).await;
        if method == "Page.addScriptToEvaluateOnNewDocument" {
            // Registration survives script exceptions, including termination.
            assert!(interrupted["result"]["identifier"].is_string(), "{method}: {interrupted}");
        } else {
            assert!(interrupted.get("error").is_some() || interrupted["result"].get("exceptionDetails").is_some(), "{method}: {interrupted}");
            let error = interrupted.to_string().to_lowercase();
            assert!(error.contains("terminated") || error.contains("timeout") || error.contains("timed out") || error.contains("exceeded"), "expected a watchdog interruption: {interrupted}");
        }

        socket.send(Message::Text(json!({"id":4,"sessionId":session,"method":"Runtime.evaluate","params":{"expression":"[1+1,globalThis.started]","returnByValue":true}}).to_string().into())).await.unwrap();
        let after = response(&mut socket, 4).await;
        assert_eq!(after["result"]["result"]["value"], json!([2, true]), "script must have started and isolate must remain usable: {after}");
    }
}

async fn response(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    id: u64,
) -> Value {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Message::Text(text) = socket.next().await.expect("reply").expect("WebSocket frame") {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["id"].as_u64() == Some(id) { return value; }
            }
        }
    }).await.unwrap_or_else(|_| panic!("no response to command {id}; the first command must be watchdog bounded"))
}
