use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn set_range_text_matches_chromium_for_empty_and_nonempty_controls() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(
        r#"<textarea id="t"></textarea><input id="i"><input id="n" type="number">"#,
    ));
    rt.run_page_init();
    let value = rt.evaluate(r#"(() => {
        const t = document.getElementById('t');
        const out = {};
        const run = (name, initial, selection, replacement, args) => {
            t.value = initial;
            t.setSelectionRange(...selection);
            try {
                t.setRangeText(replacement, ...args);
                out[name] = [t.value, t.selectionStart, t.selectionEnd, t.selectionDirection];
            } catch (e) { out[name] = e.name; }
        };
        run('empty', '', [0, 0], 'a', []);
        run('before', 'abcdef', [0, 0], 'XYZ', [1, 3]);
        run('overlap', 'abcdef', [1, 2], 'XYZ', [1, 3]);
        run('after', 'abcdef', [3, 5], 'XYZ', [1, 3]);
        run('preserve', 'abcdef', [1, 5], 'XYZ', [2, 4, 'preserve']);
        run('undefinedMode', 'abcdef', [1, 5], 'XYZ', [2, 4, undefined]);
        run('select', 'abcdef', [0, 0], 'X', [1, 3, 'select']);
        run('start', 'abcdef', [0, 0], 'X', [1, 3, 'start']);
        run('end', 'abcdef', [0, 0], 'X', [1, 3, 'end']);
        run('invalid', 'abcdef', [0, 0], 'X', [1, 3, 'invalid']);
        run('nullMode', 'abcdef', [0, 0], 'X', [1, 3, null]);
        run('reversed', 'abcdef', [0, 0], 'X', [5, 1]);
        run('beyond', 'abc', [0, 0], 'X', [6, 9]);
        const i = document.getElementById('i');
        i.value = 'abc';
        out.inputAssignment = [i.selectionStart, i.selectionEnd];
        i.setRangeText('X', 1, 2);
        out.input = [i.value, i.selectionStart, i.selectionEnd];
        i.setRangeText('Y', 0, 1, undefined);
        out.inputUndefinedMode = [i.value, i.selectionStart, i.selectionEnd];
        try { document.getElementById('n').setRangeText('X', 1, 2); }
        catch (e) { out.number = e.name; }
        return out;
    })()"#).unwrap();
    assert_eq!(value, json!({
        "empty": ["a", 0, 0, "forward"],
        "before": ["aXYZdef", 0, 0, "forward"],
        "overlap": ["aXYZdef", 1, 4, "forward"],
        "after": ["aXYZdef", 1, 6, "forward"],
        "preserve": ["abXYZef", 1, 6, "forward"],
        "undefinedMode": ["abXYZef", 1, 6, "forward"],
        "select": ["aXdef", 1, 2, "forward"],
        "start": ["aXdef", 1, 1, "forward"],
        "end": ["aXdef", 2, 2, "forward"],
        "invalid": "TypeError",
        "nullMode": "TypeError",
        "reversed": "IndexSizeError",
        "beyond": ["abcX", 0, 0, "forward"],
        "input": ["aXc", 3, 3],
        "inputUndefinedMode": ["YXc", 3, 3],
        "inputAssignment": [3, 3],
        "number": "InvalidStateError"
    }));
}
