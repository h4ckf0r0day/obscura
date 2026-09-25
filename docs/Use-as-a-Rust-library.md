The `obscura` crate embeds the engine in a Rust program with a `Browser` / `Page` / `Element` API plus a cookie store, no CDP round-trips. It builds V8 from source, so it is a git dependency rather than a crates.io release.

## Add the dependency

```toml
[dependencies]
obscura = { git = "https://github.com/h4ckf0r0day/obscura" }
tokio = { version = "1", features = ["rt", "macros"] }
anyhow = "1"
```

The first build compiles V8 from source, so it is slow and needs the same build tools as [Build from source](Build-from-source.md). Pin a tag for reproducible builds:

```toml
obscura = { git = "https://github.com/h4ckf0r0day/obscura", tag = "v0.1.7" }
```

## Quickstart

```rust
use obscura::Browser;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let browser = Browser::builder()
        .stealth(true)
        .build()?;

    let mut page = browser.new_page().await?;
    page.goto("https://example.com").await?;

    println!("URL: {}", page.url());
    println!("HTML bytes: {}", page.content().len());

    let el = page.wait_for_selector("h1", Duration::from_secs(5)).await?;
    println!("Heading: {}", el.text());

    let title = page.evaluate("document.title");
    println!("Title: {}", title);

    Ok(())
}
```

## API surface

`Browser::builder()` configures the engine: `.stealth(bool)`, `.proxy(url)`, `.user_agent(ua)`, `.storage_dir(dir)`, then `.build()`. `Browser::new()` uses defaults.

`Page`:
- `goto(url).await` navigate and wait for load
- `content()` rendered HTML
- `url()` current URL
- `evaluate(js)` run JavaScript, returns a `serde_json::Value`
- `query_selector(css)` first match as an `Element`, or `None`
- `wait_for_selector(css, Duration).await` poll until present
- `settle(max_ms).await` drive the event loop so async work (`fetch`, timers) completes
- `on_request(cb)` / `on_response(cb)` passive callbacks for every request and response
- `enable_interception()` channel to block, mock, or rewrite requests
- `add_preload_script(js)` run a script before the page's own scripts

`Element`: `text()`, `attribute(name)`, `click()`.

`CookieStore`: `set`, `get_all`, `get_for_url`, `save_to_file`, `load_from_file`.

## Intercept requests

The interception API observes, blocks, mocks, and rewrites the requests a page makes, including JavaScript `fetch()` and XHR. Use it to capture API payloads while crawling, block trackers, or mock responses in tests.

### Passive callbacks

`on_request` and `on_response` fire for every request and response (navigation and JS `fetch()`/XHR) and are non-blocking. `on_response` is the main path for capturing the JSON an SPA loads asynchronously. Both return a stable id; pass it to `off_request` / `off_response` to detach the callback when a crawl phase is done. Callbacks are scoped to the page that registered them: they never fire for another page's requests and are dropped with the page.

```rust
use obscura::{Browser, ResourceType};
use std::sync::Arc;

let browser = Browser::new()?;
let mut page = browser.new_page().await?;

page.on_response(Arc::new(|info, resp| {
    if info.resource_type == ResourceType::Fetch {
        println!("{} -> {} bytes", info.url, resp.body.len());
    }
}));

page.goto("https://example.com").await?;
page.settle(2000).await;   // let in-page fetch() calls resolve
```

### Active interception

`enable_interception()` returns a channel of every JS `fetch()`/XHR request. Resolve each through its `resolver` to pass, block, mock, or rewrite it.

```rust
use obscura::{Browser, InterceptResolution};

let mut page = browser.new_page().await?;
let mut rx = page.enable_interception();

tokio::spawn(async move {
    while let Some(req) = rx.recv().await {
        let action = if req.url.contains("/ads") {
            InterceptResolution::Fail { reason: "blocked".into() }
        } else if req.url.ends_with("/api/flags") {
            InterceptResolution::Fulfill {
                status: 200,
                headers: Default::default(),
                body: r#"{"newDashboard":true}"#.into(),
            }
        } else {
            // Pass through, or rewrite by setting url/method/headers/body.
            InterceptResolution::Continue { url: None, method: None, headers: None, body: None }
        };
        let _ = req.resolver.send(action);
    }
});

page.goto("https://example.com").await?;
page.settle(2000).await;
```

A `Continue` with `url: Some(...)` rewrites the target. The new URL is re-checked against the SSRF / private-network gate, so a rewrite cannot reach an internal address that would otherwise need `--allow-private-network`.

### Preload scripts

`add_preload_script` runs a script before any of the page's own `<script>` tags (the CDP `Page.addScriptToEvaluateOnNewDocument` contract), so it can install hooks before the page bootstraps. Call it before `goto`.

```rust
let mut page = browser.new_page().await?;
page.add_preload_script("window.__patched = true;");
page.goto("https://example.com").await?;
```

`resource_type` reports `Fetch` for JS-initiated requests and does not yet split `Xhr` from `Fetch`.

## When to use which interface

- Embedding the engine in a Rust service: this crate.
- Driving from Node/Python with existing Puppeteer/Playwright code: the [CDP server](Connect-Puppeteer-or-Playwright.md).
- Giving an AI agent browser tools: the [MCP server](Use-the-MCP-server.md).
- One-off fetches and scraping from the shell: the [CLI](CLI-reference.md).
