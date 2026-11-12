use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(
        r#"<body><button id="b" onclick="probe()">x</button></body>"#,
    ));
    rt.run_page_init();
    rt
}

#[test]
fn global_event_is_visible_to_functions_called_from_inline_handlers_and_listeners() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        const out = {};
        globalThis.probe = function () {
            out.free = [typeof event, event.type, event === window.event];
        };
        const b = document.getElementById('b');
        b.addEventListener('click', e => { out.listener = window.event === e; });
        b.click();
        out.after = typeof window.event;
        return out;
    })()"#,
        )
        .unwrap();
    assert_eq!(
        result,
        json!({"free": ["object", "click", true], "listener": true, "after": "undefined"})
    );
}

#[test]
fn nested_dispatch_restores_the_outer_event() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        const b = document.getElementById('b'), seen = [];
        b.addEventListener('ping', () => {
            seen.push(window.event.type);
            b.dispatchEvent(new Event('pong'));
            seen.push(window.event.type);
        });
        b.addEventListener('pong', () => seen.push(window.event.type));
        b.dispatchEvent(new Event('ping'));
        seen.push(typeof window.event);
        return seen;
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!(["ping", "pong", "ping", "undefined"]));
}

#[test]
fn event_is_restored_when_a_listener_throws() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        const b = document.getElementById('b');
        let during;
        b.addEventListener('boom', () => {
            during = typeof window.event;
            throw new Error('listener failure');
        });
        b.dispatchEvent(new Event('boom'));
        return [during, typeof window.event];
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!(["object", "undefined"]));
}

#[test]
fn window_event_is_replaceable() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        const b = document.getElementById('b');
        let during;
        b.addEventListener('x', () => { during = [typeof window.event, window.event === 7]; });
        b.dispatchEvent(new Event('x'));
        const before = during;
        window.event = 7;
        b.dispatchEvent(new Event('x'));
        return [before, during, window.event];
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!([["object", false], ["number", true], 7]));
}

#[test]
fn event_is_not_exposed_to_listeners_inside_a_shadow_tree() {
    let mut rt = runtime();
    let result = rt
        .evaluate(
            r#"(() => {
        const host = document.createElement('div');
        document.body.appendChild(host);
        const root = host.attachShadow({ mode: 'open' });
        const inner = document.createElement('span');
        root.appendChild(inner);
        const seen = {};
        inner.addEventListener('x', () => { seen.shadow = typeof window.event; });
        host.addEventListener('x', () => { seen.light = typeof window.event; });
        inner.dispatchEvent(new Event('x'));
        host.dispatchEvent(new Event('x'));
        return seen;
    })()"#,
        )
        .unwrap();
    assert_eq!(result, json!({"shadow": "undefined", "light": "object"}));
}
