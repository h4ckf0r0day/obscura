use obscura_js::{frame::FrameRealm, runtime::ObscuraJsRuntime};
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html><html><body></body></html>"));
    rt.set_url("https://example.com/");
    rt.run_page_init();
    rt
}

#[test]
fn browser_interface_globals_match_their_instances_and_reject_construction() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const instances = {Navigator:navigator, PluginArray:navigator.plugins,
            MimeTypeArray:navigator.mimeTypes, Plugin:navigator.plugins[0],
            MimeType:navigator.mimeTypes[0], Location:location, Performance:performance};
        return Object.entries(instances).map(([name, instance]) => {
            const C = globalThis[name];
            const d = Object.getOwnPropertyDescriptor(globalThis, name);
            let error;
            try { new C(); } catch (e) { error = e.name; }
            return [typeof C, C.length, error, instance instanceof C,
                Object.getPrototypeOf(instance) === C.prototype,
                Object.prototype.toString.call(instance) === `[object ${name}]`,
                d.writable, d.configurable, d.enumerable];
        });
    })()"#).unwrap();
    assert_eq!(result, json!(vec![json!(["function", 0, "TypeError", true, true, true, true, true, false]); 7]));
    assert_eq!(rt.evaluate("[performance instanceof EventTarget, Array.isArray(navigator.plugins), navigator.plugins.length, navigator.mimeTypes.length]").unwrap(), json!([true, false, 5, 2]));
}

#[test]
fn browser_rect_interfaces_preserve_readonly_state_and_live_negative_edges() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const r = new DOMRectReadOnly(10, 20, -5, -7);
        const original = r.toJSON();
        const writable = Reflect.set(r, 'x', 90);
        const m = DOMRect.fromRect(r);
        m.x = 30; m.width = -12;
        let illegalReceiver;
        try { Object.getOwnPropertyDescriptor(DOMRectReadOnly.prototype, 'x').get.call({}); }
        catch (e) { illegalReceiver = e.name; }
        return {original, writable, unchanged:r.x, edges:[m.left,m.right,m.top,m.bottom],
            mutable:m instanceof DOMRect && m instanceof DOMRectReadOnly,
            immutable:r instanceof DOMRectReadOnly && !(r instanceof DOMRect),
            empty:DOMRectReadOnly.fromRect(null).toJSON(), illegalReceiver};
    })()"#).unwrap();
    assert_eq!(result, json!({
        "original":{"x":10,"y":20,"width":-5,"height":-7,"top":13,"right":10,"bottom":20,"left":5},
        "writable":false,"unchanged":10,"edges":[18,30,13,20],"mutable":true,"immutable":true,
        "empty":{"x":0,"y":0,"width":0,"height":0,"top":0,"right":0,"bottom":0,"left":0},
        "illegalReceiver":"TypeError"
    }));
}

#[test]
fn browser_interfaces_belong_to_each_native_frame_realm() {
    let mut rt = runtime();
    let _frame = FrameRealm::new(&mut rt, 1, 0, "https://example.com/frame", "<!doctype html><body></body>").unwrap();
    let result = rt.evaluate(r#"(() => {
        const child = globalThis.__obscura_frameObjects[1].window;
        const names = ['Navigator','PluginArray','MimeTypeArray','Location','Performance','DOMRectReadOnly'];
        return {
            separate:names.every(n => typeof child[n] === 'function' && child[n] !== globalThis[n]),
            instances:child.navigator instanceof child.Navigator &&
                child.navigator.plugins instanceof child.PluginArray &&
                child.navigator.mimeTypes instanceof child.MimeTypeArray &&
                child.location instanceof child.Location && child.performance instanceof child.Performance,
            parentInstance:child.navigator instanceof Navigator,
            rect:new child.DOMRect(1,2,3,4) instanceof child.DOMRectReadOnly
        };
    })()"#).unwrap();
    assert_eq!(result, json!({"separate":true,"instances":true,"parentInstance":false,"rect":true}));
}

#[test]
fn synchronous_iframe_browser_interfaces_have_matching_child_instances() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const frame = document.createElement('iframe'); document.body.appendChild(frame);
        const w = frame.contentWindow;
        const instances = {Navigator:w.navigator, PluginArray:w.navigator.plugins,
            MimeTypeArray:w.navigator.mimeTypes, Plugin:w.navigator.plugins[0],
            MimeType:w.navigator.mimeTypes[0], Location:w.location, Performance:w.performance};
        return {
            types:Object.entries(instances).map(([name, value]) => [
                value instanceof w[name], value instanceof globalThis[name],
                Object.getPrototypeOf(value) === w[name].prototype]),
            stable:w.navigator.plugins === w.navigator.plugins,
            inherited:w.performance instanceof w.EventTarget,
            rect:new w.DOMRect(1,2,3,4) instanceof w.DOMRectReadOnly,
            identity:w.navigator.userAgent === navigator.userAgent,
            plugins:[...w.navigator.plugins].map(p=>p.name)
                .join('|') === [...navigator.plugins].map(p=>p.name).join('|')
        };
    })()"#).unwrap();
    assert_eq!(result, json!({"types":vec![json!([true,false,true]);7],
        "stable":true,"inherited":true,"rect":true,"identity":true,"plugins":true}));
}

#[test]
fn synchronous_iframe_rectangle_factories_return_child_realm_instances() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const f = document.createElement('iframe'); document.body.appendChild(f);
        const w = f.contentWindow;
        return ['DOMRect', 'DOMRectReadOnly'].map(name => {
            const rect = w[name].fromRect({x:10, y:20, width:-5, height:-7});
            return [rect instanceof w[name], rect instanceof globalThis[name],
                rect.left, rect.bottom];
        });
    })()"#).unwrap();
    assert_eq!(result, json!([[true, false, 5, 20], [true, false, 5, 20]]));
}
