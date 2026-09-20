use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[test]
fn modal_focus_inertness_and_cancelable_escape_share_the_input_contract() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<button id='outside'>outside</button><dialog id='a'><input id='first'><button id='last'>last</button></dialog><dialog id='b'><button id='nested'>nested</button></dialog>"));
    rt.run_page_init();
    assert_eq!(rt.evaluate(r#"(() => {
        const a = document.getElementById('a'), b = document.getElementById('b');
        const outside = document.getElementById('outside');
        outside.focus(); a.showModal(); a.showModal();
        const initial = document.activeElement.id;
        outside.focus();
        const blockedFocus = document.activeElement.id;
        const blockedInput = !__obscura_inputAllowed(outside);
        __obscura_keyDefault(new KeyboardEvent('keydown', {key:'Tab', shiftKey:true}));
        const reverse = document.activeElement.id;
        b.showModal(); b.close();
        const restoredNested = document.activeElement.id;
        let cancels = 0;
        const prevent = e => { cancels++; e.preventDefault(); };
        a.addEventListener('cancel', prevent);
        __obscura_keyDefault(new KeyboardEvent('keydown', {key:'Escape'}));
        const canceled = a.open;
        a.removeEventListener('cancel', prevent);
        const key = new KeyboardEvent('keydown', {key:'Escape', cancelable:true});
        key.preventDefault(); __obscura_keyDefault(key);
        const keyCanceled = a.open;
        __obscura_keyDefault(new KeyboardEvent('keydown', {key:'Escape'}));
        const closed = !a.open && document.activeElement === outside;
        a.show(); __obscura_keyDefault(new KeyboardEvent('keydown', {key:'Escape'}));
        const nonmodal = a.open;
        a.close(); a.showModal(); a.remove(); outside.focus();
        const removed = document.activeElement === outside;
        return [initial, blockedFocus, blockedInput, reverse, restoredNested,
            cancels, canceled, keyCanceled, closed, nonmodal, removed];
    })()"#).unwrap(), json!(["first", "first", true, "last", "last", 1, true, true, true, true, true]));
}
