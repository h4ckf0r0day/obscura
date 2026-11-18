Two recipes for the two most common extensions: a new CDP method, and a new JS Web API.

## Adding a CDP method

Worked example: `MyDomain.doThing` that takes `{ name }` and returns `{ ok }`.

### 1. Add the handler

Create or edit a file under `crates/obscura-cdp/src/domains/`:

```rust
// crates/obscura-cdp/src/domains/my_domain.rs
use serde_json::{json, Value};
use crate::dispatch::CdpContext;

pub async fn do_thing(
    params: &Value,
    _ctx: &mut CdpContext,
    _session_id: &Option<String>,
) -> Result<Value, String> {
    let name = params.get("name")
        .and_then(|v| v.as_str())
        .ok_or("missing name")?;

    // do the work

    Ok(json!({ "ok": true, "name": name }))
}
```

### 2. Register in the dispatcher

Export the module in `crates/obscura-cdp/src/domains/mod.rs`:

```rust
pub mod my_domain;
```

The dispatcher splits `Domain.method` before matching the domain. In
`crates/obscura-cdp/src/dispatch.rs`, add a domain arm:

```rust
"MyDomain" if method == "doThing" => {
    domains::my_domain::do_thing(&req.params, ctx, &req.session_id).await
}
```

### 3. Test it

`crates/obscura-cdp/tests/cdp_my_domain.rs`:

```rust
use obscura_cdp::dispatch::{dispatch, CdpContext};
use obscura_cdp::types::CdpRequest;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn my_domain_do_thing_returns_ok() {
    let mut ctx = CdpContext::new();
    let resp = dispatch(&CdpRequest {
        id: 1,
        method: "MyDomain.doThing".into(),
        params: json!({ "name": "test" }),
        session_id: None,
    }, &mut ctx).await;

    assert!(resp.error.is_none());
    assert_eq!(resp.result.unwrap()["ok"], true);
}
```

Register `cdp_my_domain` in the `integration_tests!` list in
`crates/obscura-cdp/tests/integration.rs`, then run:

```bash
cargo nextest run --release --features render -p obscura-cdp --test integration -E 'test(cdp_my_domain::)'
```

## Adding a Web API

`crypto.subtle.digest` is already implemented. The excerpts below show the
JS-to-Rust flow. Modify the existing op and shim when extending it; do not
register a duplicate op.

### 1. Add the Rust op

In `crates/obscura-js/src/ops.rs`:

```rust
#[op2]
#[buffer]
fn op_subtle_digest(#[string] algorithm: &str, #[buffer] data: &[u8]) -> Vec<u8> {
    use sha1::Digest as _;
    match algorithm.to_ascii_uppercase().as_str() {
        "SHA-1"   => sha1::Sha1::digest(data).to_vec(),
        "SHA-256" => sha2::Sha256::digest(data).to_vec(),
        "SHA-384" => sha2::Sha384::digest(data).to_vec(),
        "SHA-512" => sha2::Sha512::digest(data).to_vec(),
        "SHA-512/224" => sha2::Sha512_224::digest(data).to_vec(),
        "SHA-512/256" => sha2::Sha512_256::digest(data).to_vec(),
        _ => vec![], // The JS shim rejects unsupported names before this call.
    }
}
```

### 2. Register the op

In the same file, add the op to the vector in `build_extension()`:

```rust
let mut ops = vec![
    op_dom(),
    // Other registered ops...
    op_subtle_digest(),
];
```

### 3. Add the JS shim

The existing crypto implementation in `crates/obscura-js/js/bootstrap.js`
validates inputs before calling the private native bridge. Its digest method is:

```js
async digest(algorithm, data) {
  const name = (typeof algorithm === "string" ? algorithm : algorithm && algorithm.name || "")
    .toUpperCase().replace("_", "-");
  if (name !== "SHA-1" && name !== "SHA-256" && name !== "SHA-384" &&
      name !== "SHA-512" && name !== "SHA-512/224" && name !== "SHA-512/256") {
    throw new DOMException("Unrecognized algorithm name", "NotSupportedError");
  }
  return bufferOf(__obscuraCore.ops.op_subtle_digest(name, toBytes(data)));
}
```

`toBytes` and `bufferOf` are helpers in the same crypto closure. The async method
turns validation errors into rejected promises. Do not expose the native bridge
to page scripts or substitute a different algorithm for unsupported input.

### 4. Add a dependency if needed

`crates/obscura-js/Cargo.toml`:

```toml
sha1 = "0.10"
sha2 = "0.10"
```

### 5. Smoke test

```bash
cargo build --release -p obscura-cli --bins --features render
./target/release/obscura fetch https://example.com --eval "
  crypto.subtle.digest('SHA-256', new TextEncoder().encode('hi'))
    .then(buf => {
      document.body.textContent = Array.from(new Uint8Array(buf))
        .map(b => b.toString(16).padStart(2, '0')).join('');
    })
" --dump text
```

## Tips

- Keep the JS shim thin. All side effects go through ops.
- Use async wrappers when validation failures must reject a promise.
- Match the spec: Web API names and shapes are checked by Puppeteer / Playwright wrappers.
- DOM mutations go through `op_dom`, not new ops.
- For events that need to fire across handlers, use the existing `_makeListenerBox` helper in `bootstrap.js`.

## Worked examples in the tree

- CDP method with intercept: `crates/obscura-cdp/src/domains/page.rs` `do_navigate`.
- Web API with op + JS shim: `crypto.subtle.digest` (above).
- Web API in pure JS (no op): `DOMParser` in `bootstrap.js`.
- Web API with async event firing: `WebSocket`, `IntersectionObserver` in `bootstrap.js`.
