use obscura_js::runtime::ObscuraJsRuntime;
use serde_json::{json, Value};

async fn evaluate(source: &str) -> Value {
    let mut runtime = ObscuraJsRuntime::new();
    runtime.set_dom(obscura_dom::parse_html("<!doctype html><body></body>"));
    runtime.run_page_init();
    runtime.evaluate_for_cdp(source, true, true).await.unwrap().value.unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn response_streams_preserve_bytes_errors_and_clone_consumption() {
    let value = evaluate(r#"(async () => {
        const original = new Response('{"ok":true}');
        const json = await new Response(original.body).json();
        const binary = new Response(new ReadableStream({start(c) { c.enqueue(new Uint8Array([0,255,128])); c.close(); }}));
        const copy = binary.clone();
        const unused = [binary.bodyUsed, copy.bodyUsed];
        const a = [...new Uint8Array(await binary.arrayBuffer())];
        const b = [...new Uint8Array(await copy.arrayBuffer())];
        let badChunk = false, sourceError = false, consumed = false;
        try { await new Response(new ReadableStream({start(c) {c.enqueue('not bytes');c.close();}})).text(); } catch(e) {badChunk=e instanceof TypeError;}
        try { await new Response(new ReadableStream({start(c) {c.error(new Error('source failure'));}})).text(); } catch(e) {sourceError=e.message==='source failure';}
        try { binary.clone(); } catch(e) { consumed=e instanceof TypeError; }
        return {json,a,b,unused,badChunk,sourceError,consumed};
    })()"#).await;
    assert_eq!(value, json!({"json":{"ok":true},"a":[0,255,128],"b":[0,255,128],"unused":[false,false],"badChunk":true,"sourceError":true,"consumed":true}));
}

#[tokio::test(flavor = "current_thread")]
async fn response_streams_keep_demand_and_cancellation_with_the_source() {
    let value = evaluate(r#"(async () => {
        let pulls=0, cancelled=null;
        const source=new ReadableStream({pull(c){pulls++;c.enqueue(new Uint8Array([pulls]));},cancel(reason){cancelled=reason;}});
        const response=new Response(source);
        await new Promise(resolve=>setTimeout(resolve,0));
        const lazy=pulls<=1, same=response.body===source, unused=!response.bodyUsed;
        await response.body.cancel('stop');
        const locked=new ReadableStream(),reader=locked.getReader();let rejected=false;
        try{new Response(locked);}catch(e){rejected=e instanceof TypeError;}reader.releaseLock();
        let count=0;
        const bytes=await new Response(new ReadableStream({pull(c){count++;if(count<=3)c.enqueue(new Uint8Array([count]));else c.close();}})).arrayBuffer();
        return {lazy,same,unused,cancelled,used:response.bodyUsed,rejected,bytes:[...new Uint8Array(bytes)]};
    })()"#).await;
    assert_eq!(value, json!({"lazy":true,"same":true,"unused":true,"cancelled":"stop","used":true,"rejected":true,"bytes":[1,2,3]}));
}

#[tokio::test(flavor = "current_thread")]
async fn response_streams_reject_locked_bytes_and_forward_tee_failures() {
    let value = evaluate(r#"(async () => {
        const response=new Response('locked bytes'), reader=response.body.getReader();
        let cloneRejected=false,textRejected=false;
        try{response.clone();}catch(e){cloneRejected=e instanceof TypeError;}
        try{await response.text();}catch(e){textRejected=e instanceof TypeError;}
        reader.releaseLock();const text=await response.text();
        let controller;const branches=new ReadableStream({start(c){controller=c;c.enqueue(new Uint8Array([1]));}}).tee();
        await new Promise(resolve=>setTimeout(resolve,0));
        const branch=branches[0].getReader();
        const closed=branch.closed.then(()=>false,e=>e.message==='upstream error');
        controller.error(new Error('upstream error'));const sourceClosed=await closed;let readError=false;
        try{await branch.read();}catch(e){readError=e.message==='upstream error';}
        const cancellations=new ReadableStream({cancel(){return Promise.reject(new Error('cancel failed'));}}).tee();
        const settled=await Promise.allSettled(cancellations.map(b=>b.cancel()));
        return {cloneRejected,textRejected,text,closed:sourceClosed,readError,cancelRejected:settled.every(s=>s.status==='rejected'&&s.reason.message==='cancel failed')};
    })()"#).await;
    assert_eq!(value, json!({"cloneRejected":true,"textRejected":true,"text":"locked bytes","closed":true,"readError":true,"cancelRejected":true}));
}

#[tokio::test(flavor = "current_thread")]
async fn response_stream_clone_preserves_default_stream_chunk_identity() {
    let value = evaluate(r#"(async () => {
        const response = new Response(new ReadableStream({start(c) {c.enqueue(new Uint8Array([1])); c.close();}}));
        const copy = response.clone();
        const first = await response.body.getReader().read();
        first.value[0] = 9;
        return [...new Uint8Array(await copy.arrayBuffer())];
    })()"#).await;
    assert_eq!(value, json!([9]));
}
