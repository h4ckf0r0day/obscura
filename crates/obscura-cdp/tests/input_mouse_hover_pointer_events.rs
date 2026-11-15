#![cfg(feature = "render")]

// A hover (Input.dispatchMouseEvent mouseMoved) fires the pointer events a
// real browser fires before the mouse ones: pointerover/pointerenter on the
// way in, pointerout/pointerleave on the way out, and pointermove on every
// move. Menus built on pointer events (Radix and the libraries on top of it)
// open on pointerenter/pointermove and never opened on a hover before.

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
        let body = r##"<!doctype html><html><head><style>
            html, body { margin: 0; }
            #outer { position: absolute; left: 0; top: 0; width: 200px; height: 100px; }
            #inner { position: absolute; left: 20px; top: 20px; width: 60px; height: 40px; }
            #other { position: absolute; left: 300px; top: 0; width: 100px; height: 100px; }
        </style></head><body>
          <div id="outer"><div id="inner"></div></div>
          <div id="other"></div>
          <script>
            window.seen = [];
            for (const type of ['pointerover', 'pointerenter', 'pointermove', 'pointerout', 'pointerleave',
                                'mouseover', 'mouseenter', 'mousemove', 'mouseout', 'mouseleave']) {
              for (const id of ['outer', 'inner', 'other']) {
                document.getElementById(id).addEventListener(type, (e) => {
                  if (e.currentTarget === e.target || !e.bubbles) {
                    seen.push(type + ':' + id + (e instanceof PointerEvent ? ':' + e.pointerType : ''));
                  }
                });
              }
            }
          </script>
        </body></html>"##;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });
    format!("http://{addr}/")
}

async fn cdp(ctx: &mut CdpContext, id: u64, method: &str, params: Value, session_id: &str) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "CDP {method} failed: {:?}", response.error);
    response.result.unwrap_or_else(|| json!({}))
}

async fn take_seen(ctx: &mut CdpContext, id: u64, sid: &str) -> Vec<String> {
    let result = cdp(
        ctx,
        id,
        "Runtime.evaluate",
        json!({"expression": "JSON.stringify(seen.splice(0))", "returnByValue": true}),
        sid,
    )
    .await;
    serde_json::from_str(result["result"]["value"].as_str().unwrap()).unwrap()
}

async fn hover(ctx: &mut CdpContext, id: u64, sid: &str, x: f64, y: f64) {
    cdp(
        ctx,
        id,
        "Input.dispatchMouseEvent",
        json!({"type": "mouseMoved", "x": x, "y": y, "button": "none", "buttons": 0}),
        sid,
    )
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn hover_fires_pointer_boundary_and_move_events_before_mouse_ones() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let url = serve_fixture().await;
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let sid = "hover-session";
    ctx.sessions.insert(sid.to_string(), page_id);
    cdp(&mut ctx, 1, "Page.navigate", json!({"url": url, "waitUntil": "load"}), sid).await;

    hover(&mut ctx, 2, sid, 40.0, 30.0).await;
    let seen = take_seen(&mut ctx, 3, sid).await;
    assert_eq!(
        seen,
        vec![
            "pointerover:inner:mouse",
            "pointerenter:outer:mouse",
            "pointerenter:inner:mouse",
            "mouseover:inner",
            "mouseenter:outer",
            "mouseenter:inner",
            "pointermove:inner:mouse",
            "mousemove:inner",
        ],
        "entering a nested element"
    );

    hover(&mut ctx, 4, sid, 41.0, 31.0).await;
    let seen = take_seen(&mut ctx, 5, sid).await;
    assert_eq!(seen, vec!["pointermove:inner:mouse", "mousemove:inner"], "moving inside it");

    hover(&mut ctx, 6, sid, 150.0, 80.0).await;
    let seen = take_seen(&mut ctx, 7, sid).await;
    assert_eq!(
        seen,
        vec![
            "pointerout:inner:mouse",
            "pointerleave:inner:mouse",
            "pointerover:outer:mouse",
            "mouseout:inner",
            "mouseleave:inner",
            "mouseover:outer",
            "pointermove:outer:mouse",
            "mousemove:outer",
        ],
        "leaving the child for its parent leaves only the child"
    );

    hover(&mut ctx, 8, sid, 350.0, 50.0).await;
    let seen = take_seen(&mut ctx, 9, sid).await;
    assert_eq!(
        seen,
        vec![
            "pointerout:outer:mouse",
            "pointerleave:outer:mouse",
            "pointerover:other:mouse",
            "pointerenter:other:mouse",
            "mouseout:outer",
            "mouseleave:outer",
            "mouseover:other",
            "mouseenter:other",
            "pointermove:other:mouse",
            "mousemove:other",
        ],
        "moving to a sibling"
    );
}
