use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<body></body>"));
    rt.run_page_init();
    rt
}

#[test]
fn xml_document_factory_creates_empty_and_namespaced_detached_documents() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const empty = document.implementation.createDocument(null, '', null);
        const plain = document.implementation.createDocument(null, 'Root', null);
        const doc = document.implementation.createDocument('urn:root', 'p:Root', null);
        const root = doc.documentElement;
        const child = doc.createElementNS('urn:child', 'Mixed');
        const text = doc.createTextNode('Loaded');
        child.appendChild(text);
        root.appendChild(child);
        return [empty.documentElement, empty.childNodes.length, empty.contentType, empty.defaultView,
            plain.documentElement.nodeName, plain.documentElement.namespaceURI, plain.documentElement.ownerDocument === plain,
            root.nodeName, root.localName, root.prefix, root.namespaceURI, root.ownerDocument === doc,
            root.parentNode === doc, child.ownerDocument === doc, text.ownerDocument === doc,
            root.textContent, doc.querySelector('Mixed') === child, doc.querySelector('mixed'),
            doc.defaultView, document.querySelector('Mixed')];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            null,
            0,
            "application/xml",
            null,
            "Root",
            null,
            true,
            "p:Root",
            "Root",
            "p",
            "urn:root",
            true,
            true,
            true,
            true,
            "Loaded",
            true,
            null,
            null,
            null
        ])
    );
}

#[test]
fn xml_document_serializes_case_sensitively_and_clones_by_native_tree() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const doc = document.implementation.createDocument('urn:ns', 'Root', null);
        const root = doc.documentElement;
        root.setAttribute('Mixed-Case', 'v');
        root.appendChild(doc.createElement('Child'));
        root.appendChild(doc.createCDATASection('raw<data>'));
        const xml = new XMLSerializer().serializeToString(doc);
        const clone = doc.cloneNode(true);
        return [xml, clone === doc, clone.documentElement === doc.documentElement,
            clone.documentElement.tagName, clone.documentElement.childNodes.length,
            clone.documentElement.getAttribute('Mixed-Case')];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            "<Root Mixed-Case=\"v\"><Child/><![CDATA[raw<data>]]></Root>",
            false,
            false,
            "Root",
            2,
            "v"
        ])
    );
}
