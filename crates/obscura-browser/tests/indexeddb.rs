use std::io::{Read, Write};
use std::sync::Arc;

use obscura_browser::{BrowserContext, Page};
use serde_json::{json, Value};

fn spawn_html_server() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            std::thread::spawn(move || {
                let mut request = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if request.windows(4).any(|b| b == b"\r\n\r\n") {
                        break;
                    }
                }
                let body = "<!doctype html><html><body>idb fixture</body></html>";
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
        }
    });
    format!("http://{address}")
}

fn context(id: &str, storage_dir: Option<std::path::PathBuf>) -> Arc<BrowserContext> {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    Arc::new(BrowserContext::with_storage_and_network(id.into(), None, false, None, storage_dir, true))
}

/// Run an async script body; it must eventually call `done(value)`.
async fn run(page: &mut Page, body: &str) -> Value {
    page.evaluate(&format!(
        "(function() {{ window.__result = undefined; const done = (v) => {{ window.__result = JSON.stringify(v); }}; \
         const fail = (e) => done('FAIL: ' + (e && (e.name + ': ' + e.message) || e)); \
         try {{ {body} }} catch (e) {{ fail(e); }} return 1; }})()"
    ));
    for _ in 0..100 {
        page.settle(50).await;
        if let Some(s) = page.evaluate("window.__result").as_str() {
            return serde_json::from_str(s).unwrap();
        }
    }
    panic!("script never called done()");
}

const PROMISIFY: &str = r#"
    const p = (req) => new Promise((res, rej) => { req.onsuccess = () => res(req.result); req.onerror = () => rej(req.error); });
    const openDb = () => new Promise((res, rej) => {
        const req = indexedDB.open('app', 1);
        req.onupgradeneeded = (e) => {
            const db = req.result;
            const s = db.createObjectStore('people', { keyPath: 'id', autoIncrement: true });
            s.createIndex('by_email', 'email', { unique: true });
            s.createIndex('by_tag', 'tags', { multiEntry: true });
            s.createIndex('by_age', 'age');
            window.upgrade = e.oldVersion + '->' + e.newVersion;
        };
        req.onsuccess = () => res(req.result);
        req.onerror = () => rej(req.error);
    });
"#;

#[tokio::test(flavor = "current_thread")]
async fn stores_indexes_cursors_and_ranges() {
    let origin = spawn_html_server();
    let mut page = Page::new("idb-basic".into(), context("idb-basic", None));
    page.navigate(&format!("{origin}/")).await.unwrap();

    let result = run(&mut page, &format!(r#"{PROMISIFY}
        (async () => {{
            const db = await openDb();
            let tx = db.transaction('people', 'readwrite');
            let s = tx.objectStore('people');
            const ids = [];
            for (const person of [
                {{ name: 'Ann', email: 'ann@x', tags: ['a', 'b', 'a'], age: 30 }},
                {{ name: 'Bob', email: 'bob@x', tags: ['b'], age: 25 }},
                {{ name: 'Cy', email: 'cy@x', tags: [], age: 30, when: new Date(5) }},
            ]) ids.push(await p(s.add(person)));
            await new Promise((r) => {{ tx.oncomplete = r; }});

            tx = db.transaction(['people']);
            s = tx.objectStore('people');
            const all = await p(s.getAll());
            const byEmail = await p(s.index('by_email').get('bob@x'));
            const tagB = await p(s.index('by_tag').getAllKeys('b'));
            const age30 = await p(s.index('by_age').count(30));
            const range = await p(s.getAllKeys(IDBKeyRange.bound(1, 3, true, false)));
            const names = [];
            await new Promise((r) => {{
                const req = s.index('by_age').openCursor(null, 'prev');
                req.onsuccess = () => {{ const c = req.result; if (!c) return r(); names.push(c.value.name + '@' + c.key); c.continue(); }};
            }});
            const unique = [];
            await new Promise((r) => {{
                const req = s.index('by_age').openKeyCursor(null, 'nextunique');
                req.onsuccess = () => {{ const c = req.result; if (!c) return r(); unique.push(c.key + ':' + c.primaryKey); c.continue(); }};
            }});
            done({{
                upgrade, ids, count: all.length, firstId: all[0].id, date: all[2].when instanceof Date && all[2].when.getTime(),
                byEmail: byEmail.name, tagB, age30, range, names, unique,
                stores: [...db.objectStoreNames], indexes: [...s.indexNames],
                cmp: [indexedDB.cmp(1, 'a'), indexedDB.cmp([1], 'z'), indexedDB.cmp('b', 'a')],
            }});
        }})().catch(fail);
    "#)).await;

    assert_eq!(
        result,
        json!({
            "upgrade": "0->1", "ids": [1, 2, 3], "count": 3, "firstId": 1, "date": 5,
            "byEmail": "Bob", "tagB": [1, 2], "age30": 2, "range": [2, 3],
            "names": ["Cy@30", "Ann@30", "Bob@25"], "unique": ["25:2", "30:1"],
            "stores": ["people"], "indexes": ["by_age", "by_email", "by_tag"],
            "cmp": [-1, 1, 1],
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn constraint_errors_and_abort_roll_back() {
    let origin = spawn_html_server();
    let mut page = Page::new("idb-abort".into(), context("idb-abort", None));
    page.navigate(&format!("{origin}/")).await.unwrap();

    let result = run(&mut page, &format!(r#"{PROMISIFY}
        (async () => {{
            const db = await openDb();
            const out = {{}};
            let tx = db.transaction('people', 'readwrite');
            tx.objectStore('people').put({{ id: 1, email: 'a@x', age: 1 }});
            await new Promise((r) => {{ tx.oncomplete = r; }});

            // A failed add aborts the transaction unless the error is handled.
            tx = db.transaction('people', 'readwrite');
            const s = tx.objectStore('people');
            s.put({{ id: 2, email: 'b@x', age: 2 }});
            const dup = s.add({{ id: 1, email: 'z@x', age: 3 }});
            dup.onerror = (e) => {{ out.dupError = dup.error.name; }};
            await new Promise((r) => {{ tx.onabort = () => {{ out.abortError = tx.error.name; r(); }}; }});

            tx = db.transaction('people', 'readwrite');
            const s2 = tx.objectStore('people');
            const clash = s2.put({{ id: 3, email: 'a@x', age: 3 }});
            clash.onerror = (e) => {{ out.uniqueError = clash.error.name; e.preventDefault(); }};
            s2.put({{ id: 4, email: 'd@x', age: 4 }});
            await new Promise((r) => {{ tx.oncomplete = r; }});

            tx = db.transaction('people', 'readwrite');
            tx.objectStore('people').put({{ id: 5, email: 'e@x', age: 5 }});
            tx.abort();
            await new Promise((r) => {{ tx.onabort = r; }});

            const keys = await p(db.transaction('people').objectStore('people').getAllKeys());
            out.keys = keys;
            try {{ db.transaction('people').objectStore('people').put({{ id: 9 }}); }} catch (e) {{ out.readOnly = e.name; }}
            // With no requests queued the transaction commits once its task
            // ends; using it afterwards is an InvalidStateError, as in Chrome.
            const late = db.transaction('people');
            await new Promise((r) => setTimeout(r, 0));
            try {{ late.objectStore('people'); }} catch (e) {{ out.finished = e.name; }}
            done(out);
        }})().catch(fail);
    "#)).await;

    assert_eq!(
        result,
        json!({
            "dupError": "ConstraintError", "abortError": "ConstraintError",
            "uniqueError": "ConstraintError", "keys": [1, 4],
            "readOnly": "ReadOnlyError", "finished": "InvalidStateError",
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn databases_persist_across_navigation_and_storage_dir() {
    let origin = spawn_html_server();
    let dir = std::env::temp_dir().join(format!("obscura-idb-persist-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let write = format!(r#"{PROMISIFY}
        (async () => {{
            const db = await openDb();
            const tx = db.transaction('people', 'readwrite');
            tx.objectStore('people').put({{ name: 'Kept', email: 'k@x', tags: ['t'], age: 9, bytes: new Uint8Array([1, 2]), m: new Map([[1, 'one']]) }});
            await new Promise((r) => {{ tx.oncomplete = r; }});
            db.close();
            done('ok');
        }})().catch(fail);
    "#);
    let read = format!(r#"{PROMISIFY}
        (async () => {{
            window.upgrade = 'none';
            const db = await openDb();
            const s = db.transaction('people').objectStore('people');
            const v = await p(s.index('by_tag').get('t'));
            const list = await indexedDB.databases();
            done({{ upgrade, name: v && v.name, bytes: v && Array.from(v.bytes), map: v && v.m.get(1), list }});
        }})().catch(fail);
    "#);
    let expected = json!({"upgrade": "none", "name": "Kept", "bytes": [1, 2], "map": "one", "list": [{"name": "app", "version": 1}]});

    {
        let ctx = context("idb-persist-1", Some(dir.clone()));
        let mut page = Page::new("idb-persist-1".into(), ctx.clone());
        page.navigate(&format!("{origin}/a")).await.unwrap();
        assert_eq!(run(&mut page, &write).await, json!("ok"));
        // Same context, new document.
        page.navigate(&format!("{origin}/b")).await.unwrap();
        assert_eq!(run(&mut page, &read).await, expected);
        ctx.save_cookies();
    }

    let mut page = Page::new("idb-persist-2".into(), context("idb-persist-2", Some(dir.clone())));
    page.navigate(&format!("{origin}/c")).await.unwrap();
    assert_eq!(run(&mut page, &read).await, expected);

    let deleted = run(&mut page, r#"
        const req = indexedDB.deleteDatabase('app');
        req.onsuccess = async () => done(await indexedDB.databases());
    "#).await;
    assert_eq!(deleted, json!([]));
    let _ = std::fs::remove_dir_all(&dir);
}
