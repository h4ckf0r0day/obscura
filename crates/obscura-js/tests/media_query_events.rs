use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[tokio::test]
async fn media_query_list_delivers_changes_and_honors_listener_lifecycle() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<body></body>"));
    rt.set_viewport(1280.0, 720.0);
    rt.run_page_init();
    rt.evaluate(r#"(() => {
        globalThis.events = [];
        globalThis.q = matchMedia('(min-width: 1200px)');
        globalThis.legacy = e => events.push(['legacy', e.matches, e.target === q]);
        q.addListener(legacy);
        q.addListener(legacy);
        q.addEventListener('change', e => events.push(['once', e.matches]), {once: true});
        q.onchange = e => events.push(['property', e.matches]);
        const ac = new AbortController();
        q.addEventListener('change', () => events.push(['aborted']), {signal: ac.signal});
        ac.abort();
        return null;
    })()"#).unwrap();
    rt.set_viewport(1000.0, 700.0);
    assert_eq!(rt.evaluate("[q.matches, events.length]").unwrap(), json!([false, 0]));
    rt.run_event_loop_bounded(30).await.unwrap();
    rt.evaluate("(() => { q.removeListener(legacy); q.onchange = null; })()").unwrap();
    rt.set_viewport(1280.0, 720.0);
    rt.run_event_loop_bounded(30).await.unwrap();
    assert_eq!(rt.evaluate("events").unwrap(), json!([
        ["legacy", false, true], ["once", false], ["property", false]
    ]));
    assert_eq!(rt.evaluate("[q instanceof MediaQueryList, q instanceof EventTarget, q.matches]").unwrap(), json!([true, true, true]));
    assert_eq!(rt.evaluate("(() => { try { new MediaQueryList(); } catch(e) { return e.name; } })()").unwrap(), json!("TypeError"));
}

#[test]
fn media_query_property_handler_is_independent_and_keeps_listener_order() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<body></body>"));
    rt.run_page_init();
    let result = rt.evaluate(r#"(() => {
        const q = matchMedia('(min-width: 1px)'), seen = [], results = [];
        const shared = () => seen.push('shared');
        const emit = () => {
            seen.length = 0;
            q.dispatchEvent(new MediaQueryListEvent('change'));
            results.push(seen.slice());
        };
        q.addEventListener('change', shared);
        q.onchange = shared;
        q.addEventListener('change', () => seen.push('last'));
        emit();
        q.onchange = () => seen.push('replacement');
        emit();
        q.onchange = null;
        emit();
        q.onchange = shared;
        emit();
        q.removeEventListener('change', shared);
        emit();
        return results;
    })()"#).unwrap();
    assert_eq!(result, json!([
        ["shared", "shared", "last"],
        ["shared", "replacement", "last"],
        ["shared", "last"],
        ["shared", "last", "shared"],
        ["last", "shared"]
    ]));
}
