//! Legacy HTMLSelectElement, HTMLOptionElement and table members that older
//! pages read constantly (`table.rows[i].cells[j]`, `select.item(i)`, ...).
//! Expected values are what Chrome returns for the same markup.

use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

const PAGE: &str = r#"<select id="s"><option value="a" id="oa">A</option><optgroup><option value="b" name="nb" selected>B</option></optgroup></select><table id="t"><tfoot><tr><td>f</td></tr></tfoot><thead><tr><th>h</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody><tr><td>direct</td></tr></table>"#;

async fn evaluate(expression: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "session-1";
    ctx.sessions.insert(session_id.to_string(), page_id);
    let nav = dispatch(
        &CdpRequest {
            id: 1,
            method: "Page.navigate".to_string(),
            params: json!({"url": format!("data:text/html,{PAGE}"), "waitUntil": "load"}),
            session_id: Some(session_id.to_string()),
        },
        &mut ctx,
    )
    .await;
    assert!(nav.error.is_none(), "navigate failed: {:?}", nav.error);
    let resp = dispatch(
        &CdpRequest {
            id: 2,
            method: "Runtime.evaluate".to_string(),
            params: json!({"expression": expression, "returnByValue": true}),
            session_id: Some(session_id.to_string()),
        },
        &mut ctx,
    )
    .await;
    assert!(resp.error.is_none(), "evaluate failed: {:?}", resp.error);
    resp.result.unwrap()["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn select_exposes_length_item_named_item_and_option_index() {
    let value = evaluate(
        "(() => { const s = document.getElementById('s');
           return [s.length, s.item(s.selectedIndex).value, s.item(5), s.namedItem('oa').value,
                   s.namedItem('nb').value, s.namedItem('none'), s.options[1].index,
                   document.createElement('option').index].join('|'); })()",
    )
    .await;
    assert_eq!(value, json!("2|b||a|b||1|0"));
}

#[tokio::test(flavor = "current_thread")]
async fn table_rows_put_head_first_and_foot_last() {
    let value = evaluate(
        "(() => { const t = document.getElementById('t');
           // The parser wraps the markup's direct <tr> in an implied <tbody>; a
           // row appended by script stays a direct child of the table.
           const direct = t.appendChild(document.createElement('tr')); direct.insertCell().textContent = 'dom';
           const rows = Array.from(t.rows, (r) => r.cells[0].textContent).join(',');
           const body = t.tBodies[0].rows[0];
           return [rows, t.tHead.rows.length, t.tFoot.localName, t.tBodies.length, body.cells.length,
                   body.rowIndex, body.sectionRowIndex, t.rows[2].sectionRowIndex, direct.rowIndex,
                   direct.sectionRowIndex, body.cells[1].cellIndex,
                   document.createElement('tr').rowIndex, document.createElement('td').cellIndex].join('|'); })()",
    )
    .await;
    assert_eq!(value, json!("h,1,direct,dom,f|1|tfoot|2|2|1|0|0|3|3|1|-1|-1"));
}

#[tokio::test(flavor = "current_thread")]
async fn rows_and_cells_can_be_inserted_and_deleted() {
    let value = evaluate(
        "(() => { const t = document.getElementById('t');
           const row = t.insertRow(2); row.insertCell(0).textContent = 'x'; row.insertCell(0).textContent = 'y';
           const out = [row.parentNode.localName, Array.from(row.cells, (c) => c.textContent).join('')];
           row.deleteCell(-1); out.push(row.cells.length);
           t.deleteRow(0); out.push(t.rows[0].cells[0].textContent);
           let error = ''; try { t.insertRow(99); } catch (e) { error = e.name; } out.push(error);
           const empty = document.createElement('table'); empty.insertRow();
           out.push(empty.firstElementChild.localName + '>' + empty.firstElementChild.firstElementChild.localName);
           out.push(t.createTBody().previousElementSibling.localName);
           return out.join('|'); })()",
    )
    .await;
    assert_eq!(value, json!("tbody|yx|1|1|IndexSizeError|tbody>tr|tbody"));
}

#[tokio::test(flavor = "current_thread")]
async fn legacy_members_only_exist_on_their_elements() {
    let value = evaluate(
        "(() => { const div = document.createElement('div');
           return [['length', 'rows', 'cells', 'cellIndex', 'index', 'tBodies'].some((k) => k in div),
                   document.getElementById('s') instanceof HTMLSelectElement, div instanceof HTMLSelectElement,
                   document.getElementById('t') instanceof HTMLTableElement, new Option('o') instanceof HTMLOptionElement,
                   div instanceof HTMLOptionElement].join('|'); })()",
    )
    .await;
    assert_eq!(value, json!("false|true|false|true|true|false"));
}
