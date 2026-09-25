//! Shared markdown extraction script used by the LP.getMarkdown CDP method
//! and the CLI `--dump markdown` mode. Lives in obscura-browser so both
//! obscura-cdp and obscura-cli can call it without depending on each other.

/// JS expression that walks `document.body` and returns a markdown string.
/// Must be evaluated against a Page that has a fully-bootstrapped JS runtime.
pub const HTML_TO_MARKDOWN_JS: &str = r#"
(function() {
    // GFM table: header row, `|---|` separator, then one line per row with
    // no blank lines between. Cells collapse to a single line with `|`
    // escaped; colspan repeats empty cells and short rows are padded. The
    // caption becomes a paragraph above or below per its caption-side.
    function tableToMd(table, depth) {
        var rows = [];
        var header = null;
        function collect(parent, inHead) {
            var kids = parent.childNodes || [];
            for (var i = 0; i < kids.length; i++) {
                var k = kids[i];
                if (k.nodeType !== 1) continue;
                var t = k.tagName.toLowerCase();
                if (t === 'thead') collect(k, true);
                else if (t === 'tbody' || t === 'tfoot') collect(k, false);
                else if (t === 'tr') {
                    var cells = [];
                    var allTh = true;
                    var cs = k.childNodes || [];
                    for (var j = 0; j < cs.length; j++) {
                        var c = cs[j];
                        if (c.nodeType !== 1) continue;
                        var ct = c.tagName.toLowerCase();
                        if (ct !== 'td' && ct !== 'th') continue;
                        if (ct !== 'th') allTh = false;
                        var text = toMd(c, depth).replace(/\s*\n+\s*/g, ' ').trim().replace(/\|/g, '\\|');
                        cells.push(text);
                        var span = parseInt(c.getAttribute('colspan') || '1', 10);
                        for (var s2 = 1; s2 < span && s2 < 1000; s2++) cells.push('');
                    }
                    if (!cells.length) continue;
                    if (header === null && rows.length === 0 && (inHead || allTh)) header = cells;
                    else rows.push(cells);
                }
            }
        }
        collect(table, false);
        if (header === null) {
            if (!rows.length) return '';
            header = rows.shift();
        }
        var width = header.length;
        for (var r = 0; r < rows.length; r++) width = Math.max(width, rows[r].length);
        function line(cells) {
            var out = [];
            for (var i = 0; i < width; i++) out.push(cells[i] || '');
            return '| ' + out.join(' | ') + ' |';
        }
        var sep = [];
        for (var w = 0; w < width; w++) sep.push('---');
        var md = [line(header), '|' + sep.join('|') + '|'];
        for (var r2 = 0; r2 < rows.length; r2++) md.push(line(rows[r2]));
        var body = md.join('\n');
        var caption = null;
        var tk = table.childNodes || [];
        for (var q = 0; q < tk.length; q++) {
            if (tk[q].nodeType === 1 && tk[q].tagName.toLowerCase() === 'caption') { caption = tk[q]; break; }
        }
        if (caption) {
            var capKids = caption.childNodes || [];
            var capRaw = '';
            for (var ck = 0; ck < capKids.length; ck++) capRaw += toMd(capKids[ck], depth);
            var capText = capRaw.replace(/\s*\n+\s*/g, ' ').trim();
            var side = '';
            try { side = getComputedStyle(caption).captionSide || ''; } catch (e) {}
            if (!side) side = (caption.getAttribute('style') || '').match(/caption-side\s*:\s*bottom/i) ? 'bottom' : 'top';
            if (capText) body = side === 'bottom' ? body + '\n\n' + capText : capText + '\n\n' + body;
        }
        return '\n\n' + body + '\n\n';
    }
    function toMd(el, depth) {
        if (!el) return '';
        var out = '';
        if (el.nodeType === 3) return el.textContent || '';
        if (el.nodeType !== 1) return '';
        var tag = (el.tagName || '').toLowerCase();
        var children = '';
        var cn = el.childNodes || [];
        for (var i = 0; i < cn.length; i++) children += toMd(cn[i], depth);
        children = children.replace(/\n{3,}/g, '\n\n');
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
            case 'table': return tableToMd(el, depth);
            case 'caption': return '';
            case 'thead': case 'tbody': case 'tfoot': case 'tr': case 'th': case 'td': return children;
            case 'script': case 'style': case 'noscript': case 'link': case 'meta': return '';
            case 'div': case 'section': case 'article': case 'main': case 'aside': case 'nav': case 'header': case 'footer':
                return '\n' + children;
            case 'span': return children;
            default: return children;
        }
    }
    var body = document.body || document.documentElement;
    var md = toMd(body, 0);
    md = md.replace(/\n{3,}/g, '\n\n').trim();
    return md;
})()
"#;
