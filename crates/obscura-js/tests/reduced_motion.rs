use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[tokio::test]
async fn reduced_motion_updates_queries_and_change_events() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<body></body>"));
    rt.run_page_init();
    rt.evaluate("(() => { globalThis.q = matchMedia('(prefers-reduced-motion: reduce)'); globalThis.changes = []; q.onchange = e => changes.push(e.matches); })()").unwrap();
    rt.set_reduced_motion(true);
    rt.run_event_loop_bounded(30).await.unwrap();
    rt.set_reduced_motion(false);
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(rt.evaluate("[q.matches, changes]").unwrap(), json!([false, [true, false]]));
}

#[cfg(feature = "render")]
#[test]
fn reduced_motion_invalidates_css_geometry() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<body></body>"));
    rt.run_page_init();
    rt.evaluate(r#"(() => {
        const html = '<style>div{width:100px}@media(prefers-reduced-motion:reduce){div{width:40px}}</style><div>probe</div>';
        document.body.innerHTML = html;
        globalThis.widths = () => [document.querySelector('div').getBoundingClientRect().width];
    })()"#).unwrap();
    assert_eq!(rt.evaluate("widths()").unwrap(), json!([100]));
    rt.set_reduced_motion(true);
    assert_eq!(rt.evaluate("widths()").unwrap(), json!([40]));
    rt.set_reduced_motion(false);
    assert_eq!(rt.evaluate("widths()").unwrap(), json!([100]));
}
