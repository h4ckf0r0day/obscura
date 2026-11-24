use obscura_dom::parse_html;
use obscura_js::{frame::FrameRealm, runtime::ObscuraJsRuntime};

#[tokio::test(flavor = "current_thread")]
async fn frame_posted_task_messages_keep_the_senders_origin_and_target() {
    let mut parent = ObscuraJsRuntime::new();
    parent.set_dom(parse_html("<html><body></body></html>"));
    parent.set_url("https://parent.example/");
    parent.run_page_init();
    let frame = FrameRealm::new(&mut parent, 1, 0, "https://child.example/",
        "<html><body></body></html>").expect("frame realm");
    frame.execute_script(&mut parent, r#"
        scheduler.postTask(() => parent.postMessage('from-frame-task', '*'));
    "#).unwrap();
    parent.run_event_loop().await.unwrap();
    let messages = parent.take_pending_frame_messages();
    assert_eq!(messages.len(), 1, "a frame task must queue its message to the parent");
    let message = &messages[0];
    assert_eq!(message.source_frame_id, 1);
    assert_eq!(message.target_frame_id, 0);
    assert_eq!(message.origin, "https://child.example");
    assert_eq!(message.data_json, r#"{"v":"from-frame-task"}"#);
}
