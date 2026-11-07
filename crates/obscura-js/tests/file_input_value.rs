use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn file_input_value_rejects_paths_and_clears_selection_without_events() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<input id=f type=file multiple>"));
    rt.run_page_init();
    let result = rt.evaluate(r#"(() => {
        const input = document.getElementById('f');
        const events = [];
        input.addEventListener('input', () => events.push('input'));
        input.addEventListener('change', () => events.push('change'));
        const files = [
            { name: 'one.txt', type: 'text/plain', b64: 'YQ==' },
            { name: 'two.txt', type: 'text/plain', b64: 'Yg==' }
        ];
        __obscura_setInputFiles(input, files);
        const before = [input.value, input.files.length];
        events.length = 0;
        let error;
        try { input.value = '/etc/passwd'; } catch (err) { error = err.name; }
        const rejected = [input.value, input.files.length, events.slice()];
        input.value = '';
        const empty = [input.value, input.files.length, events.slice()];
        __obscura_setInputFiles(input, files);
        events.length = 0;
        input.value = null;
        const viaNull = [input.value, input.files.length, events.slice()];
        return { before, error, rejected, empty, viaNull };
    })()"#).unwrap();
    assert_eq!(result, json!({
        "before": ["C:\\fakepath\\one.txt", 2],
        "error": "InvalidStateError",
        "rejected": ["C:\\fakepath\\one.txt", 2, []],
        "empty": ["", 0, []],
        "viaNull": ["", 0, []]
    }));
}
