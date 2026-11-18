use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

const PAGE: &str = r#"<select id="s"><option value="a">A</option><option value="b" selected>B</option></select><table id="t"><tfoot><tr><td>f</td></tr></tfoot><thead><tr><th>h</th></tr></thead><tbody><tr id="body"><td id="first">1</td><td>2</td></tr></tbody><tr><td>direct</td></tr></table>"#;

async fn evaluate(expression: &str) -> Value {
    let mut ctx = CdpContext::new();
    let page_id = ctx.create_page();
    let session_id = "legacy-table";
    ctx.sessions.insert(session_id.to_string(), page_id);
    let nav = dispatch(&CdpRequest {
        id: 1, method: "Page.navigate".to_string(),
        params: json!({"url": format!("data:text/html,{PAGE}"), "waitUntil": "load"}),
        session_id: Some(session_id.to_string()),
    }, &mut ctx).await;
    assert!(nav.error.is_none(), "{:?}", nav.error);
    assert!(nav.result.as_ref().unwrap().get("errorText").is_none(), "{:?}", nav.result);
    let resp = dispatch(&CdpRequest {
        id: 2, method: "Runtime.evaluate".to_string(),
        params: json!({"expression": expression, "returnByValue": true}),
        session_id: Some(session_id.to_string()),
    }, &mut ctx).await;
    assert!(resp.error.is_none(), "{:?}", resp.error);
    let result = resp.result.unwrap();
    assert!(result.get("exceptionDetails").is_none(), "{result}");
    result["result"]["value"].clone()
}

#[tokio::test(flavor = "current_thread")]
async fn issue_repro_preserves_landed_select_and_table_rows_apis() {
    assert_eq!(evaluate(r#"(() => {
        const s = document.getElementById('s'), t = document.createElement('table');
        t.innerHTML = '<tbody><tr><td>1</td><td>2</td></tr></tbody>';
        return [s.length, s[s.selectedIndex].value, t.rows.length, t.rows[0].cells.length].join(',');
    })()"#).await, json!("2,b,1,2"));
}

#[tokio::test(flavor = "current_thread")]
async fn select_indexed_access_is_live_and_uses_platform_setter_reflection() {
    assert_eq!(evaluate(r#"(() => {
        const s = document.getElementById('s');
        const initial = [s[0] === s.item(0), s[1] === s.item(1), s[2] === undefined,
            '0' in s, '2' in s, s instanceof HTMLSelectElement];
        const inserted = new Option('New', 'n');
        s[0] = inserted;
        const replaced = [s[0] === inserted, s.length, inserted.index, s[1].value];
        s[0] = null;
        const removed = [s.length, s[0].value, s[1] === undefined];
        s[3] = new Option('Last', 'last');
        const filled = Array.from(s.options, option => option.value);
        const defined = Reflect.defineProperty(s, '0', {value:new Option('Defined','d'),configurable:false});
        const descriptor = Object.getOwnPropertyDescriptor(s, '0');
        const reflection = [defined, s[0].value, descriptor.writable, descriptor.enumerable,
            descriptor.configurable, Reflect.deleteProperty(s, '0'), Reflect.deleteProperty(s, '99')];
        const accessor = Reflect.defineProperty(s, '0', {get(){return null;}});
        const kept = s[0].value;
        Reflect.defineProperty(s, '0', {configurable:false});
        let invalid = false;
        try { s[0] = 'bad'; } catch(e) { invalid = e instanceof TypeError; }
        s.innerHTML = '<option value="fresh">Fresh</option>';
        return {initial,replaced,removed,filled,reflection,accessor,kept,invalid,
            fresh:[s[0].value,s[1] === undefined,s.length]};
    })()"#).await, json!({
        "initial":[true,true,true,true,false,true],
        "replaced":[true,2,0,"b"],"removed":[1,"b",true],
        "filled":["b","","","last"],"reflection":[true,"d",true,true,true,false,true],
        "accessor":true,"kept":"d","invalid":true,"fresh":["fresh",true,1],
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn native_indices_follow_actual_table_section_and_cell_ownership() {
    assert_eq!(evaluate(r#"(() => {
        const t = document.getElementById('t'), direct = t.appendChild(document.createElement('tr'));
        direct.insertCell().textContent = 'dom';
        const body = t.tBodies[0].rows[0];
        body.cells[0].innerHTML = '1<table><tr><td>nested</td></tr></table>';
        body.append(document.createElementNS('urn:foreign', 'td'));
        t.tBodies[0].append(document.createElementNS('urn:foreign', 'tr'));
        const section = document.createElement('tbody'), detached = section.insertRow();
        return [Array.from(t.rows, r => r.cells[0].textContent).join(','),
            t.tHead.rows.length, t.tFoot.localName, t.tBodies.length, body.cells.length,
            body.rowIndex, body.sectionRowIndex, t.rows[2].sectionRowIndex,
            direct.rowIndex, direct.sectionRowIndex, body.cells[1].cellIndex,
            detached.rowIndex, detached.sectionRowIndex,
            document.createElement('tr').rowIndex, document.createElement('td').cellIndex];
    })()"#).await, json!(["h,1nested,direct,dom,f",1,"tfoot",2,2,1,0,0,3,3,1,-1,0,-1,-1]));
}

#[tokio::test(flavor = "current_thread")]
async fn all_table_child_collections_are_live_cached_readonly_and_named() {
    assert_eq!(evaluate(r#"(() => {
        const t = document.getElementById('t'), bodies = t.tBodies, section = bodies[0];
        const rows = section.rows, row = rows[0], cells = row.cells;
        const descriptor = Object.getOwnPropertyDescriptor(cells, '0');
        const initial = [bodies === t.tBodies, rows === section.rows, cells === row.cells,
            bodies instanceof HTMLCollection, rows instanceof HTMLCollection, cells instanceof HTMLCollection,
            Array.isArray(cells), Object.prototype.toString.call(cells),
            rows.body === row, cells.first === cells[0], cells.item(9), cells.namedItem('missing'),
            Object.keys(cells), [descriptor.value === cells[0], descriptor.writable,
                descriptor.enumerable, descriptor.configurable]];
        const writes = [Reflect.set(cells, '0', null), Reflect.set(cells, 'length', 0),
            Reflect.defineProperty(cells, '0', {value:null}), Reflect.deleteProperty(cells, '0')];
        cells[0].id = 'renamed'; cells[0].setAttribute('name', 'alias');
        const renamed = [cells.first === undefined, cells.renamed === cells[0], cells.alias === cells[0]];
        row.insertCell(0).id = 'new'; row.deleteCell(-1);
        const changed = [cells.length, cells[0].id, cells[1].id, cells[1].cellIndex];
        section.deleteRow(0); t.createTBody();
        return {initial, writes, renamed, changed, empty:rows.length, bodies:bodies.length,
            retained:cells.length, detached:row.rowIndex, detachedSection:row.sectionRowIndex};
    })()"#).await, json!({
        "initial":[true,true,true,true,true,true,false,"[object HTMLCollection]",true,true,null,null,
            ["0","1"],[true,false,true,true]],
        "writes":[false,false,false,false],"renamed":[true,true,true],
        "changed":[2,"new","renamed",1],"empty":0,"bodies":3,"retained":2,
        "detached":-1,"detachedSection":-1,
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn table_and_section_mutations_keep_correct_parent_and_row_order() {
    assert_eq!(evaluate(r#"(() => {
        const t = document.getElementById('t'), row = t.insertRow(2);
        row.insertCell(0).textContent = 'x'; row.insertCell(0).textContent = 'y';
        const out = [row.parentNode.localName, Array.from(row.cells, c => c.textContent).join('')];
        row.deleteCell(-1); out.push(row.cells.length);
        t.deleteRow(0); out.push(t.rows[0].cells[0].textContent);
        const empty = document.createElement('table');
        empty.append(document.createElement('thead'), document.createElement('tbody'),
            document.createElement('tfoot'), document.createElement('tbody'));
        const inserted = empty.insertRow();
        out.push(inserted.parentNode === empty.lastElementChild);
        const body = t.createTBody(); out.push(body.previousElementSibling.localName);
        body.insertRow().insertCell().textContent = 'section';
        body.insertRow(0); body.deleteRow(-1); out.push(body.rows.length);
        const trulyEmpty = document.createElement('table'); trulyEmpty.insertRow();
        out.push(trulyEmpty.firstElementChild.localName, trulyEmpty.rows[0].localName);
        return out;
    })()"#).await, json!(["tbody","yx",1,"1",true,"tbody",1,"tbody","tr"]));
}

#[tokio::test(flavor = "current_thread")]
async fn table_index_conversion_required_arguments_and_empty_deletion_match_webidl() {
    assert_eq!(evaluate(r#"(() => {
        const t = document.createElement('table'), section = document.createElement('tbody');
        const row = document.createElement('tr');
        const error = fn => { try { fn(); return 'ok'; } catch (e) { return e.name; } };
        const out = [error(() => t.deleteRow(-1)), error(() => row.deleteCell(-1)),
            error(() => section.deleteRow(-1)), error(() => t.deleteRow()),
            error(() => section.deleteRow()), error(() => row.deleteCell()),
            error(() => t.insertRow(1)), error(() => row.insertCell(-2)),
            error(() => t.insertRow(1n)), error(() => row.insertCell(Symbol()))];
        let calls = 0;
        t.insertRow({valueOf(){ calls++; return 0.8; }}).insertCell();
        out.push(calls, t.rows.length, error(() => t.deleteRow(undefined)), t.rows.length);
        t.insertRow(4294967296);
        out.push(t.rows.length, t.insertRow.length, t.deleteRow.length,
            row.insertCell.length, row.deleteCell.length, t.createTBody.length);
        return out;
    })()"#).await, json!(["ok","ok","ok","TypeError","TypeError","TypeError",
        "IndexSizeError","IndexSizeError","TypeError","TypeError",1,1,"ok",0,1,0,1,0,1,0]));
}

#[tokio::test(flavor = "current_thread")]
async fn index_coercion_mutations_precede_collection_range_validation() {
    assert_eq!(evaluate(r#"(() => {
        const error = fn => { try { fn(); return 'ok'; } catch (e) { return e.name; } };
        const section = document.createElement('tbody'); section.insertRow();
        const row = document.createElement('tr'); row.insertCell();
        const out = [
            error(() => section.insertRow({valueOf(){section.deleteRow(-1);return 1;}})),
            section.rows.length,
            error(() => row.insertCell({valueOf(){row.deleteCell(-1);return 1;}})),
            row.cells.length];
        section.insertRow();
        row.insertCell();
        out.push(error(() => section.deleteRow({valueOf(){section.deleteRow(-1);return 0;}})),
            section.rows.length,
            error(() => row.deleteCell({valueOf(){row.deleteCell(-1);return 0;}})),
            row.cells.length);
        const table = document.createElement('table'); table.insertRow();
        out.push(error(() => table.insertRow({valueOf(){table.deleteRow(-1);return 1;}})),
            table.rows.length);
        return out;
    })()"#).await, json!(["IndexSizeError",0,"IndexSizeError",0,
        "IndexSizeError",0,"IndexSizeError",0,"IndexSizeError",0]));
}

#[tokio::test(flavor = "current_thread")]
async fn table_members_reject_wrong_receivers_and_preserve_native_reflection() {
    assert_eq!(evaluate(r#"(() => {
        const interfaces = [[HTMLTableElement,'tBodies'],[HTMLTableSectionElement,'rows'],
            [HTMLTableRowElement,'cells'],[HTMLTableRowElement,'rowIndex'],
            [HTMLTableCellElement,'cellIndex']];
        const invalid = interfaces.map(([C, key]) => {
            const get = Object.getOwnPropertyDescriptor(C.prototype, key).get;
            return [document.createElement('div'), {}, Object.create(C.prototype),
                document.createElementNS('urn:foreign','tr')].every(value => {
                try { get.call(value); return false; } catch (e) { return e instanceof TypeError; }
            });
        });
        const method = HTMLTableRowElement.prototype.insertCell;
        let bad = false;
        try { method.call(document.createElement('div')); } catch(e) { bad = e instanceof TypeError; }
        const descriptor = Object.getOwnPropertyDescriptor(HTMLTableRowElement.prototype, 'cells');
        return [invalid, bad, descriptor.enumerable, descriptor.configurable,
            descriptor.get.name, descriptor.get.toString(), method.toString(),
            ['rows','cells','tBodies','rowIndex','cellIndex'].some(key => key in document.createElement('div'))];
    })()"#).await, json!([[true,true,true,true,true],true,true,true,"get cells",
        "function get cells() { [native code] }","function insertCell() { [native code] }",false]));
}

#[tokio::test(flavor = "current_thread")]
async fn section_setters_keep_standard_placement_and_reject_invalid_values() {
    assert_eq!(evaluate(r#"(() => {
        const t = document.createElement('table');
        t.innerHTML = '<caption></caption><colgroup></colgroup><tbody></tbody><tfoot></tfoot>';
        const bodies = t.tBodies, head = document.createElement('thead'), foot = document.createElement('tfoot');
        t.tHead = head; t.tFoot = foot;
        const order = Array.from(t.children, c => c.localName);
        const error = fn => { try { fn(); return 'ok'; } catch (e) { return e.name; } };
        const invalid = [error(() => t.tHead = document.createElement('tbody')),
            error(() => t.tHead = document.createElementNS('urn:foreign','thead')),
            error(() => t.tFoot = {nodeType:1, localName:'tfoot', namespaceURI:'http://www.w3.org/1999/xhtml'}),
            error(() => t.tHead = {nodeType:1, localName:'div'}),
            error(() => {const body=document.createElement('tbody');
                Object.defineProperty(body,'localName',{value:'thead'});t.tHead=body;})];
        const retained = [t.tHead === head, t.tFoot === foot, bodies.length];
        t.tHead = null; t.tFoot = undefined;
        return [order, invalid, retained, t.tHead, t.tFoot];
    })()"#).await, json!([["caption","colgroup","thead","tbody","tfoot"],
        ["HierarchyRequestError","TypeError","TypeError","TypeError","HierarchyRequestError"],[true,true,1],null,null]));
}
