use obscura::Browser;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn serve(body: String) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = body.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let bytes = stream.read(&mut request).await.unwrap();
                if request[..bytes].starts_with(b"GET /pending ") {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len(),
                );
                stream.write_all(header.as_bytes()).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
    });
    format!("http://{address}")
}

#[tokio::test]
async fn pending_replacement_must_not_alias_a_retired_sibling() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let foreign = serve("<!doctype html><p>child</p>".into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe name="a" src="{foreign}/a"></iframe>
        <iframe id="b" name="b" src="{foreign}/b"></iframe>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(500).await;
    let a = page.frame_urls().iter().position(|url| url.ends_with("/a")).unwrap();
    assert_eq!(
        page.evaluate_in_frame(a,
            "(() => { globalThis.retiredSibling = parent.frames.b; return true; })()").unwrap(),
        json!(true),
    );
    assert_eq!(page.evaluate(&format!(
        r#"(() => {{
            document.querySelector('#b').remove();
            const replacement = document.createElement('iframe');
            replacement.name = 'replacement';
            replacement.src = '{foreign}/pending';
            document.body.appendChild(replacement);
            return replacement.isConnected;
        }})()"#,
    )), json!(true));
    assert_eq!(
        page.evaluate_in_frame(a,
            "[parent.frames.length, parent.frames[0] === self, parent.frames[1] === retiredSibling]"
        ).unwrap(),
        json!([2, true, false]),
    );
}

fn child_index(urls: &[String], suffix: &str) -> usize {
    urls.iter().position(|url| url.ends_with(suffix)).unwrap()
}

#[tokio::test]
async fn remote_parent_exposes_siblings_and_preserves_message_sources() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let foreign = serve(r#"<!doctype html><p>child</p><script>
        globalThis.initial = [parent.frames.length, parent.length,
            parent.frames.a === (location.pathname === '/a' ? self : parent.frames[0]),
            typeof parent.frames.b, parent.frames === parent, parent.top === top];
        globalThis.received = [];
        addEventListener('message', event => {
            received.push([event.data, event.source === parent.frames.a,
                event.source === parent.frames.b, event.origin]);
            if (event.data === 'ping') event.source.postMessage('pong', event.origin);
        });
    </script>"#.into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe name="a" src="{foreign}/a"></iframe>
        <iframe name="b" src="{foreign}/b"></iframe>
        <iframe name="c" srcdoc="<p>same-origin</p>"></iframe>
        <script>globalThis.earlyFrameCount = frames.length;</script>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(1000).await;
    assert_eq!(page.evaluate("earlyFrameCount").as_f64(), Some(3.0));
    let urls = page.frame_urls();
    let a = child_index(&urls, "/a");
    let b = child_index(&urls, "/b");
    for index in [a, b] {
        assert_eq!(page.evaluate_in_frame(index, "initial").unwrap(),
            json!([3, 3, true, "object", true, true]));
        assert_eq!(page.evaluate_in_frame(index, r#"[
            parent.frames[0] === parent.frames.a,
            parent.frames[1] === parent.frames.b,
            parent.frames.b.parent === parent,
            parent.frames.b.top === top,
            parent.frames.b.frames === parent.frames.b,
            parent.frames.b.closed,
            (() => { try { parent.frames[99]; return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            0 in parent.frames,
            (() => { try { 'b' in parent.frames; return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            (() => { try { parent.document; return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            Object.keys(parent).join(','),
            (() => { try { parent.constructor; return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            (() => { try { parent.toString; return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            parent.then === undefined,
            parent[Symbol.toStringTag] === undefined,
            (() => { try { 'then' in parent; return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            Symbol.toStringTag in parent,
            (() => { const descriptor = Object.getOwnPropertyDescriptor(parent, 'b');
                return [descriptor.value === parent.frames[1], descriptor.enumerable,
                    descriptor.configurable, descriptor.writable]; })(),
            Object.getPrototypeOf(parent) === null,
            (() => { try { Reflect.setPrototypeOf(parent, {}); return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            (() => { try { Reflect.defineProperty(parent, 'forbidden', {value:1}); return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            (() => { try { Reflect.deleteProperty(parent, 'forbidden'); return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            (() => { try { Reflect.set(parent, 'forbidden', 1); return false; } catch (e) { return e.name === 'SecurityError'; } })(),
            (() => { try { Reflect.preventExtensions(parent); return false; } catch (e) { return e.name === 'SecurityError'; } })()
        ]"#).unwrap(), json!([true, true, true, true, true, false, true, true, true, true,
            "0,1,2", true, true, true, true, true, true, [true, false, true, false],
            true, true, true, true, true, true]));
    }
    page.evaluate_in_frame(a,
        "(() => { parent.frames.b.postMessage('ping', location.origin); return true; })()").unwrap();
    page.settle(200).await;
    assert_eq!(page.evaluate_in_frame(b, "received").unwrap(),
        json!([["ping", true, false, foreign]]));
    assert_eq!(page.evaluate_in_frame(a, "received").unwrap(),
        json!([["pong", false, true, foreign]]));
    page.evaluate_in_frame(a,
        "(() => { parent.frames.b.postMessage('wrong-target', 'https://unrelated.example'); return true; })()").unwrap();
    page.settle(100).await;
    assert_eq!(page.evaluate_in_frame(b, "received.length").unwrap(), json!(1));
}

#[tokio::test]
async fn remote_siblings_track_connection_order_names_and_retired_contexts() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let foreign = serve("<!doctype html><p>child</p>".into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe id="a" name="a" src="{foreign}/a"></iframe>
        <iframe id="b" name="b" src="{foreign}/b"></iframe>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(1000).await;
    let a = child_index(&page.frame_urls(), "/a");
    page.evaluate_in_frame(a, "(() => { globalThis.oldSibling = parent.frames.b; return true; })()").unwrap();
    assert_eq!(page.evaluate("(() => { document.body.insertBefore(document.querySelector('#b'), document.querySelector('#a')); return true; })()"), json!(true));
    page.settle(500).await;
    let a = child_index(&page.frame_urls(), "/a");
    assert_eq!(page.evaluate_in_frame(a,
        "[parent.length, parent.frames[0] === self, parent.frames[1] === parent.frames.b, oldSibling.closed]"
    ).unwrap(), json!([2, true, true, true]));
    assert_eq!(page.evaluate("document.querySelector('#a').name = 'b'"), json!("b"));
    assert_eq!(page.evaluate_in_frame(a, "parent.frames.b === parent.frames[0]").unwrap(), json!(false));
    assert_eq!(page.evaluate("document.querySelector('#b').name = 'renamed'"), json!("renamed"));
    assert_eq!(page.evaluate_in_frame(a,
        "[name, (() => { try { parent.frames.b; return false; } catch (e) { return e.name === 'SecurityError'; } })(), (() => { try { parent.frames.renamed; return false; } catch (e) { return e.name === 'SecurityError'; } })()]"
    ).unwrap(), json!(["a", true, true]));
    assert_eq!(page.evaluate_in_frame(a, "name = 'b'").unwrap(), json!("b"));
    let b = child_index(&page.frame_urls(), "/b");
    assert_eq!(page.evaluate_in_frame(b, "name = 'renamed'").unwrap(), json!("renamed"));
    assert_eq!(page.evaluate_in_frame(a,
        "[parent.frames.b === self, parent.frames.renamed === parent.frames[1], (() => { try { parent.frames.missing; return false; } catch (e) { return e.name === 'SecurityError'; } })()]"
    ).unwrap(), json!([true, true, true]));
    assert_eq!(page.evaluate("(() => { globalThis.savedOwner = document.querySelector('#b'); savedOwner.remove(); return !savedOwner.isConnected; })()"), json!(true));
    assert_eq!(page.evaluate_in_frame(a,
        "[parent.length, parent.frames[0] === self, (() => { try { parent.frames.renamed; return false; } catch (e) { return e.name === 'SecurityError'; } })()]"
    ).unwrap(), json!([1, true, true]));
    assert_eq!(page.evaluate("(() => { document.body.appendChild(savedOwner); return savedOwner.isConnected; })()"), json!(true));
    page.settle(500).await;
    let a = child_index(&page.frame_urls(), "/a");
    assert_eq!(page.evaluate_in_frame(a,
        "[parent.length, parent.frames[0] === self, parent.frames.renamed === parent.frames[1], oldSibling.closed]"
    ).unwrap(), json!([2, true, true, true]));
}

#[tokio::test]
async fn opaque_nested_frame_resolves_its_actual_parent_and_top() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let parent = serve(r#"<!doctype html><iframe name="outer" sandbox="allow-scripts"
        srcdoc="&lt;iframe name='inner' sandbox='allow-scripts' srcdoc='&lt;p&gt;nested&lt;/p&gt;'&gt;&lt;/iframe&gt;"></iframe>"#.into()).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(500).await;
    let nested = (0..page.frame_urls().len()).find(|index|
        page.evaluate_in_frame(*index, "document.querySelector('p') !== null").unwrap() == json!(true)
    ).unwrap();
    assert_eq!(page.evaluate_in_frame(nested, r#"[
        parent !== top, parent.parent === top, parent.top === top,
        parent.frames[0] === self, parent.length,
        top.frames.outer === parent, top.length
    ]"#).unwrap(), json!([true, true, true, true, 1, true, 1]));
}

#[tokio::test]
async fn remote_proxy_survives_navigation_and_posts_to_the_current_document() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let foreign = serve(r#"<!doctype html><script>
        globalThis.received = [];
        addEventListener('message', event => received.push(event.data));
    </script>"#.into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe name="a" src="{foreign}/a"></iframe>
        <iframe id="b" name="b" src="{foreign}/b"></iframe>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(500).await;
    let a = child_index(&page.frame_urls(), "/a");
    page.evaluate_in_frame(a,
        "(() => { globalThis.savedSibling = parent.frames.b; return true; })()").unwrap();
    assert_eq!(page.evaluate(&format!(
        "document.querySelector('#b').src = '{foreign}/b-next'"
    )), json!(format!("{foreign}/b-next")));
    page.settle(500).await;
    let a = child_index(&page.frame_urls(), "/a");
    assert_eq!(page.evaluate_in_frame(a,
        "[savedSibling === parent.frames.b, savedSibling.closed, parent.frames[0] === self]"
    ).unwrap(), json!([true, false, true]));
    page.evaluate_in_frame(a,
        "(() => { savedSibling.postMessage('new-document', '*'); return true; })()").unwrap();
    page.settle(100).await;
    let b = child_index(&page.frame_urls(), "/b-next");
    assert_eq!(page.evaluate_in_frame(b, "received").unwrap(), json!(["new-document"]));
}

#[tokio::test]
async fn queued_message_keeps_its_source_identity_after_the_sender_is_removed() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let foreign = serve("<!doctype html><p>child</p>".into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe id="a" name="a" src="{foreign}/a"></iframe>
        <iframe name="b" src="{foreign}/b"></iframe>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(500).await;
    let a = child_index(&page.frame_urls(), "/a");
    let b = child_index(&page.frame_urls(), "/b");
    page.evaluate_in_frame(b, r#"(() => {
        globalThis.savedSource = parent.frames.a;
        globalThis.received = [];
        addEventListener('message', event => received.push([
            event.data, event.source === savedSource, event.source?.closed,
            savedSource.self === null, savedSource.window === null,
            savedSource.frames === null, savedSource.parent === null,
            savedSource.top === null, savedSource.length, savedSource.opener === null
        ]));
        return true;
    })()"#).unwrap();
    page.evaluate_in_frame(a,
        "(() => { parent.frames.b.postMessage('before-removal', '*'); return true; })()").unwrap();
    assert_eq!(page.evaluate(
        "(() => { document.querySelector('#a').remove(); return true; })()"
    ), json!(true));
    page.settle(100).await;
    let b = child_index(&page.frame_urls(), "/b");
    assert_eq!(page.evaluate_in_frame(b, "received").unwrap(),
        json!([["before-removal", true, true, true, true, true, true, true, 0, true]]));
}

#[tokio::test]
async fn root_and_remote_named_access_share_context_order_and_name_matching() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    let foreign = serve("<!doctype html><p>child</p>".into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe id="a" name="a" src="{foreign}/a"></iframe>
        <iframe id="b" name="b" src="{foreign}/b"></iframe>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(500).await;
    assert_eq!(page.evaluate(r#"(() => {
        globalThis.ownerA = document.querySelector('#a');
        globalThis.ownerB = document.querySelector('#b');
        document.body.insertBefore(ownerB, ownerA);
        ownerA.name = 'b';
        ownerB.name = 'renamed';
        return true;
    })()"#), json!(true));
    page.settle(500).await;
    assert_eq!(page.evaluate(
        "[frames.b === ownerA.contentWindow, frames.b === ownerB.contentWindow, frames.renamed === undefined, frames.a === ownerA.contentWindow]"
    ), json!([false, false, true, false]));
    assert_eq!(page.evaluate("ownerB.name = 'b'"), json!("b"));
    let a = child_index(&page.frame_urls(), "/a");
    assert_eq!(page.evaluate_in_frame(a, "name = 'b'").unwrap(), json!("b"));
    assert_eq!(page.evaluate_in_frame(a,
        "[parent.frames.b === self, parent.frames[0] === self]"
    ).unwrap(), json!([true, true]));
    assert_eq!(page.evaluate("frames.b === ownerA.contentWindow"), json!(true));
    assert_eq!(page.evaluate(
        "(() => {ownerA.id = 'fallback'; ownerA.name = 'fallback'; return true;})()"
    ), json!(true));
    assert_eq!(page.evaluate("frames.fallback === ownerA"), json!(true));
}

#[tokio::test]
async fn rejected_frame_keeps_metadata_without_aliasing_root_or_retaining_realm_references() {
    std::env::set_var("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    std::env::set_var("OBSCURA_MAX_LIVE_FRAMES", "1");
    let foreign = serve("<!doctype html><p>child</p>".into()).await;
    let parent = serve(format!(
        r#"<!doctype html><iframe name="a" src="{foreign}/a"></iframe>
        <iframe name="b" src="{foreign}/b"></iframe>"#,
    )).await;
    let browser = Browser::new().unwrap();
    let mut page = browser.new_page().await.unwrap();
    page.goto(&parent).await.unwrap();
    page.settle(500).await;
    assert_eq!(page.frame_urls().len(), 1);
    assert_eq!(page.evaluate(r#"(() => {
        const owners = Array.from(document.querySelectorAll('iframe'));
        const pending = owners.find(owner => !owner._frameId);
        let conversions = 0;
        pending.contentWindow.postMessage({get value(){conversions++;return 7;}},
            {toString(){conversions++;return '*';}});
        const cyclic = {}; cyclic.self = cyclic;
        let cloneError = false;
        try {pending.contentWindow.postMessage(cyclic, '*');}
        catch (error) {cloneError = error.name === 'DataCloneError';}
        return [frames.length, pending.contentWindow !== window,
            pending._iframeWin !== window,
            Object.keys(__obscura_frameWindows).length,
            Object.keys(__obscura_frameElements).length, conversions, cloneError,
            pending.contentWindow === frames[owners.indexOf(pending)]];
    })()"#), json!([2, true, true, 1, 1, 2, true, true]));
}
