use obscura_dom::parse_html;
use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn setup(html: &str) -> ObscuraJsRuntime {
    let dom = parse_html(html);
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(dom);
    rt.set_url("http://example.com/test");
    rt.set_title("Test Page");
    rt.run_page_init();
    rt
}

#[tokio::test]
async fn srcdoc_iframe_created_via_innerhtml_does_not_abort() {
    let mut rt = setup("<html><body><div id=host></div></body></html>");
    rt.evaluate(r#"host.innerHTML = '<iframe srcdoc="<p>frame</p>"></iframe>'"#)
        .unwrap();
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate(
            r#"(() => {
                const frame = host.querySelector('iframe');
                return [!!frame, frame.contentDocument.body.innerHTML];
            })()"#
        )
        .unwrap(),
        json!([true, "<p>frame</p>"])
    );
}

#[tokio::test]
async fn srcdoc_iframe_created_via_property_then_append_does_not_abort() {
    let mut rt = setup("<html><body></body></html>");
    rt.evaluate(
        r#"(() => {
            const frame = document.createElement('iframe');
            frame.srcdoc = '<p>prop</p>';
            document.body.appendChild(frame);
        })()"#,
    )
    .unwrap();
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate(
            r#"(() => {
                const frame = document.querySelector('iframe');
                return frame.contentDocument.body.innerHTML;
            })()"#
        )
        .unwrap(),
        json!("<p>prop</p>")
    );
}

#[tokio::test]
async fn srcdoc_iframe_with_synchronous_dom_touch_does_not_abort() {
    let mut rt = setup("<html><body></body></html>");
    rt.evaluate(
        r#"(() => {
            const frame = document.createElement('iframe');
            frame.srcdoc = '<body><span>inner</span></body>';
            document.body.appendChild(frame);
        })()"#,
    )
    .unwrap();
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate(
            r#"(() => {
                const frame = document.querySelector('iframe');
                const doc = frame.contentDocument;
                const span = doc.querySelector('span');
                span.textContent = 'touched';
                return doc.body.textContent;
            })()"#
        )
        .unwrap(),
        json!("touched")
    );
}
