use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn cdp_file_selection_honors_multiple_attribute() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(
        r#"<body><input id="single" type="file"><input id="multi" type="file" multiple></body>"#,
    ));
    rt.run_page_init();
    let result = rt.evaluate(r#"(() => {
        const files = [
            {name:'first.png', type:'image/png', b64:'AP8='},
            {name:'second.png', type:'image/png', b64:'AQI='}
        ];
        const summary = {};
        for (const name of ['single', 'multi']) {
            const input = document.getElementById(name);
            const events = [];
            input.addEventListener('input', e => events.push(e.type));
            input.addEventListener('change', e => events.push(e.type));
            __obscura_setInputFiles(input, files);
            summary[name] = {
                count: input.files.length,
                names: [...input.files].map(file => file.name),
                events
            };
        }
        return summary;
    })()"#).unwrap();
    assert_eq!(result, json!({
        "single": {
            "count": 1,
            "names": ["first.png"],
            "events": ["input", "change"]
        },
        "multi": {
            "count": 2,
            "names": ["first.png", "second.png"],
            "events": ["input", "change"]
        }
    }));
}
