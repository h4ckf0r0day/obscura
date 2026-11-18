use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[cfg(feature = "render")]
#[test]
fn computed_insets_use_css_offsets_and_refresh_live_declarations() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_viewport(800.0, 600.0);
    rt.set_dom(obscura_dom::parse_html(r#"<!doctype html><style>
        body{margin:0} #parent{position:relative;width:400px;height:200px;
        margin:27px;padding:13px;border:3px solid black}
        .box{width:40px;height:20px}
        #decl{top:5px;left:10%} #rel{position:relative;top:5px;left:7px}
        #end{position:relative;bottom:7px;right:9px}
        #both{position:relative;top:5px;bottom:7px;left:9px;right:11px}
        #abs{position:absolute;top:10px;left:12px;margin:3px 5px 7px 9px}
        #fixed{position:fixed;top:10px;left:12px}
        #none{display:none;position:relative;top:10%;left:7px}
        #contents{display:contents;position:relative;top:10%;left:7px}
        </style><div id=parent><div id=plain class=box></div>
        <div id=decl class=box></div><div id=rel class=box></div>
        <div id=end class=box></div><div id=both class=box></div>
        <div id=abs class=box></div><div id=fixed class=box></div>
        <div id=none></div><div id=contents></div></div>"#));
    rt.run_page_init();
    assert_eq!(rt.evaluate(r#"(() => {
        globalThis.insets = c => [c.top,c.right,c.bottom,c.getPropertyValue('left')];
        globalThis.live = getComputedStyle(document.getElementById('rel'));
        return ['plain','decl','rel','end','both','abs','fixed','none','contents']
            .map(id => insets(getComputedStyle(document.getElementById(id))));
    })()"#).unwrap(), json!([
        ["auto","auto","auto","auto"], ["5px","auto","auto","10%"],
        ["5px","-7px","-5px","7px"], ["-7px","9px","7px","-9px"],
        ["5px","11px","7px","9px"], ["10px","360px","186px","12px"],
        ["10px","748px","570px","12px"], ["10%","auto","auto","7px"],
        ["10%","auto","auto","7px"]
    ]));
    assert_eq!(rt.evaluate(r#"(() => {
        document.getElementById('rel').style.top = '9px';
        return insets(live);
    })()"#).unwrap(), json!(["9px","-7px","-9px","7px"]));
    rt.set_viewport(900.0, 700.0);
    assert_eq!(rt.evaluate("insets(getComputedStyle(document.getElementById('fixed')))" ).unwrap(),
        json!(["10px","848px","670px","12px"]));
}

#[cfg(feature = "render")]
#[test]
fn computed_insets_resolve_percentages_against_the_relevant_box() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_viewport(800.0, 600.0);
    rt.set_dom(obscura_dom::parse_html(r#"<!doctype html><style>
        body{margin:0} #parent{position:relative;width:400px;height:200px;
        padding:10px;border:3px solid black;transform:translate(20px,30px)}
        .box{width:40px;height:20px} #rel{position:relative;top:10%;left:10%}
        #abs{position:absolute;top:10%;left:10%}
        #sticky{position:sticky;top:10%;left:10%}
        #static{top:calc(10% + 5px);bottom:calc(2em + 5px);left:2em}
        #scroller{overflow:auto;width:300px;height:100px;padding:10px}
        #nested{position:sticky;top:10%;left:10%}
        </style><div id=parent><div id=rel class=box></div><div id=abs class=box></div>
        <div id=sticky class=box></div><div id=static></div>
        <div id=scroller><div id=nested class=box></div><div style='height:400px'></div></div></div>"#));
    rt.run_page_init();
    assert_eq!(rt.evaluate(r#"(() => {
        const read = id => {
            const c = getComputedStyle(document.getElementById(id));
            return [c.top,c.right,c.bottom,c.left];
        };
        return ['rel','abs','sticky','static','nested'].map(read);
    })()"#).unwrap(), json!([
        ["20px","-40px","-20px","40px"], ["22px","338px","178px","42px"],
        ["60px","auto","auto","80px"], ["calc(10% + 5px)","auto","37px","32px"],
        ["12px","auto","auto","32px"]
    ]));
}

#[cfg(feature = "render")]
#[test]
fn computed_insets_preserve_constraints_inline_values_and_scroll_independence() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_viewport(800.0, 600.0);
    rt.set_dom(obscura_dom::parse_html(include_str!("fixtures/computed_insets.html")));
    rt.run_page_init();
    let read = r#"(() => {
        const out = {};
        for (const e of document.querySelectorAll('.item[id]')) {
            const c = getComputedStyle(e);
            out[e.id] = [c.top,c.right,c.bottom,c.left];
        }
        return out;
    })()"#;
    let expected = json!({
        "ltr":["5px","20px","7px","10px"], "rtl":["5px","20px","7px","10px"],
        "center":["0px","0px","0px","0px"], "startauto":["10px","374px","196px","12px"],
        "endauto":["199px","9px","7px","377px"], "allauto":["13px","373px","193px","13px"],
        "fixedcb":["10px","374px","196px","12px"], "inline":["10%","auto","auto","10%"],
        "inlinepx":["5px","-7px","-5px","7px"], "blockchild":["5px","-40px","-5px","40px"],
        "inlineblock":["5px","-40px","-5px","40px"], "stickyend":["auto","80px","60px","auto"],
        "math":["32px","16px","32px","37px"], "absmath":["32px","354px","174px","32px"]
    });
    let initial = rt.evaluate(read).unwrap();
    eprintln!("INSET_EDGE_ORACLE {initial}");
    assert_eq!(initial, expected);
    rt.evaluate("window.scrollTo(0,1200)").unwrap();
    assert_eq!(rt.evaluate(read).unwrap(), expected);
}

#[cfg(not(feature = "render"))]
#[test]
fn computed_insets_without_render_preserve_inline_values_and_auto_defaults() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<div id=plain></div><div id=decl style='top:5px;left:10%'></div>"));
    rt.run_page_init();
    assert_eq!(rt.evaluate(r#"['plain','decl'].map(id => {
        const c = getComputedStyle(document.getElementById(id));
        return [c.top,c.right,c.bottom,c.left];
    })"#).unwrap(), json!([["auto","auto","auto","auto"],["5px","auto","auto","10%"]]));
}
