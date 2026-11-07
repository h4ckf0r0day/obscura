//! Shared markdown extraction script used by the LP.getMarkdown CDP method
//! and the CLI `--dump markdown` mode. Lives in obscura-browser so both
//! obscura-cdp and obscura-cli can call it without depending on each other.

/// JS expression that walks `document.body` and returns a markdown string.
/// Must be evaluated against a Page that has a fully-bootstrapped JS runtime.
pub const HTML_TO_MARKDOWN_JS: &str = r#"
(function() {
    // Post-order walk on an explicit stack: a deep tree cannot overflow the
    // JS stack, and every node is converted once. A <tr> reuses its cells'
    // results instead of converting each cell again, which doubled the work
    // for every level of nested tables.
    function finish(el, frame) {
        var tag = (el.tagName || '').toLowerCase();
        var children = frame.parts.join('').replace(/\n{3,}/g, '\n\n');
        switch(tag) {
            case 'h1': return '\n# ' + children.trim() + '\n\n';
            case 'h2': return '\n## ' + children.trim() + '\n\n';
            case 'h3': return '\n### ' + children.trim() + '\n\n';
            case 'h4': return '\n#### ' + children.trim() + '\n\n';
            case 'h5': return '\n##### ' + children.trim() + '\n\n';
            case 'h6': return '\n###### ' + children.trim() + '\n\n';
            case 'p': return '\n' + children.trim() + '\n\n';
            case 'br': return '\n';
            case 'hr': return '\n---\n\n';
            case 'strong': case 'b': return '**' + children + '**';
            case 'em': case 'i': return '*' + children + '*';
            case 'code': return '`' + children + '`';
            case 'pre': return '\n```\n' + children + '\n```\n\n';
            case 'blockquote': return '\n> ' + children.trim().replace(/\n/g, '\n> ') + '\n\n';
            case 'a':
                var href = el.getAttribute('href') || '';
                if (href && children.trim()) return '[' + children.trim() + '](' + href + ')';
                return children;
            case 'img':
                var src = el.getAttribute('src') || '';
                var alt = el.getAttribute('alt') || '';
                return '![' + alt + '](' + src + ')';
            case 'ul': case 'ol':
                return '\n' + children + '\n';
            case 'li':
                var parent = el.parentNode;
                var isOrdered = parent && parent.tagName && parent.tagName.toLowerCase() === 'ol';
                var bullet = isOrdered ? '1. ' : '- ';
                return bullet + children.trim() + '\n';
            case 'table': return '\n' + children + '\n';
            case 'thead': case 'tbody': case 'tfoot': return children;
            case 'tr':
                var cells = [];
                for (var j = 0; j < frame.cells.length; j++) cells.push(frame.cells[j].trim());
                return '| ' + cells.join(' | ') + ' |\n';
            case 'th': case 'td': return children;
            case 'script': case 'style': case 'noscript': case 'link': case 'meta': return '';
            case 'div': case 'section': case 'article': case 'main': case 'aside': case 'nav': case 'header': case 'footer':
                return '\n' + children;
            case 'span': return children;
            default: return children;
        }
    }
    var body = document.body || document.documentElement;
    if (!body) return '';
    var stack = [{ node: body, kids: body.childNodes || [], index: 0, parts: [], cells: [] }];
    var md = '';
    while (stack.length) {
        var frame = stack[stack.length - 1];
        // read once per node: childNodes builds a fresh list on every access
        var cn = frame.kids;
        if (frame.index < cn.length) {
            var child = cn[frame.index++];
            if (child.nodeType === 3) frame.parts.push(child.textContent || '');
            else if (child.nodeType === 1) stack.push({ node: child, kids: child.childNodes || [], index: 0, parts: [], cells: [] });
            continue;
        }
        stack.pop();
        var out = finish(frame.node, frame);
        if (!stack.length) { md = out; break; }
        var parent = stack[stack.length - 1];
        parent.parts.push(out);
        parent.cells.push(out);
    }
    md = md.replace(/\n{3,}/g, '\n\n').trim();
    return md;
})()
"#;
