use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime(html: &str) -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(html));
    rt.set_url("http://example.com/dir");
    rt.run_page_init();
    rt
}

#[test]
fn document_dir_reflects_the_html_element() {
    let mut rt = runtime(r#"<html dir="RTL"><head></head><body></body></html>"#);
    assert_eq!(rt.evaluate(r#"(() => {
        const root = document.documentElement, out = [document.dir];
        document.dir = "LTR";
        out.push(document.dir, root.getAttribute("dir"));
        document.dir = "sideways";
        out.push(document.dir, root.getAttribute("dir"));
        const svg = document.implementation.createDocument("http://www.w3.org/2000/svg", "svg", null);
        svg.dir = "rtl";
        out.push(svg.dir, svg.documentElement.getAttribute("dir"));
        const empty = new Document();
        empty.dir = "rtl";
        out.push(empty.dir);
        const detached = document.implementation.createHTMLDocument("");
        detached.dir = "rtl";
        out.push(detached.dir, detached.documentElement.getAttribute("dir"));
        return out;
    })()"#).unwrap(), json!(["rtl", "ltr", "LTR", "", "sideways", "", null, "", "rtl", "rtl"]));
}

#[cfg(feature = "render")]
#[test]
fn computed_direction_follows_the_dir_attribute() {
    let mut rt = runtime(r#"<!doctype html><html dir="rtl"><body>
        <p id="p">x <span id="span">y</span></p>
        <div id="ltr" dir="ltr"><b id="nested">z</b></div></body></html>"#);
    assert_eq!(rt.evaluate(r#"(() => {
        const direction = node => getComputedStyle(node).direction;
        const out = [document.documentElement, document.body]
            .concat(["p", "span", "ltr", "nested"].map(id => document.getElementById(id)))
            .map(direction);
        document.dir = "ltr";
        out.push(direction(document.getElementById("span")));
        return out;
    })()"#).unwrap(), json!(["rtl", "rtl", "rtl", "rtl", "ltr", "ltr", "ltr"]));
}
