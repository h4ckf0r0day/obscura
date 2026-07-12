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
        self.serialize_worklist(self.child_work(node_id), &mut buf);
        buf
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
                NodeData::Element { name, attrs, .. } => {
                    let tag = name.local.as_ref();
                    if include_self {
                        buf.push('<');
                        buf.push_str(tag);
                        for attr in attrs {
                            buf.push(' ');
                            let attr_name = attr.name.local.as_ref();
                            buf.push_str(attr_name);
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
                        for w in self.child_work(node_id) {
                            stack.push(w);
                        }
                    }
                }
                NodeData::Text { contents } => {
                    let parent_is_raw = node.parent
                        .and_then(|pid| {
                            self.with_node(pid, |p| {
                                p.as_element()
                                    .map(|name| is_raw_text_element(name.local.as_ref()))
                                    .unwrap_or(false)
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

fn escape_text(s: &str, buf: &mut String) {
    for c in s.chars() {
        match c {
            '&' => buf.push_str("&amp;"),
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
            '"' => buf.push_str("&quot;"),
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

fn is_raw_text_element(tag: &str) -> bool {
    matches!(tag, "script" | "style" | "textarea" | "title")
}

#[cfg(test)]
mod tests {
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
}
