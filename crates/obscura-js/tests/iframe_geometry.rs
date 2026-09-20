#![cfg(feature = "render")]

use obscura_js::{frame::FrameRealm, runtime::ObscuraJsRuntime};
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html><body></body>"));
    rt.set_url("https://example.com/");
    rt.run_page_init();
    rt
}

#[test]
fn synchronous_iframe_rectangles_use_child_styles_and_viewport() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        document.body.innerHTML = '<style>div { width:777px }</style><div id="parent"></div><iframe style="width:300px;height:150px"></iframe>';
        const f = document.querySelector('iframe'), d = f.contentDocument;
        d.body.innerHTML = '<style>body{margin:8px}div{width:100px;height:20px}</style><div id="child"></div>';
        const el = d.getElementById('child'), r = d.createRange();
        r.selectNode(el);
        const a = el.getClientRects()[0];
        const initial = [a.x,a.y,a.width,a.height,a.bottom,r.getClientRects()[0].bottom,
            f.contentWindow.getComputedStyle(el).width,document.getElementById('parent').getBoundingClientRect().width];
        el.style.width = '50%';
        const percent = el.getBoundingClientRect().width;
        f.style.width = '500px';
        const resized = el.getBoundingClientRect().width;
        el.style.display = 'none';
        const hidden = el.getClientRects().length;
        el.style.display = 'block';
        f.style.display = 'none';
        const hiddenFrame = el.getClientRects().length;
        f.style.display = 'block';
        f.remove();
        return {initial,percent,resized,hidden,hiddenFrame,removed:el.getClientRects().length};
    })()"#).unwrap();
    assert_eq!(result, json!({"initial":[8,8,100,20,28,28,"100px",777],
        "percent":142,"resized":242,"hidden":0,"hiddenFrame":0,"removed":0}));
}

#[test]
fn nested_iframes_and_adopted_nodes_receive_boxes_without_layout_for_detached_elements() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const f = document.createElement('iframe'); document.body.appendChild(f);
        const nested = f.contentDocument.createElement('iframe');
        f.contentDocument.body.appendChild(nested);
        const d = nested.contentDocument;
        const el = document.createElement('div');
        el.style.cssText = 'width:100px;height:20px;transform:translate(5px,7px)';
        const detached = el.getClientRects().length;
        d.body.appendChild(el);
        const a = el.getClientRects()[0];
        const visible = [a.x,a.y,a.width,a.height,a.bottom];
        el.remove();
        return {detached,visible,removed:el.getClientRects().length};
    })()"#).unwrap();
    assert_eq!(result, json!({"detached":0,"visible":[13,15,100,20,35],"removed":0}));
}

#[test]
fn native_frame_geometry_uses_the_frame_dom_instead_of_colliding_parent_node_ids() {
    let mut rt = runtime();
    rt.evaluate("document.body.innerHTML = '<div style=\"width:900px;height:60px\"></div>'").unwrap();
    let _frame = FrameRealm::new(&mut rt, 1, 0, "https://example.com/frame",
        "<!doctype html><body><div style='width:123px;height:45px'></div></body>").unwrap();
    let result = rt.evaluate(r#"(() => {
        const w = globalThis.__obscura_frameObjects[1].window;
        const el = w.document.querySelector('div'), r = el.getClientRects()[0];
        return [r.width,r.height,w.getComputedStyle(el).width];
    })()"#).unwrap();
    assert_eq!(result, json!([123,45,"123px"]));
}

#[test]
fn measured_iframe_resources_join_native_warmup_and_disappear_after_removal() {
    let mut rt = runtime();
    rt.evaluate(r#"(() => {
        const f = document.createElement('iframe'); f.id='frame'; document.body.appendChild(f);
        const d = f.contentDocument;
        d.body.innerHTML = '<style>@font-face{font-family:Child;src:url(child.woff2)}</style><img src="https://example.com/child.png"><div style="width:100px;height:20px"></div>';
        d.querySelector('div').getBoundingClientRect();
    })()"#).unwrap();
    assert!(rt.render_css_sources().iter().any(|(css, base)|
        css.contains("child.woff2") && base == "https://example.com/"));
    assert!(rt.pending_render_image_urls().iter().any(|(url, _)| url == "https://example.com/child.png"));
    rt.evaluate("document.getElementById('frame').remove()").unwrap();
    assert!(!rt.render_css_sources().iter().any(|(css, _)| css.contains("child.woff2")));
    assert!(rt.pending_render_image_urls().is_empty());
}
