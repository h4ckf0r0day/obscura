use crate::runtime::ObscuraJsRuntime;
use serde_json::{json, Value};

async fn probe(script: &str) -> Value {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html><body></body>"));
    rt.set_url("https://idb.test/");
    rt.run_page_init();
    rt.execute_script("indexeddb-regression", &format!(r#"
        globalThis.idbResult = null;
        const opened = (name, version = 1, upgrade = db => db.createObjectStore('items')) => new Promise((resolve, reject) => {{
            const r = indexedDB.open(name, version);
            r.onupgradeneeded = () => upgrade(r.result);
            r.onsuccess = () => resolve(r.result);
            r.onerror = () => reject(r.error);
        }});
        const requested = r => new Promise((resolve, reject) => {{
            r.onsuccess = () => resolve(r.result);
            r.onerror = () => reject(r.error);
        }});
        const finished = tx => new Promise(resolve => {{
            tx.oncomplete = () => resolve('complete');
            tx.onabort = () => resolve('abort');
        }});
        (async () => {{ {script} }})().then(
            value => idbResult = value,
            error => idbResult = {{error:error.name, message:error.message}}
        );
    "#)).unwrap();
    rt.run_event_loop_bounded(1000).await.unwrap();
    rt.evaluate("idbResult").unwrap()
}

#[tokio::test]
async fn indexeddb_upgrade_precedes_open_and_requests_precede_commit() {
    let out = probe(r#"
        const events = [];
        const r = indexedDB.open('ordering', 1);
        const db = await new Promise(resolve => {
            r.onupgradeneeded = () => { events.push('upgrade'); r.result.createObjectStore('items'); };
            r.onsuccess = () => { events.push('open'); resolve(r.result); };
        });
        const tx = db.transaction('items', 'readwrite');
        const done = finished(tx).then(() => events.push('complete'));
        tx.objectStore('items').put('kept', 1).onsuccess = () => events.push('put');
        await done;
        const value = await requested(db.transaction('items').objectStore('items').get(1));
        return {events, value, schema:db.objectStoreNames.contains('items')};
    "#).await;
    assert_eq!(out, json!({"events":["upgrade","open","put","complete"],"value":"kept","schema":true}));
}

#[tokio::test]
async fn indexeddb_abort_rolls_back_and_readonly_writes_throw() {
    let out = probe(r#"
        const db = await opened('abort');
        const tx = db.transaction('items', 'readwrite');
        const done = finished(tx);
        const r = tx.objectStore('items').put('discard', 1);
        let requestError;
        r.onerror = () => {requestError = r.error.name;};
        tx.abort();
        const terminal = await done;
        let readonly;
        const read = db.transaction('items');
        const value = await requested(read.objectStore('items').get(1));
        try { db.transaction('items').objectStore('items').put('bad', 2); }
        catch (e) { readonly = e.name; }
        return {terminal, requestError, missing:value === undefined, readonly};
    "#).await;
    assert_eq!(out, json!({"terminal":"abort","requestError":"AbortError","missing":true,"readonly":"ReadOnlyError"}));
}

#[tokio::test]
async fn indexeddb_values_are_cloned_and_duplicate_add_aborts_atomically() {
    let out = probe(r#"
        const db = await opened('clone');
        const tx = db.transaction('items', 'readwrite'), done = finished(tx);
        const source = {n:1, date:new Date(123), bytes:new Uint8Array([0,255]), map:new Map([['a',2]])};
        source.self = source;
        const store = tx.objectStore('items');
        store.put(source, 'clone'); source.n = 99;
        const cloned = await requested(store.get('clone'));
        await done;
        const dup = db.transaction('items', 'readwrite'), dupDone = finished(dup);
        const s = dup.objectStore('items'); s.add('first', 1);
        let error;
        s.add('second', 1).onerror = e => {error = e.target.error.name;};
        const terminal = await dupDone;
        const missing = await requested(db.transaction('items').objectStore('items').get(1));
        return [cloned.n, cloned.date.getTime(), Array.from(cloned.bytes), cloned.map.get('a'),
            cloned.self === cloned, error, terminal, missing === undefined];
    "#).await;
    assert_eq!(out, json!([1,123,[0,255],2,true,"ConstraintError","abort",true]));
}

#[tokio::test]
async fn indexeddb_typed_keys_ranges_and_request_callbacks_share_transaction() {
    let out = probe(r#"
        const db = await opened('keys');
        const tx = db.transaction('items', 'readwrite'), done = finished(tx);
        const s = tx.objectStore('items');
        s.put('number', 1); s.put('string', '1'); s.put('date', new Date(1));
        s.put('array', [1,'a']); s.put('binary', new Uint8Array([1]));
        const first = s.get(1);
        first.onsuccess = () => s.put('callback', 2);
        await done;
        const read = db.transaction('items'), r = read.objectStore('items');
        return [await requested(r.get(1)), await requested(r.get('1')),
            await requested(r.get(new Date(1))), await requested(r.get([1,'a'])),
            await requested(r.get(new Uint8Array([1]))),
            await requested(r.getAll(IDBKeyRange.bound(1,2))),
            await requested(r.count())];
    "#).await;
    assert_eq!(out, json!(["number","string","date","array","binary",["number","callback"],6]));
}

#[tokio::test]
async fn indexeddb_failed_upgrade_keeps_previous_version_and_schema() {
    let out = probe(r#"
        const db = await opened('upgrade'); db.close();
        const r = indexedDB.open('upgrade', 2);
        const error = await new Promise(resolve => {
            r.onupgradeneeded = () => {r.result.createObjectStore('discard'); r.transaction.abort();};
            r.onerror = () => resolve(r.error.name);
            r.onsuccess = () => resolve('unexpected success');
        });
        const old = await requested(indexedDB.open('upgrade'));
        return [error, old.version, old.objectStoreNames.contains('items'), old.objectStoreNames.contains('discard')];
    "#).await;
    assert_eq!(out, json!(["AbortError",1,true,false]));
}

#[tokio::test]
async fn indexeddb_request_error_can_be_canceled_without_aborting() {
    let out = probe(r#"
        const db = await opened('handled');
        const tx = db.transaction('items','readwrite'), done = finished(tx);
        const s = tx.objectStore('items'); s.add('first',1);
        s.add('duplicate',1).onerror = e => e.preventDefault();
        s.put('second',2);
        const terminal = await done;
        return [terminal,await requested(db.transaction('items').objectStore('items').getAll())];
    "#).await;
    assert_eq!(out, json!(["complete",["first","second"]]));
}

#[tokio::test]
async fn indexeddb_unique_indexes_and_cursors_use_generated_keys() {
    let out = probe(r#"
        const request = indexedDB.open('indexed',1);
        const db = await new Promise((resolve,reject) => {
            request.onupgradeneeded = () => request.result.createObjectStore('users',{keyPath:'id',autoIncrement:true})
                .createIndex('email','email',{unique:true});
            request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error);
        });
        const tx = db.transaction('users','readwrite'), done = finished(tx), users = tx.objectStore('users');
        const first = users.add({email:'b@test'}), second = users.add({email:'a@test'});
        const keys = [await requested(first),await requested(second)]; await done;
        const read = db.transaction('users'), index = read.objectStore('users').index('email');
        const found = await requested(index.get('b@test'));
        const rows = await new Promise(resolve => {
            const rows = [], cursor = index.openCursor();
            cursor.onsuccess = () => {const c=cursor.result;if (!c) return resolve(rows);
                rows.push([c.key,c.primaryKey,c.value.id]); c.continue();};
        });
        const bad = db.transaction('users','readwrite'), badDone = finished(bad);
        bad.objectStore('users').add({email:'a@test'});
        return [keys,found.id,rows,await badDone];
    "#).await;
    assert_eq!(out, json!([[1,2],1,[["a@test",2,2],["b@test",1,1]],"abort"]));
}

#[tokio::test]
async fn indexeddb_waiting_upgrade_notifies_and_respects_closed_connections() {
    let out = probe(r#"
        const first = await opened('connections'), events = [];
        first.onversionchange = e => {events.push([e.oldVersion,e.newVersion]);first.close();};
        const r = indexedDB.open('connections',2);
        const upgraded = await new Promise((resolve,reject) => {
            r.onupgradeneeded = () => {events.push('upgrade');r.result.createObjectStore('second');};
            r.onsuccess = () => resolve(r.result);r.onerror=()=>reject(r.error);
        });
        return [events,upgraded.version,upgraded.objectStoreNames.contains('second')];
    "#).await;
    assert_eq!(out, json!([[[1,2],"upgrade"],2,true]));
}

#[tokio::test]
async fn indexeddb_transaction_is_inactive_in_unrelated_timer_tasks() {
    let out = probe(r#"
        const db = await opened('inactive');
        const tx = db.transaction('items','readwrite');
        const s = tx.objectStore('items');
        s.put('first',1);
        return await new Promise(resolve => setTimeout(() => {
            try {s.put('late',2);resolve('accepted');}catch(e){resolve(e.name);}
        },0));
    "#).await;
    assert_eq!(out, json!("TransactionInactiveError"));
}

#[tokio::test]
async fn indexeddb_rejects_dom_nodes_instead_of_storing_a_plain_object() {
    let out = probe(r#"
        const db = await opened('uncloneable');
        const tx = db.transaction('items','readwrite');
        try {tx.objectStore('items').put({nested:document.body},1);return 'accepted';}
        catch(e){return e.name;}
    "#).await;
    assert_eq!(out, json!("DataCloneError"));
}

#[tokio::test]
async fn indexeddb_cross_document_versionchange_is_delivered_by_the_event_loop() {
    let storage = std::sync::Arc::new(crate::indexed_db::IndexedDbStorage::default());
    let setup = |storage| {
        let mut rt = ObscuraJsRuntime::new();
        rt.set_dom(obscura_dom::parse_html("<!doctype html><body></body>"));
        rt.set_url("https://idb.test/");
        rt.set_indexed_db(storage);
        rt.run_page_init();
        rt
    };
    let mut first = setup(storage.clone());
    first.execute_script("first", r#"
        globalThis.notified = false;
        const request = indexedDB.open('cross-document',1);
        request.onsuccess = () => { const db = request.result;
            db.onversionchange = () => {notified = true; db.close();}; };
    "#).unwrap();
    first.run_event_loop_bounded(100).await.unwrap();
    let mut second = setup(storage);
    second.execute_script("second", r#"
        globalThis.version = 0;
        const request = indexedDB.open('cross-document',2);
        request.onsuccess = () => version = request.result.version;
    "#).unwrap();
    second.run_event_loop_bounded(10).await.unwrap();
    first.run_event_loop_bounded(100).await.unwrap();
    assert_eq!(first.evaluate("notified").unwrap(), json!(true));
    second.run_event_loop_bounded(100).await.unwrap();
    assert_eq!(second.evaluate("version").unwrap().as_f64(), Some(2.0));
}

#[tokio::test]
async fn indexeddb_failed_unique_insert_does_not_consume_an_auto_increment_key() {
    let out = probe(r#"
        const r = indexedDB.open('generator',1);
        const db = await new Promise(resolve => {
            r.onupgradeneeded = () => r.result.createObjectStore('items',{autoIncrement:true}).createIndex('unique','x',{unique:true});
            r.onsuccess = () => resolve(r.result);
        });
        const tx = db.transaction('items','readwrite'), done = finished(tx), s = tx.objectStore('items');
        s.add({x:1}); s.add({x:1}).onerror = e => e.preventDefault();
        const key = await requested(s.add({x:2})); await done;
        return key;
    "#).await;
    assert_eq!(out.as_f64(),Some(2.0));
}

#[tokio::test]
async fn indexeddb_handlers_keep_listener_registration_order() {
    let out = probe(r#"
        const r = indexedDB.open('listeners',1), events = [];
        r.addEventListener('success', () => events.push('listener'));
        r.onsuccess = () => events.push('handler');
        await new Promise(resolve => r.addEventListener('success',resolve));
        return [events,r instanceof EventTarget,Object.prototype.toString.call(r)];
    "#).await;
    assert_eq!(out,json!([["listener","handler"],true,"[object IDBOpenDBRequest]"]));
}

#[tokio::test]
async fn indexeddb_new_javascript_task_cannot_add_to_an_existing_transaction() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<!doctype html>"));
    rt.set_url("https://idb.test/");
    rt.run_page_init();
    rt.execute_script("open",r#"
        globalThis.db = null;
        const request = indexedDB.open('task',1);
        request.onupgradeneeded = () => request.result.createObjectStore('items');
        request.onsuccess = () => db = request.result;
    "#).unwrap();
    rt.run_event_loop_bounded(100).await.unwrap();
    rt.execute_script("first-task",r#"
        globalThis.store = db.transaction('items','readwrite').objectStore('items');
        store.put('first',1);
    "#).unwrap();
    let result = rt.evaluate("(() => {try {store.put('late',2);return 'accepted';}catch(e){return e.name;}})()").unwrap();
    assert_eq!(result,json!("TransactionInactiveError"));
}

#[tokio::test]
async fn indexeddb_clones_files_and_blob_aliases_without_losing_their_interfaces() {
    let out = probe(r#"
        const db = await opened('files');
        const tx = db.transaction('items','readwrite'), done = finished(tx);
        const file = new File([new Uint8Array([0,255,117])],'fixture.bin',{type:'application/octet-stream',lastModified:123});
        tx.objectStore('items').put({file,alias:file,map:new Map([['file',file]])},1); await done;
        const copy = await requested(db.transaction('items').objectStore('items').get(1));
        return [copy.file instanceof File,copy.file.name,copy.file.type,copy.file.lastModified,
            Array.from(new Uint8Array(await copy.file.arrayBuffer())),copy.alias === copy.file,copy.map.get('file') === copy.file];
    "#).await;
    assert_eq!(out,json!([true,"fixture.bin","application/octet-stream",123,[0,255,117],true,true]));
}

#[tokio::test]
async fn indexeddb_cloned_nonextractable_crypto_keys_keep_their_material_and_permissions() {
    let out = probe(r#"
        const key = await crypto.subtle.importKey('raw',new Uint8Array([1,2,3]),{name:'HMAC',hash:'SHA-256'},false,['sign']);
        const expected = new Uint8Array(await crypto.subtle.sign('HMAC',key,new Uint8Array([4,5,6])));
        const db = await opened('keys-storage');
        const tx = db.transaction('items','readwrite'), done = finished(tx);
        tx.objectStore('items').put({key},1); await done;
        const copy = (await requested(db.transaction('items').objectStore('items').get(1))).key;
        const actual = new Uint8Array(await crypto.subtle.sign('HMAC',copy,new Uint8Array([4,5,6])));
        let rejected;
        try {await crypto.subtle.exportKey('raw',copy);rejected=false;}catch(e){rejected=true;}
        return [copy instanceof CryptoKey,copy.extractable,copy.usages,actual.length,
            actual.every((b,i) => b === expected[i]),rejected];
    "#).await;
    assert_eq!(out,json!([true,false,["sign"],32,true,true]));
}

#[tokio::test]
async fn indexeddb_empty_upgrade_completes_and_index_metadata_outlives_transactions() {
    let out = probe(r#"
        const empty = await new Promise((resolve,reject) => {
            const req = indexedDB.open('empty');
            req.onsuccess = () => resolve(req.result); req.onerror = () => reject(req.error);
        });
        const db = await opened('metadata',1,db => {
            db.createObjectStore('items').createIndex('email','email',{unique:true});
        });
        const tx = db.transaction('items'), done = finished(tx), index = tx.objectStore('items').index('email');
        await done;
        return [empty.version,empty.objectStoreNames.length,index.keyPath,index.unique,index.multiEntry];
    "#).await;
    assert_eq!(out,json!([1,0,"email",true,false]));
}
