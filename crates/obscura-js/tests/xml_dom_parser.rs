use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::json;

fn runtime() -> ObscuraJsRuntime {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html(
        "<body><main id='live'>Original page</main></body>",
    ));
    rt.run_page_init();
    rt
}

#[test]
fn xml_dom_parser_preserves_root_for_each_xml_mime_type() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const parser = new DOMParser();
        return ['application/xml', 'text/xml', 'application/xhtml+xml', 'image/svg+xml'].map(type => {
            const doc = parser.parseFromString('<Root xmlns="urn:root"><Item/></Root>', type);
            const root = doc.documentElement;
            return [doc.contentType, root.nodeName, root.tagName, root.localName,
                root.namespaceURI, root.prefix, root.children[0].nodeName,
                doc.querySelector('Root') === root, doc.querySelector('Item') !== null,
                doc.querySelector('item') === null, doc.body, doc.head];
        });
    })()"#).unwrap();
    let expected = [
        "application/xml",
        "text/xml",
        "application/xhtml+xml",
        "image/svg+xml",
    ]
    .map(|mime| {
        json!([
            mime, "Root", "Root", "Root", "urn:root", null, "Item", true, true, true, null, null
        ])
    });
    assert_eq!(result, json!(expected));
}

#[test]
fn xml_dom_parser_resolves_inherited_redeclared_and_reset_namespaces() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const doc = new DOMParser().parseFromString(
            '<p:Root xmlns="urn:default" xmlns:p="urn:outer" xmlns:a="urn:attrs" Plain="one" a:Code="two">' +
            '<Child/><p:Child xmlns:p="urn:inner"/><Empty xmlns=""><Leaf/></Empty></p:Root>', 'application/xml');
        const root = doc.documentElement;
        const describe = node => [node.nodeName, node.localName, node.prefix, node.namespaceURI];
        return [describe(root), ...Array.from(root.children, describe), describe(root.children[2].firstElementChild),
            root.getAttribute('Plain'), root.getAttribute('plain'),
            root.getAttributeNS('urn:attrs', 'Code'), root.getAttributeNS(null, 'Plain'),
            root.getAttributeNS('http://www.w3.org/2000/xmlns/', 'p'),
            root.getElementsByTagName('p:Child').length, root.getElementsByTagName('Child').length,
            root.getElementsByTagName('p:child').length, root.getElementsByTagName('P:Child').length,
            root.getElementsByTagName('*').length, root.getElementsByTagName('p:Root').length,
            doc.getElementsByTagName('p:Root').length];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            ["p:Root", "Root", "p", "urn:outer"],
            ["Child", "Child", null, "urn:default"],
            ["p:Child", "Child", "p", "urn:inner"],
            ["Empty", "Empty", null, null],
            ["Leaf", "Leaf", null, null],
            "one",
            null,
            "two",
            "one",
            "urn:outer",
            1,
            1,
            0,
            0,
            4,
            0,
            1
        ])
    );
}

#[test]
fn xml_dom_parser_preserves_text_entities_and_non_element_nodes() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const doc = new DOMParser().parseFromString(
            '<?xml version="1.0"?><Root Value="&quot;&apos;&amp;&lt;&gt;&#65;&#x1F600;">' +
            'A&amp;B&#32;&#x41;<![CDATA[<Item>&literal;]]><!--kept--><?work ready?><Empty/></Root>', 'text/xml');
        const root = doc.documentElement;
        return [root.getAttribute('Value'), root.textContent,
            Array.from(root.childNodes, n => [n.nodeType, n.nodeName, n.nodeValue]),
            root.children.length, root.firstElementChild.nodeName];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            "\"'&<>A😀",
            "A&B A<Item>&literal;",
            [
                [3, "#text", "A&B A"],
                [4, "#cdata-section", "<Item>&literal;"],
                [8, "#comment", "kept"],
                [7, "work", "ready"],
                [1, "Empty", null]
            ],
            1,
            "Empty"
        ])
    );
}

#[test]
fn xml_dom_parser_documents_are_detached_and_own_their_nodes() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const doc = new DOMParser().parseFromString('<Catalog><Item id="found">Read me</Item></Catalog>', 'application/xml');
        const root = doc.documentElement, item = doc.getElementById('found');
        return [root.ownerDocument === doc, item.ownerDocument === doc,
            item.firstChild.ownerDocument === doc, root.parentNode === doc,
            root.isConnected, doc.defaultView, doc.ownerDocument, doc.URL,
            doc.childNodes.length, doc.firstChild === root, doc.querySelector('Catalog') === root,
            doc.querySelectorAll('Item').length, doc.getElementsByTagName('Item').length,
            document.getElementById('found'), document.getElementById('live').textContent];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            true,
            true,
            true,
            true,
            true,
            null,
            null,
            "about:blank",
            1,
            true,
            true,
            1,
            1,
            null,
            "Original page"
        ])
    );
}

#[test]
fn xml_dom_parser_malformed_input_returns_parsererror_without_changing_live_page() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const parser = new DOMParser();
        const invalid = ['', '<Root>', '<Root><Item></Root>', '<One/><Two/>',
            '<Root><!--unfinished</Root>', '<Root><![CDATA[unfinished</Root>', '<Root><?unfinished</Root>',
            '<Root A="one" A="two"/>', '<Root>&unknown;</Root>', '<p:Root/>', '<Root A=one/>'];
        return [invalid.map(source => {
            const doc = parser.parseFromString(source, 'application/xml');
            return doc.querySelector('parsererror') !== null;
        }), document.getElementById('live').textContent];
    })()"#).unwrap();
    assert_eq!(result, json!([vec![true; 11], "Original page"]));
}

#[test]
fn xml_dom_parser_html_parsing_keeps_html_rules_and_live_document() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const doc = new DOMParser().parseFromString('<!doctype html><html><head><title>Example</title></head>' +
            '<body><DIV id="parsed">A&nbsp;B<br>End</DIV></body></html>', 'text/html');
        return [doc.contentType, doc.documentElement.nodeName, doc.body.nodeName,
            doc.getElementById('parsed').nodeName, doc.getElementById('parsed').textContent,
            doc.title, document.getElementById('parsed'), document.getElementById('live').textContent];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            "text/html",
            "HTML",
            "BODY",
            "DIV",
            "A\u{a0}BEnd",
            "Example",
            null,
            "Original page"
        ])
    );
}

#[test]
fn xml_dom_parser_odata_metadata_and_atom_content_populate_a_consumer() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const parser = new DOMParser();
        const edm = 'http://schemas.microsoft.com/ado/2008/09/edm';
        const atom = 'http://www.w3.org/2005/Atom';
        const metadata = parser.parseFromString(
            '<edmx:Edmx xmlns:edmx="http://schemas.microsoft.com/ado/2007/06/edmx">' +
            '<edmx:DataServices><Schema xmlns="' + edm + '" Namespace="Shop">' +
            '<EntityType Name="Product"><Property Name="ID" Type="Edm.Int32"/>' +
            '<Property Name="Name" Type="Edm.String"/></EntityType></Schema></edmx:DataServices></edmx:Edmx>',
            'application/xml');
        const feed = parser.parseFromString('<feed xmlns="' + atom + '"><entry>' +
            '<title>Tea &amp; Coffee</title><id>product:42</id></entry></feed>', 'application/xml');
        const entities = metadata.getElementsByTagNameNS(edm, 'EntityType');
        const columns = Array.from(entities[0].getElementsByTagNameNS(edm, 'Property'), property => property.getAttribute('Name'));
        const entry = feed.getElementsByTagNameNS(atom, 'entry')[0];
        const title = entry.getElementsByTagNameNS(atom, 'title')[0].textContent;
        const output = document.createElement('article');
        output.id = 'loaded';
        output.textContent = entities[0].getAttribute('Name') + ': ' + columns.join(', ') + ' | ' + title;
        document.body.appendChild(output);
        return [metadata.documentElement.nodeName, columns,
            document.getElementById('loaded').textContent, metadata.querySelector('parsererror'), feed.querySelector('parsererror')];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            "edmx:Edmx",
            ["ID", "Name"],
            "Product: ID, Name | Tea & Coffee",
            null,
            null
        ])
    );
}

#[test]
fn xml_dom_parser_xhtml_keeps_xml_case_rules() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const source = '<html xmlns="http://www.w3.org/1999/xhtml"><Mixed UPPER="kept" lower="also"/></html>';
        const xml = new DOMParser().parseFromString(source, 'application/xhtml+xml');
        const mixed = xml.documentElement.firstElementChild;
        const html = new DOMParser().parseFromString('<body><Mixed UPPER="kept"></Mixed></body>', 'text/html');
        return [mixed.nodeName, mixed.localName, mixed.namespaceURI, mixed.getAttribute('UPPER'),
            mixed.getAttribute('upper'), mixed.getAttribute('lower'), xml.querySelector('Mixed') === mixed,
            xml.querySelector('mixed'), html.querySelector('Mixed') === html.querySelector('mixed'),
            html.querySelector('mixed').getAttribute('upper')];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            "Mixed",
            "Mixed",
            "http://www.w3.org/1999/xhtml",
            "kept",
            null,
            "also",
            true,
            null,
            true,
            "kept"
        ])
    );
}

#[test]
fn xml_dom_parser_serialization_roundtrip_preserves_xml_nodes() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const parser = new DOMParser();
        const source = '<p:Root xmlns:p="urn:root" xmlns="urn:child" Code="A&amp;B">' +
            '<Child/><![CDATA[<literal>&data]]><!--keep--><?work ready?></p:Root>';
        const doc = parser.parseFromString(source, 'application/xml');
        const serialized = new XMLSerializer().serializeToString(doc);
        const restored = parser.parseFromString(serialized, 'application/xml');
        const root = restored.documentElement;
        return [restored.querySelector('parsererror'), root.nodeName, root.namespaceURI, root.prefix,
            root.getAttribute('Code'), root.firstElementChild.nodeName, root.firstElementChild.namespaceURI,
            Array.from(root.childNodes, n => [n.nodeType, n.nodeName, n.nodeValue]), root.textContent,
            root.ownerDocument === restored, serialized.includes('<![CDATA[<literal>&data]]>'),
            serialized.includes('<!--keep-->'), serialized.includes('<?work ready?>')];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            null,
            "p:Root",
            "urn:root",
            "p",
            "A&B",
            "Child",
            "urn:child",
            [
                [1, "Child", null],
                [4, "#cdata-section", "<literal>&data"],
                [8, "#comment", "keep"],
                [7, "work", "ready"]
            ],
            "<literal>&data",
            true,
            true,
            true,
            true
        ])
    );
}

#[test]
fn xml_dom_parser_deep_document_clone_preserves_nodes_and_ownership() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const original = new DOMParser().parseFromString(
            '<p:Root xmlns:p="urn:root"><Child><![CDATA[content]]><!--keep--><?work ready?></Child></p:Root>',
            'application/xml');
        const cloned = original.cloneNode(true);
        const root = cloned.documentElement, child = root.firstElementChild;
        child.setAttribute('changed', 'yes');
        return [cloned !== original, root !== original.documentElement, root.nodeName, root.namespaceURI,
            root.prefix, root.ownerDocument === cloned, child.ownerDocument === cloned,
            Array.from(child.childNodes, n => [n.nodeType, n.nodeName, n.nodeValue, n.ownerDocument === cloned]),
            original.documentElement.firstElementChild.getAttribute('changed'), cloned.defaultView];
    })()"#).unwrap();
    assert_eq!(
        result,
        json!([
            true,
            true,
            "p:Root",
            "urn:root",
            "p",
            true,
            true,
            [
                [4, "#cdata-section", "content", true],
                [8, "#comment", "keep", true],
                [7, "work", "ready", true]
            ],
            null,
            null
        ])
    );
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
fn xml_dom_parser_serialization_matches_xml_control_whitespace_normalization() {
    let mut rt = runtime();
    let result = rt.evaluate(r#"(() => {
        const parser = new DOMParser();
        const doc = parser.parseFromString('<Root Value="&#9;&#10;&#13;">&#13;</Root>', 'application/xml');
        const serialized = new XMLSerializer().serializeToString(doc);
        const restored = parser.parseFromString(serialized, 'application/xml');
        return [doc.documentElement.getAttribute('Value'), doc.documentElement.textContent,
            restored.documentElement.getAttribute('Value'), restored.documentElement.textContent,
            restored.querySelector('parsererror')];
    })()"#).unwrap();
    assert_eq!(result, json!(["\t\n\r", "\r", "\t\n\r", "\n", null]));
}
