use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(ctx: &mut CdpContext, method: &str, params: Value) -> Value {
    let response = dispatch(
        &CdpRequest {
            id: 1,
            method: method.to_string(),
            params,
            session_id: Some("ax-session".to_string()),
        },
        ctx,
    )
    .await;
    assert!(response.error.is_none(), "{method}: {:?}", response.error);
    response.result.unwrap()
}

async fn setup(html: &str) -> CdpContext {
    let mut ctx = CdpContext::new();
    let page = ctx.create_page();
    ctx.sessions.insert("ax-session".to_string(), page);
    cdp(&mut ctx, "Page.navigate", json!({"url": "about:blank"})).await;
    cdp(&mut ctx, "Runtime.evaluate", json!({
        "expression": format!("document.body.innerHTML = {}", serde_json::to_string(html).unwrap()),
        "returnByValue": true
    })).await;
    ctx
}

#[tokio::test(flavor = "current_thread")]
async fn accessibility_names_use_live_dom_and_backend_ids() {
    let mut ctx = setup(
        r#"<label for="field">Email</label><input id="field">
        <button id="save">Save <b>changes</b></button><a href="/">Read docs</a>"#,
    )
    .await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let nodes = tree["nodes"].as_array().unwrap();
    for (role, name) in [
        ("textbox", "Email"),
        ("button", "Save changes"),
        ("link", "Read docs"),
    ] {
        assert!(
            nodes
                .iter()
                .any(|node| node["role"]["value"] == role && node["name"]["value"] == name),
            "{tree}"
        );
    }
    let button = nodes
        .iter()
        .find(|node| node["role"]["value"] == "button")
        .unwrap();
    let resolved = cdp(
        &mut ctx,
        "DOM.resolveNode",
        json!({"backendNodeId": button["backendDOMNodeId"]}),
    )
    .await;
    assert!(resolved["object"]["objectId"].is_string(), "{resolved}");
    cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('save').textContent = 'Updated'"}),
    )
    .await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    assert!(tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["role"]["value"] == "button" && node["name"]["value"] == "Updated"));
}

#[tokio::test(flavor = "current_thread")]
async fn accessibility_labels_preserve_link_text() {
    let mut ctx = setup(
        r#"<span id="label">Read <a href="/terms">terms</a></span>
        <button aria-labelledby="label" aria-label="Fallback">Continue</button>
        <label for="field">Accept <a href="/terms">terms</a> and <span role="link">privacy</span><select><option>Noise</option></select><span role="button">Nested action</span></label>
        <input id="field">"#,
    )
    .await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let nodes = tree["nodes"].as_array().unwrap();
    for (role, name) in [
        ("button", "Read terms"),
        ("textbox", "Accept terms and privacy"),
    ] {
        assert!(
            nodes
                .iter()
                .any(|node| node["role"]["value"] == role && node["name"]["value"] == name),
            "{tree}"
        );
    }
}

#[cfg(feature = "render")]
#[tokio::test(flavor = "current_thread")]
async fn accessibility_names_and_visibility_use_the_live_cascade() {
    let mut ctx = setup(r#"<style>
        .gone { display: none !important }
        .invisible { visibility: hidden }
        .restored { visibility: visible }
    </style>
    <div class="gone" style="display:block"><button>Display hidden</button></div>
    <div class="invisible">Hidden text<button>Inherited hidden</button><button class="restored">Restored</button></div>
    <button style="opacity:0">Transparent</button>
    <button style="position:absolute;left:-10000px">Offscreen</button>
    <span id="label" class="gone">Hidden <span hidden>reference</span></span>
    <button aria-labelledby="label">Fallback</button>
    <label class="gone" for="field">Hidden native <span hidden>label</span></label><input id="field">
    <button id="dynamic">Before<span class="gone"> noise</span></button>"#).await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    let nodes = tree["nodes"].as_array().unwrap();
    let names: Vec<_> = nodes
        .iter()
        .filter(|node| node["role"]["value"] == "button")
        .map(|node| node["name"]["value"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        names,
        [
            "Restored",
            "Transparent",
            "Offscreen",
            "Hidden reference",
            "Before"
        ]
    );
    assert!(nodes
        .iter()
        .any(|node| node["role"]["value"] == "textbox"
            && node["name"]["value"] == "Hidden native label"));
    for node in nodes {
        if let Some(parent) = node["parentId"].as_str() {
            let parent = nodes.iter().find(|node| node["nodeId"] == parent).unwrap();
            assert!(
                parent["childIds"]
                    .as_array()
                    .unwrap()
                    .contains(&node["nodeId"]),
                "{tree}"
            );
        }
        if let Some(children) = node["childIds"].as_array() {
            for child in children {
                let child = nodes.iter().find(|node| node["nodeId"] == *child).unwrap();
                assert_eq!(child["parentId"], node["nodeId"]);
            }
        }
    }
    cdp(
        &mut ctx,
        "Runtime.evaluate",
        json!({"expression": "document.getElementById('dynamic').className = 'gone'"}),
    )
    .await;
    let tree = cdp(&mut ctx, "Accessibility.getFullAXTree", json!({})).await;
    assert!(!tree["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["name"]["value"] == "Before"));
}
