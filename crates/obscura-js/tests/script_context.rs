use obscura_js::runtime::ObscuraJsRuntime;

#[tokio::test(flavor = "current_thread")]
async fn delayed_classic_scripts_restore_the_execution_context_not_their_creator() {
    let mut rt = ObscuraJsRuntime::new();
    rt.set_dom(obscura_dom::parse_html("<html><head></head><body></body></html>"));
    rt.set_url("http://example.com/test");
    rt.set_title("Test Page");
    rt.run_page_init();
    let result = rt.call_function_on_for_cdp(r#"async () => {
        const results = [];
        for (const [ordered, throws] of [[false,false],[true,false],[false,true]]) {
            const observed = {};
            globalThis.scriptContextObserved = observed;
            const finished = new Promise(resolve => globalThis.scriptContextDone = resolve);
            const creator = document.createElement('script');
            creator.id = 'creator';
            const body = `(() => { scriptContextObserved.child = document.currentScript.id;
                const nested = document.createElement('script'); nested.id = 'nested';
                nested.textContent = 'scriptContextObserved.nested = document.currentScript.id';
                document.head.appendChild(nested);
                scriptContextObserved.afterNested = document.currentScript.id;
                ${throws ? 'throw new Error("script context regression")' : ''} })()`;
            creator.textContent = `(() => {
                scriptContextObserved.creatorBefore = document.currentScript.id;
                const child = document.createElement('script'); child.id = 'child';
                ${ordered ? 'child.async = false;' : ''}
                child.src = ${JSON.stringify('data:text/javascript,' + encodeURIComponent(body))};
                child.onload = () => {
                    scriptContextObserved.load = document.currentScript?.id || null;
                    setTimeout(() => {
                        scriptContextObserved.timer = document.currentScript?.id || null;
                        scriptContextDone();
                    }, 0);
                };
                child.onerror = () => { scriptContextObserved.fetchError = true; scriptContextDone(); };
                document.head.appendChild(child);
                scriptContextObserved.creatorAfter = document.currentScript.id;
            })()`;
            document.head.appendChild(creator);
            await finished;
            results.push(observed);
        }
        return results;
    }"#, None, &[], true, true).await.unwrap();
    let expected = serde_json::json!({
        "creatorBefore":"creator", "creatorAfter":"creator", "child":"child",
        "nested":"nested", "afterNested":"child", "load":null, "timer":null,
    });
    assert_eq!(result.value.unwrap(), serde_json::json!([expected, expected, expected]));
}
