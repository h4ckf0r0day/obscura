use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html><body></body>"));
    rt.run_page_init();
    rt
}

#[tokio::test]
async fn color_scheme_updates_compound_queries_once_and_clears() {
    let mut rt = runtime();
    rt.evaluate(r#"(() => {
        globalThis.q = matchMedia('(prefers-color-scheme: dark) and (prefers-reduced-motion: reduce)');
        globalThis.changes = [];
        q.addListener(e => changes.push(e.matches));
        globalThis.probe = () => ['dark', 'light', 'no-preference'].map(v =>
            matchMedia('(prefers-color-scheme: ' + v + ')').matches);
    })()"#).unwrap();
    assert_eq!(rt.evaluate("probe()").unwrap(), json!([false, true, false]));
    assert_eq!(rt.evaluate("[matchMedia('(prefers-color-scheme)').matches, matchMedia('(prefers-color-scheme:invalid)').matches]").unwrap(), json!([true, false]));
    rt.set_media_preferences(true, true);
    rt.run_event_loop_bounded(30).await.unwrap();
    rt.set_media_preferences(true, true);
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate("[probe(), changes]").unwrap(),
        json!([[true, false, false], [true]])
    );
    rt.set_reduced_motion(false);
    assert_eq!(rt.evaluate("probe()").unwrap(), json!([true, false, false]));
    rt.set_media_preferences(false, false);
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate("[probe(), changes]").unwrap(),
        json!([[false, true, false], [true, false]])
    );
}

#[tokio::test]
async fn color_scheme_reaches_existing_and_new_child_documents() {
    let mut rt = runtime();
    rt.evaluate(r#"(() => {
        const f = document.createElement('iframe'); document.body.appendChild(f);
        globalThis.child = f.contentWindow;
        child.eval("globalThis.q = matchMedia('(prefers-color-scheme: dark)'); globalThis.changes = []; q.onchange = e => changes.push(e.matches)");
    })()"#).unwrap();
    rt.set_media_preferences(true, true);
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate("[child.q.matches, child.changes]").unwrap(),
        json!([true, [true]])
    );
    assert_eq!(rt.evaluate(r#"(() => {
        const f = document.createElement('iframe'); document.body.appendChild(f);
        return f.contentWindow.matchMedia('(prefers-color-scheme: dark) and (prefers-reduced-motion: reduce)').matches;
    })()"#).unwrap(), json!(true));
    rt.set_media_preferences(false, false);
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(
        rt.evaluate("[child.q.matches, child.changes]").unwrap(),
        json!([false, [true, false]])
    );
}

#[cfg(feature = "render")]
#[test]
fn color_scheme_invalidates_live_styles_geometry_and_print_selection() {
    let mut rt = runtime();
    rt.evaluate(r#"(() => {
        document.body.innerHTML = '<style>div{width:100px;color:rgb(1,2,3)}@media(prefers-color-scheme:dark){div{width:40px;color:rgb(4,5,6)}}@media print and (prefers-color-scheme:dark){div{width:60px}}</style><div>probe</div>';
        globalThis.box = document.querySelector('div'); globalThis.live = getComputedStyle(box);
        globalThis.probe = () => [live.color, live.width, box.getBoundingClientRect().width];
    })()"#).unwrap();
    assert_eq!(
        rt.evaluate("probe()").unwrap(),
        json!(["rgb(1, 2, 3)", "100px", 100])
    );
    rt.set_media_preferences(true, true);
    assert_eq!(
        rt.evaluate("probe()").unwrap(),
        json!(["rgb(4, 5, 6)", "40px", 40])
    );
    let previous = rt.set_render_media(
        obscura_js::CssMediaType::Print
            .with_reduced_motion(true)
            .with_dark_color_scheme(true),
    );
    assert_eq!(
        rt.evaluate("box.getBoundingClientRect().width").unwrap(),
        json!(60.0)
    );
    rt.set_render_media(previous);
    rt.set_media_preferences(false, false);
    assert_eq!(
        rt.evaluate("probe()").unwrap(),
        json!(["rgb(1, 2, 3)", "100px", 100])
    );
}
