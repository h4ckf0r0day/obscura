use obscura_js::runtime::ObscuraJsRuntime;

#[test]
fn runtimes_can_be_used_and_dropped_out_of_creation_order() {
    let mut first = ObscuraJsRuntime::new();
    let mut second = ObscuraJsRuntime::new();

    assert_eq!(first.evaluate("1 + 1").unwrap(), serde_json::json!(2.0),);
    assert_eq!(second.evaluate("2 + 2").unwrap(), serde_json::json!(4.0),);

    drop(first);

    assert_eq!(second.evaluate("3 + 3").unwrap(), serde_json::json!(6.0),);
}

#[tokio::test(flavor = "current_thread")]
async fn runtime_futures_can_be_interleaved_on_one_thread() {
    let mut first = ObscuraJsRuntime::new();
    let mut second = ObscuraJsRuntime::new();

    first
        .execute_script("first", "setTimeout(() => globalThis.done = 1, 1)")
        .unwrap();
    second
        .execute_script("second", "setTimeout(() => globalThis.done = 2, 1)")
        .unwrap();

    let (first_result, second_result) =
        tokio::join!(first.run_event_loop(), second.run_event_loop());
    first_result.unwrap();
    second_result.unwrap();
    assert_eq!(first.evaluate("done").unwrap(), serde_json::json!(1.0));
    assert_eq!(second.evaluate("done").unwrap(), serde_json::json!(2.0));
}

#[tokio::test(flavor = "current_thread")]
async fn module_loads_can_be_interleaved_on_one_thread() {
    let mut first = ObscuraJsRuntime::new();
    let mut second = ObscuraJsRuntime::new();

    let (first_result, second_result) = tokio::join!(
        first.load_inline_module(
            "globalThis.moduleResult = 11",
            "https://first.invalid/module.js",
            1_000,
        ),
        second.load_inline_module(
            "globalThis.moduleResult = 22",
            "https://second.invalid/module.js",
            1_000,
        ),
    );
    first_result.unwrap();
    second_result.unwrap();
    assert_eq!(
        first.evaluate("moduleResult").unwrap(),
        serde_json::json!(11.0)
    );
    assert_eq!(
        second.evaluate("moduleResult").unwrap(),
        serde_json::json!(22.0)
    );
}
