#![cfg(feature = "render")]

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn serve_fixture() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 2048];
        let _ = socket.read(&mut buf).await.unwrap();
        let body = r#"<!doctype html><html><head><style>
            html, body { margin: 0 }
            div { width: 200px; height: 100px }
        </style></head><body>
          <div id="pad"></div>
          <div id="blocker"></div>
          <div id="drop"></div>
          <script>
            window.events = [];
            const log = (id, e) => window.events.push(id + ':' + e.type + ':' + e.isTrusted +
              (e.touches ? ':' + e.touches.length + '/' + e.changedTouches.length : ''));
            const pad = document.getElementById('pad');
            for (const t of ['pointerdown','touchstart','touchmove','pointerup','touchend','mousedown','mouseup','click']) {
              pad.addEventListener(t, e => log('pad', e));
            }
            const blocker = document.getElementById('blocker');
            blocker.addEventListener('touchstart', e => e.preventDefault());
            blocker.addEventListener('click', e => log('blocker', e));
            const drop = document.getElementById('drop');
            drop.addEventListener('dragenter', e => log('drop', e));
            drop.addEventListener('dragover', e => { e.preventDefault(); log('drop', e); });
            drop.addEventListener('drop', e => {
              e.preventDefault();
              window.events.push('drop:data:' + e.dataTransfer.getData('text/plain') + ':' + e.dataTransfer.types.join('|'));
            });
          </script>
        </body></html>"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, sid: &str) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(sid.to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "CDP {method} failed: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn events(ctx: &mut CdpContext, id: u64, sid: &str) -> String {
    let result = cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": "window.events.splice(0).join(',')", "returnByValue": true}),
        sid,
    )
    .await;
    result["result"]["value"].as_str().unwrap().to_string()
}

async fn setup() -> (CdpContext, String) {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_fixture().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let sid = "touch-drag-session";
    ctx.sessions.insert(sid.to_string(), page_id);
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), sid).await;
    (ctx, sid.to_string())
}

#[tokio::test(flavor = "current_thread")]
async fn tap_dispatches_touch_pointer_and_click() {
    let (mut ctx, sid) = setup().await;
    let point = json!([{"x": 50, "y": 50}]);
    cdp(&mut ctx, 2, "Input.dispatchTouchEvent", json!({"type": "touchStart", "touchPoints": point}), &sid).await;
    cdp(&mut ctx, 3, "Input.dispatchTouchEvent", json!({"type": "touchEnd", "touchPoints": []}), &sid).await;
    assert_eq!(
        events(&mut ctx, 4, &sid).await,
        "pad:pointerdown:true,pad:touchstart:true:1/1,pad:pointerup:true,pad:touchend:true:0/1,\
         pad:mousedown:true,pad:mouseup:true,pad:click:true"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn moved_touch_and_prevented_touchstart_do_not_click() {
    let (mut ctx, sid) = setup().await;
    cdp(&mut ctx, 2, "Input.dispatchTouchEvent", json!({"type": "touchStart", "touchPoints": [{"x": 20, "y": 20}]}), &sid).await;
    cdp(&mut ctx, 3, "Input.dispatchTouchEvent", json!({"type": "touchMove", "touchPoints": [{"x": 120, "y": 20}]}), &sid).await;
    cdp(&mut ctx, 4, "Input.dispatchTouchEvent", json!({"type": "touchEnd", "touchPoints": []}), &sid).await;
    let swipe = events(&mut ctx, 5, &sid).await;
    assert!(swipe.contains("pad:touchmove:true:1/1"), "{swipe}");
    assert!(!swipe.contains("click"), "{swipe}");

    cdp(&mut ctx, 6, "Input.dispatchTouchEvent", json!({"type": "touchStart", "touchPoints": [{"x": 50, "y": 150}]}), &sid).await;
    cdp(&mut ctx, 7, "Input.dispatchTouchEvent", json!({"type": "touchEnd", "touchPoints": []}), &sid).await;
    assert_eq!(events(&mut ctx, 8, &sid).await, "");
}

#[tokio::test(flavor = "current_thread")]
async fn drag_events_carry_data_to_drop_target() {
    let (mut ctx, sid) = setup().await;
    let data = json!({"items": [{"mimeType": "text/plain", "data": "hello"}], "dragOperationsMask": 1});
    for (i, kind) in ["dragEnter", "dragOver", "drop"].iter().enumerate() {
        cdp(
            &mut ctx,
            10 + i as u64,
            "Input.dispatchDragEvent",
            json!({"type": kind, "x": 50, "y": 250, "data": data}),
            &sid,
        )
        .await;
    }
    assert_eq!(
        events(&mut ctx, 20, &sid).await,
        "drop:dragenter:true,drop:dragover:true,drop:dragover:true,drop:data:hello:text/plain"
    );
}
