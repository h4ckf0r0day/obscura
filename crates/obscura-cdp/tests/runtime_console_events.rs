use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::{json, Value};

async fn cdp(
    ctx: &mut CdpContext,
    id: u64,
    method: &str,
    params: Value,
    session: Option<&str>,
) -> Value {
    let response = dispatch(
        &CdpRequest {
            id,
            method: method.to_string(),
            params,
            session_id: session.map(str::to_string),
        },
        ctx,
    )
    .await;
    assert!(
        response.error.is_none(),
        "CDP {method} failed: {:?}",
        response.error
    );
    response.result.unwrap_or_else(|| json!({}))
}

async fn create_and_attach(ctx: &mut CdpContext) -> (String, String) {
    let created = cdp(
        ctx,
        900,
        "Target.createTarget",
        json!({"url": "about:blank"}),
        None,
    )
    .await;
    let target_id = created["targetId"]
        .as_str()
        .expect("Target.createTarget returned no targetId")
        .to_string();
    let session = attach(ctx, &target_id, 901).await;
    (target_id, session)
}

async fn attach(ctx: &mut CdpContext, target_id: &str, id: u64) -> String {
    let attached = cdp(
        ctx,
        id,
        "Target.attachToTarget",
        json!({"targetId": target_id, "flatten": true}),
        None,
    )
    .await;
    attached["sessionId"]
        .as_str()
        .expect("Target.attachToTarget returned no sessionId")
        .to_string()
}

fn events<'a>(ctx: &'a CdpContext, method: &str) -> Vec<(Option<&'a str>, &'a Value)> {
    ctx.pending_events
        .iter()
        .filter(|event| event.method == method)
        .map(|event| (event.session_id.as_deref(), &event.params))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn navigation_emits_console_arguments_and_uncaught_exception_details() {
    let mut ctx = CdpContext::new();
    let (_, session) = create_and_attach(&mut ctx).await;
    cdp(&mut ctx, 1, "Runtime.enable", json!({}), Some(&session)).await;
    ctx.pending_events.clear();

    let url = concat!(
        "data:text/html,<script>",
        "window.__probe=1;",
        "console.log('probe',{answer:42},undefined,NaN,-0,1n);",
        "console.error('bad');",
        "throw new Error('boom')",
        "</script>"
    );
    cdp(
        &mut ctx,
        2,
        "Page.navigate",
        json!({"url": url, "waitUntil": "load"}),
        Some(&session),
    )
    .await;

    let console = events(&ctx, "Runtime.consoleAPICalled");
    assert_eq!(console.len(), 2, "console events: {console:?}");
    assert!(console.iter().all(|(target, _)| *target == Some(&session)));

    let log = console
        .iter()
        .find(|(_, params)| params["type"] == "log")
        .expect("missing console.log event")
        .1;
    assert_eq!(log["executionContextId"], 2);
    assert!(log["timestamp"].as_f64().is_some_and(|value| value > 0.0));
    assert_eq!(log["args"].as_array().map(Vec::len), Some(6));
    assert_eq!(log["args"][0]["type"], "string");
    assert_eq!(log["args"][0]["value"], "probe");
    assert_eq!(log["args"][1]["type"], "object");
    let object_id = log["args"][1]["objectId"]
        .as_str()
        .expect("console object argument had no objectId")
        .to_string();
    assert_eq!(log["args"][2]["type"], "undefined");
    assert_eq!(log["args"][3]["unserializableValue"], "NaN");
    assert_eq!(log["args"][4]["unserializableValue"], "-0");
    assert_eq!(log["args"][5]["type"], "bigint");
    assert_eq!(log["args"][5]["unserializableValue"], "1n");

    let error = console
        .iter()
        .find(|(_, params)| params["type"] == "error")
        .expect("missing console.error event")
        .1;
    assert_eq!(error["args"][0]["value"], "bad");

    let exceptions = events(&ctx, "Runtime.exceptionThrown");
    assert_eq!(exceptions.len(), 1, "exception events: {exceptions:?}");
    assert_eq!(exceptions[0].0, Some(session.as_str()));
    let details = &exceptions[0].1["exceptionDetails"];
    assert_eq!(details["text"], "Uncaught");
    assert!(details["exceptionId"].as_u64().is_some());
    assert!(details["url"].as_str().is_some_and(|url| url.starts_with("data:text/html,")));
    assert!(details["exception"]["description"]
        .as_str()
        .is_some_and(|description| description.contains("Error: boom")));
    assert!(exceptions[0].1["timestamp"]
        .as_f64()
        .is_some_and(|value| value > 0.0));
    drop(console);
    drop(exceptions);

    let object_value = cdp(
        &mut ctx,
        20,
        "Runtime.callFunctionOn",
        json!({
            "functionDeclaration": "function() { return this.answer; }",
            "objectId": object_id,
            "returnByValue": true,
        }),
        Some(&session),
    )
    .await;
    assert_eq!(object_value["result"]["value"], 42.0);

    let marker = cdp(
        &mut ctx,
        3,
        "Runtime.evaluate",
        json!({"expression": "window.__probe", "returnByValue": true}),
        Some(&session),
    )
    .await;
    assert_eq!(marker["result"]["value"], 1.0);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_events_only_reach_sessions_while_runtime_is_enabled() {
    let mut ctx = CdpContext::new();
    let (target_id, first) = create_and_attach(&mut ctx).await;
    let second = attach(&mut ctx, &target_id, 902).await;

    cdp(&mut ctx, 1, "Runtime.enable", json!({}), Some(&first)).await;
    ctx.pending_events.clear();
    cdp(
        &mut ctx,
        2,
        "Runtime.evaluate",
        json!({"expression": "console.log('first')"}),
        Some(&first),
    )
    .await;
    let first_events = events(&ctx, "Runtime.consoleAPICalled");
    assert_eq!(
        first_events
            .iter()
            .map(|(session, _)| *session)
            .collect::<Vec<_>>(),
        vec![Some(first.as_str())]
    );
    assert_eq!(first_events[0].1["executionContextId"], 1);

    cdp(&mut ctx, 3, "Runtime.enable", json!({}), Some(&second)).await;
    ctx.pending_events.clear();
    cdp(
        &mut ctx,
        4,
        "Runtime.evaluate",
        json!({"expression": "console.warn('both')"}),
        Some(&first),
    )
    .await;
    let mut targets: Vec<&str> = events(&ctx, "Runtime.consoleAPICalled")
        .iter()
        .map(|(session, _)| session.expect("runtime event had no session"))
        .collect();
    targets.sort_unstable();
    let mut expected = vec![first.as_str(), second.as_str()];
    expected.sort_unstable();
    assert_eq!(targets, expected);
    assert!(events(&ctx, "Runtime.consoleAPICalled")
        .iter()
        .all(|(_, params)| params["type"] == "warning"));

    cdp(&mut ctx, 5, "Runtime.disable", json!({}), Some(&first)).await;
    ctx.pending_events.clear();
    cdp(
        &mut ctx,
        6,
        "Runtime.evaluate",
        json!({"expression": "console.error('second')"}),
        Some(&second),
    )
    .await;
    assert_eq!(
        events(&ctx, "Runtime.consoleAPICalled")
            .iter()
            .map(|(session, _)| *session)
            .collect::<Vec<_>>(),
        vec![Some(second.as_str())]
    );

    cdp(
        &mut ctx,
        7,
        "Runtime.releaseObjectGroup",
        json!({}),
        Some(&second),
    )
    .await;
    cdp(&mut ctx, 8, "Runtime.disable", json!({}), Some(&second)).await;
    ctx.pending_events.clear();
    let retained = cdp(
        &mut ctx,
        9,
        "Runtime.evaluate",
        json!({
            "expression": "(() => { console.log({notRetained:true}); return Object.keys(globalThis.__obscura_objects).filter(k => k.startsWith('console-')).length; })()",
            "returnByValue": true,
        }),
        Some(&second),
    )
    .await;
    assert_eq!(retained["result"]["value"], 0.0);
    assert!(events(&ctx, "Runtime.consoleAPICalled").is_empty());
}
