use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn eof_script_moves_stay_inert_while_explicit_copies_can_execute() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(
        "<!doctype html><body><script id=eof data-public=kept>globalThis.runs++;",
    ));
    rt.run_page_init();
    assert_eq!(rt.evaluate(r#"(() => {
        globalThis.runs=0;
        const script=document.getElementById('eof');
        const html=script.outerHTML,steps=[];
        script.remove();document.head.appendChild(script);steps.push(runs);
        const clone=script.cloneNode(true);clone.id='clone';document.body.appendChild(clone);steps.push(runs);
        const imported=document.importNode(script,true);imported.id='imported';document.body.appendChild(imported);steps.push(runs);
        document.implementation.createHTMLDocument('other').adoptNode(script);
        document.body.appendChild(document.adoptNode(script));steps.push(runs);
        script.textContent='globalThis.runs+=10;';
        script.remove();document.body.appendChild(script);steps.push(runs);
        const fresh=document.createElement('script');fresh.textContent='globalThis.runs++;';
        document.body.appendChild(fresh);steps.push(runs);
        return [steps,html,
            script.getAttributeNames().sort()];
    })()"#).unwrap(), json!([[0, 1, 2, 2, 2, 3], "<script id=\"eof\" data-public=\"kept\">globalThis.runs++;</script>", ["data-public", "id"]]));

    let dom = rt.take_dom().unwrap();
    drop(rt);
    let mut restored = ObscuraJsRuntime::new();
    restored.set_dom(dom);
    restored.run_page_init();
    assert_eq!(restored.evaluate(r#"(() => {
        globalThis.runs=0;
        const script=document.getElementById('eof');
        script.remove();document.head.appendChild(script);
        return runs;
    })()"#).unwrap(), json!(0.0));
}

#[test]
fn eof_fragment_scripts_stay_inert_without_disabling_complete_scripts() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html><body><div id=context></div>"));
    rt.run_page_init();
    assert_eq!(rt.evaluate(r#"(() => {
        globalThis.runs=[];
        const range=document.createRange();range.selectNode(document.getElementById('context'));
        for(const complete of [true,false]) {
            document.body.appendChild(range.createContextualFragment(
                '<script>runs.push("range-'+complete+'");'+(complete?'</script>':'')));
        }
        const inert=document.createElement('div');
        inert.innerHTML='<script>runs.push("inner")';document.body.appendChild(inert.firstChild);
        document.write('<script>runs.push("write")</script>');
        document.write('<script>runs.push("write-eof")');document.close();
        return runs;
    })()"#).unwrap(), json!(["range-true", "write"]));
}

#[test]
fn script_fragment_provenance_controls_explicit_copies() {
    for (kind, expected) in [
        ("range-eof", json!([0, [0, 0, 0]])),
        ("range-complete", json!([1, [0, 0, 0]])),
        ("inner-eof", json!([0, [0, 0, 0]])),
        ("inner-complete", json!([0, [0, 0, 0]])),
        ("write-eof", json!([0, [0, 1, 2]])),
        ("write-complete", json!([1, [0, 0, 0]])),
    ] {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(obscura_dom::parse_html("<!doctype html><body><div id=context></div>"));
        rt.run_page_init();
        rt.evaluate(&format!(r#"(kind => {{
            globalThis.runs=0;
            const code='<script id=probe>globalThis.runs=(globalThis.runs||0)+1;';
            const fragmentCode=code+(kind.endsWith('complete')?'</script>':'');
            if(kind.startsWith('range-')) {{
                const range=document.createRange();range.selectNode(document.getElementById('context'));
                document.body.appendChild(range.createContextualFragment(fragmentCode));
            }} else if(kind.startsWith('inner-')) {{
                const div=document.createElement('div');div.innerHTML=fragmentCode;document.body.appendChild(div.firstChild);
            }} else {{
                document.open();globalThis.runs=0;
                document.write('<!doctype html><body>'+fragmentCode);document.close();
            }}
        }})({kind:?})"#)).unwrap();
        assert_eq!(rt.evaluate(r#"(() => {
            const initially=globalThis.runs||0;globalThis.runs=0;
            const script=document.getElementById('probe'),steps=[];
            script.remove();document.head.appendChild(script);steps.push(runs);
            document.body.appendChild(script.cloneNode(true));steps.push(runs);
            document.body.appendChild(document.importNode(script,true));steps.push(runs);
            return [initially,steps];
        })()"#).unwrap(), expected, "{kind}");
    }
}
