# Parsing XML data

`DOMParser.parseFromString()` uses XML parsing for `application/xml`, `text/xml`,
`application/xhtml+xml`, and `image/svg+xml`. `text/html` keeps the HTML parser.
Other MIME types throw `TypeError`, matching the DOMParser API.

```javascript
const metadata = new DOMParser().parseFromString(source, 'application/xml');
const entities = metadata.getElementsByTagNameNS(
  'http://docs.oasis-open.org/odata/ns/edm', 'EntityType');
```

XML documents preserve qualified names and case, scoped default and prefixed
namespaces, namespace resets, attributes, predefined/numeric entity references,
CDATA, comments, processing instructions, and document types. Parsed nodes belong
to a detached XML document; parsing does not insert them into the active page.
Queries include the document element, and XHTML parsed as XML uses case-sensitive
names. Malformed XML returns a document with a `parsererror` element.

`XMLSerializer` preserves names and the namespace declarations already present
in parsed XML, including CDATA, comments, and processing instructions.

This change addresses XML-backed application data such as OData metadata and
Atom feeds. It does not add a complete XML browser or DTD processing support:
DTD-defined entity references are unsupported, and generic XML node cloning,
mutation ownership, and serialization namespace synthesis for newly created
nodes retain limitations. External DTD resources are not loaded.
