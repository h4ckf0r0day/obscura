use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

#[tokio::test]
async fn data_transfer_uses_chromium_defaults_and_defers_string_callbacks() {
    let mut runtime = ObscuraJsRuntime::new();
    runtime.set_dom(obscura_dom::parse_html("<body><input type=file id=upload></body>"));
    runtime.run_page_init();
    let result = runtime.evaluate(r#"(() => {
        const dt = new DataTransfer();
        const originalEffect = dt.effectAllowed;
        const hiddenFields = Object.keys(dt).filter(key => key.startsWith('_'));
        dt.setData('Text', 'first');
        const item = dt.items.item(0);
        const ownItemFields = Object.keys(item);
        const kindWritable = Reflect.set(item, 'kind', 'file');
        const typeWritable = Reflect.set(item, 'type', 'other');
        item._data = 'spoofed';
        dt.setData('text/plain', 'updated');
        const aliases = [dt.getData('TEXT'), dt.getData('text/plain'), dt.items.length,
          item.kind, item.type, dt.getData('text'), kindWritable, typeWritable];
        dt.setData('URL', 'https://example.com/');
        const urlAlias = dt.getData('text/uri-list');
        const upload = document.getElementById('upload');
        const file = new File(['bytes'], 'example.txt', { type: 'text/plain' });
        dt.items.add(file);
        const live = dt.files;
        upload.files = live;
        const before = [live.length, upload.files === live];
        dt.items.remove(2);
        const after = live.length;
        globalThis.dataTransferCallbacks = [];
        item.getAsString(value => dataTransferCallbacks.push('task:' + value));
        Promise.resolve().then(() => dataTransferCallbacks.push('microtask'));
        return [originalEffect, hiddenFields, ownItemFields, aliases, urlAlias, before, after];
    })()"#).unwrap();
    assert_eq!(result, json!([
        "none", [], [],
        ["updated", "updated", 1, "string", "text/plain", "updated", false, false],
        "https://example.com/", [1, true], 0
    ]));
    runtime.run_event_loop_bounded(100).await.unwrap();
    assert_eq!(runtime.evaluate("dataTransferCallbacks").unwrap(),
        json!(["microtask", "task:updated"]));
}
