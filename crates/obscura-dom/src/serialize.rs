use html5ever::{ns, QualName};

use crate::tree::{DomTree, NodeData, NodeId};

// A unit of pending serialization work. Held on an explicit heap stack instead
// of the call stack so a deeply nested tree cannot overflow the thread stack and
// abort the process (a stack overflow is a hard abort that `op_dom`'s
// catch_unwind cannot recover). `descendants()` is iterative + capped for the
// same reason; the serializer must be too.
enum SerializeWork {
    // Serialize this node; `include_self` mirrors the old recursive param.
    Node(NodeId, bool),
    // Emit a previously-opened element's closing tag, after its children.
    CloseTag(String),
}

impl DomTree {
    pub fn outer_html(&self, node_id: NodeId) -> String {
        let mut buf = String::new();
        self.serialize_worklist(vec![SerializeWork::Node(node_id, true)], &mut buf);
        buf
    }

    pub fn inner_html(&self, node_id: NodeId) -> String {
        let mut buf = String::new();
        self.serialize_worklist(self.child_work(self.content_source(node_id)), &mut buf);
        buf
    }

    // The node whose children represent `node_id`'s markup. For a <template>
    // that is its contents document, since the parser puts template children
    // there rather than under the element (issue #463); for everything else it
    // is the node itself.
    fn content_source(&self, node_id: NodeId) -> NodeId {
        self.with_node(node_id, |n| match &n.data {
            NodeData::Element { template_contents: Some(contents), .. } => Some(*contents),
            _ => None,
        })
        .flatten()
        .unwrap_or(node_id)
    }

    // Children as work items in document order (top of the LIFO stack first).
    fn child_work(&self, node_id: NodeId) -> Vec<SerializeWork> {
        self.children(node_id)
            .into_iter()
            .rev()
            .map(|c| SerializeWork::Node(c, true))
            .collect()
    }

    fn serialize_worklist(&self, mut stack: Vec<SerializeWork>, buf: &mut String) {
        // Defense in depth, mirroring descendants(): a well-formed subtree emits
        // at most one Node plus one CloseTag per node, so 2*nodes.len() work
        // items bound a valid walk. Exceeding it means the graph is cyclic (the
        // append_child / insert_before guards prevent that); stop rather than
        // spin forever. On a valid tree this bound is never reached.
        let max_steps = self
            .node_slot_count()
            .saturating_mul(2)
            .saturating_add(16);
        let mut steps = 0usize;

        while let Some(work) = stack.pop() {
            steps += 1;
            if steps > max_steps {
                eprintln!("obscura: serialize worklist cap hit - tree has a cycle");
                break;
            }

            let (node_id, include_self) = match work {
                SerializeWork::CloseTag(tag) => {
                    buf.push_str("</");
                    buf.push_str(&tag);
                    buf.push('>');
                    continue;
                }
                SerializeWork::Node(node_id, include_self) => (node_id, include_self),
            };

            let node = match self.get_node(node_id) {
                Some(n) => n,
                None => continue,
            };

            match &node.data {
                NodeData::Document => {
                    for w in self.child_work(node_id) {
                        stack.push(w);
                    }
                }
                NodeData::Doctype { name, .. } => {
                    buf.push_str("<!DOCTYPE ");
                    buf.push_str(name);
                    buf.push('>');
                }
                NodeData::Element { name, attrs, template_contents, .. } => {
                    let tag = name.local.as_ref();
                    if include_self {
                        buf.push('<');
                        buf.push_str(tag);
                        for attr in attrs {
                            buf.push(' ');
                            if let Some(prefix) = &attr.name.prefix {
                                buf.push_str(prefix);
                                buf.push(':');
                            }
                            buf.push_str(attr.name.local.as_ref());
                            buf.push_str("=\"");
                            escape_attr(&attr.value, buf);
                            buf.push('"');
                        }
                        buf.push('>');
                    }

                    if !is_void_element(tag) {
                        // Push the closing tag first so it pops after all the
                        // children we push next.
                        if include_self {
                            stack.push(SerializeWork::CloseTag(tag.to_string()));
                        }
                        // A <template> serializes its contents document, not its
                        // own (always empty) children (issue #463). template_contents
                        // is already in hand from this node, so read it directly
                        // rather than re-fetching and cloning the node.
                        let source = template_contents.unwrap_or(node_id);
                        for w in self.child_work(source) {
                            stack.push(w);
                        }
                    }
                }
                NodeData::Text { contents } => {
                    let parent_is_raw = node.parent
                        .and_then(|pid| {
                            self.with_node(pid, |p| {
                                p.as_element().is_some_and(serializes_text_literally)
                            })
                        })
                        .unwrap_or(false);

                    if parent_is_raw {
                        buf.push_str(contents);
                    } else {
                        escape_text(contents, buf);
                    }
                }
                NodeData::Comment { contents } => {
                    buf.push_str("<!--");
                    // The HTML parser can never produce a comment that closes early,
                    // but script can via document.createComment(...). The tokenizer
                    // ends a comment on ANY of four sequences: "-->", "--!>", a
                    // leading ">", or a leading "->" (comment-start / -start-dash
                    // abrupt-close). Every one requires a ">". Emitting it verbatim
                    // would close the comment early and let the trailing text parse
                    // as live markup (mXSS). Entities are not decoded inside
                    // comments, so escaping every ">" to "&gt;" neutralizes all four
                    // forms at once and keeps the data as a single comment.
                    // (Supersedes the earlier "-->"-only guard, which left the
                    // leading-">", leading-"->", and "--!>" forms exploitable.)
                    if contents.contains('>') {
                        buf.push_str(&contents.replace('>', "&gt;"));
                    } else {
                        buf.push_str(contents);
                    }
                    buf.push_str("-->");
                }
                NodeData::ProcessingInstruction { target, data } => {
                    buf.push_str("<?");
                    buf.push_str(target);
                    buf.push(' ');
                    buf.push_str(data);
                    buf.push('>');
                }
            }
        }
    }
}

// "Escaping a string" from the HTML Standard. Attribute mode escapes `<` and
// `>` too: left literal, a value such as `</noscript><img>` ends its raw-text
// parent when the markup is parsed again.
fn escape_text(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '\u{a0}' => buf.push_str("&nbsp;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            _ => buf.push(c),
        }
    }
}

fn escape_attr(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
            '\u{a0}' => buf.push_str("&nbsp;"),
            '"' => buf.push_str("&quot;"),
            '<' => buf.push_str("&lt;"),
            '>' => buf.push_str("&gt;"),
            _ => buf.push(c),
        }
    }
}

fn is_void_element(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

/// Parents whose text children are written without escaping, per "serializing
/// HTML fragments". `textarea` and `title` are not among them: the parser
/// decodes character references inside them, so their text must be escaped or
/// `&lt;/textarea&gt;` comes back out as a real end tag. Only HTML elements
/// qualify: a `style` or `script` in SVG or MathML holds ordinary text, and
/// tags written literally there would be parsed as markup on the way back in.
/// `noscript` is literal because scripting is enabled.
fn serializes_text_literally(name: &QualName) -> bool {
    name.ns == ns!(html)
        && matches!(
            name.local.as_ref(),
            "style" | "script" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext" | "noscript"
        )
}

#[cfg(test)]
mod tests {
    use html5ever::{LocalName, QualName};

    use crate::tree::{NodeData, ShadowRootMode};
    use crate::tree_sink::parse_html;

    #[test]
    fn test_outer_html() {
        let tree = parse_html(r#"<div id="test"><p>Hello</p></div>"#);
        let div = tree.get_element_by_id("test").unwrap();
        let html = tree.outer_html(div);
        assert!(html.contains(r#"<div id="test">"#));
        assert!(html.contains("<p>Hello</p>"));
        assert!(html.contains("</div>"));
    }

    #[test]
    fn test_inner_html() {
        let tree = parse_html(r#"<div id="test"><p>Hello</p><p>World</p></div>"#);
        let div = tree.get_element_by_id("test").unwrap();
        let html = tree.inner_html(div);
        assert!(html.contains("<p>Hello</p>"));
        assert!(html.contains("<p>World</p>"));
        assert!(!html.contains("<div"));
    }

    #[test]
    fn test_serialize_attributes() {
        let tree = parse_html(r#"<a href="https://example.com" class="link">Click</a>"#);
        let a = tree.query_selector("a").unwrap().unwrap();
        let html = tree.outer_html(a);
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("class=\"link\""));
    }

    #[test]
    fn test_serialize_special_chars() {
        let tree = parse_html("<p>Hello &amp; World &lt;3</p>");
        let p = tree.query_selector("p").unwrap().unwrap();
        let html = tree.outer_html(p);
        assert!(html.contains("&amp;"));
        assert!(html.contains("&lt;"));
    }

    /// Parses `html`, serializes it, and parses the result again.
    fn reparsed(html: &str) -> (String, crate::tree::DomTree) {
        let first = parse_html(html);
        let out = first.inner_html(first.document());
        let second = parse_html(&out);
        (out, second)
    }

    #[test]
    fn textarea_and_title_text_is_escaped_so_it_cannot_close_its_element() {
        let (out, again) = reparsed(
            "<title>a &lt;/title&gt;&lt;script&gt;x&lt;/script&gt;</title>\
             <textarea>b &lt;/textarea&gt;&lt;script&gt;y&lt;/script&gt; &amp;amp;</textarea>",
        );
        assert!(out.contains("<title>a &lt;/title&gt;&lt;script&gt;x&lt;/script&gt;</title>"), "{out}");
        assert!(
            out.contains("<textarea>b &lt;/textarea&gt;&lt;script&gt;y&lt;/script&gt; &amp;amp;</textarea>"),
            "{out}"
        );
        assert!(again.query_selector("script").unwrap().is_none(), "{out}");
    }

    #[test]
    fn raw_text_elements_keep_their_text_literal() {
        for tag in ["script", "style", "xmp", "iframe", "noembed", "noframes", "noscript"] {
            let source = format!("<{tag}>a < b && c > d</{tag}>");
            let tree = parse_html(&source);
            let node = tree.query_selector(tag).unwrap().unwrap();
            assert_eq!(tree.outer_html(node), source, "{tag}");
        }
        let tree = parse_html("<plaintext>a < b & c");
        let node = tree.query_selector("plaintext").unwrap().unwrap();
        assert_eq!(tree.inner_html(node), "a < b & c");
    }

    #[test]
    fn style_and_script_text_in_foreign_content_is_escaped() {
        for (open, close) in [("<svg><style>", "</style></svg>"), ("<svg><script>", "</script></svg>"), ("<math><style>", "</style></math>")] {
            let (out, again) = reparsed(&format!("{open}&lt;img src=x onerror=1&gt;{close}"));
            assert!(out.contains("&lt;img src=x onerror=1&gt;"), "{out}");
            assert!(again.query_selector("img").unwrap().is_none(), "{out}");
        }
    }

    #[test]
    fn attribute_values_and_text_are_escaped_as_the_standard_says() {
        let (out, again) = reparsed("<a title='&quot;&gt;&lt;img src=x&gt; a&nbsp;b &amp;amp;'>c&nbsp;d</a>");
        let link = "<a title=\"&quot;&gt;&lt;img src=x&gt; a&nbsp;b &amp;amp;\">c&nbsp;d</a>";
        assert!(out.contains(link), "{out}");
        assert_eq!(again.outer_html(again.query_selector("a").unwrap().unwrap()), link);
    }

    #[test]
    fn an_attribute_value_cannot_end_a_raw_text_parent() {
        // What a script can build with appendChild: an element inside a
        // <noscript>, whose text the parser reads as raw text next time.
        let tree = parse_html("<noscript id=host></noscript>");
        let host = tree.get_element_by_id("host").unwrap();
        let child = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), LocalName::from("p")),
            attrs: vec![crate::tree::Attribute {
                name: QualName::new(None, ns!(), LocalName::from("title")),
                value: "</noscript><img src=x onerror=1>".to_string(),
            }],
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        });
        tree.append_child(host, child);

        let out = tree.outer_html(host);
        assert_eq!(out, "<noscript id=\"host\"><p title=\"&lt;/noscript&gt;&lt;img src=x onerror=1&gt;\"></p></noscript>");
        assert!(parse_html(&out).query_selector("img").unwrap().is_none(), "{out}");
    }

    #[test]
    fn test_void_elements() {
        let tree = parse_html(r#"<img src="test.png"><br>"#);
        let img = tree.query_selector("img").unwrap().unwrap();
        let html = tree.outer_html(img);
        assert!(html.contains("<img"));
        assert!(!html.contains("</img>"));
    }

    #[test]
    fn comment_serialization_neutralizes_all_terminator_forms() {
        use crate::tree::NodeData;

        // A comment ends on any of "-->", "--!>", a leading ">", or a leading
        // "->". `document.createComment(...)` accepts arbitrary strings, so a
        // scripted comment can carry each closing form followed by a real tag.
        // Serializing must keep the payload inside a single comment; if it
        // closes early, the trailing "<img>" becomes live markup (mXSS).
        let payloads = [
            "><img src=x onerror=alert(1)>", // leading ">" abrupt-closes empty comment
            "-><img src=x>",                 // leading "->" abrupt-closes
            "a--!><img src=x>",              // internal "--!>" closes (incorrectly-closed)
            "a--><img src=x>",               // internal "-->" closes (the previously-fixed form)
        ];

        for payload in payloads {
            let tree = parse_html(r#"<div id="host"></div>"#);
            let host = tree.get_element_by_id("host").unwrap();
            let comment = tree.new_node(NodeData::Comment { contents: payload.to_string() });
            tree.append_child(host, comment);

            let serialized = tree.outer_html(host);

            // Re-parsing the serialized markup must not surface an <img>: the
            // payload has to stay inside the comment.
            let reparsed = parse_html(&serialized);
            assert!(
                reparsed.query_selector("img").unwrap().is_none(),
                "payload {payload:?} escaped the comment; serialized = {serialized}"
            );
            // And the serialized comment data must carry no raw ">".
            let inner = &serialized[serialized.find("<!--").unwrap() + 4..];
            let inner = &inner[..inner.find("-->").unwrap()];
            assert!(
                !inner.contains('>'),
                "comment data still contains a raw '>': {serialized}"
            );
        }
    }

    #[test]
    fn host_serialization_excludes_its_native_shadow_tree() {
        let tree = parse_html(r#"<x-card id="host"><span id="light">light</span></x-card>"#);
        let host = tree.get_element_by_id("host").unwrap();
        let root = tree.attach_shadow_root(host, ShadowRootMode::Open).unwrap();
        let shadow = tree.new_node(NodeData::Element {
            name: QualName::new(None, ns!(html), LocalName::from("strong")),
            attrs: Vec::new(),
            template_contents: None,
            mathml_annotation_xml_integration_point: false,
        });
        let shadow_text = tree.new_node(NodeData::Text {
            contents: "shadow".to_string(),
        });
        tree.append_child(root, shadow);
        tree.append_child(shadow, shadow_text);

        assert_eq!(tree.inner_html(host), r#"<span id="light">light</span>"#);
        assert_eq!(
            tree.outer_html(host),
            r#"<x-card id="host"><span id="light">light</span></x-card>"#
        );
        assert_eq!(tree.inner_html(root), "<strong>shadow</strong>");
    }
}
