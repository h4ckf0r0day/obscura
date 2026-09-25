# obscura

Embeddable Rust API for the [Obscura](https://github.com/h4ckf0r0day/obscura)
headless browser. Drive a real V8 + DOM browser (`Browser`, `Page`, `Element`,
`CookieStore`) directly from Rust, with no separate process or CDP round-trips.

## Install

This crate is not published to crates.io, so depend on it via git. Building it
compiles Obscura from source, including its embedded V8 (`deno_core`), so the
first build is large and slow.

```toml
[dependencies]
obscura = { git = "https://github.com/h4ckf0r0day/obscura", features = ["api"] }
tokio = { version = "1", features = ["rt", "macros"] }
anyhow = "1"
```

## Usage

```rust,no_run
use obscura::Browser;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let browser = Browser::builder()
        .stealth(true)
        .storage_dir("/tmp/cookies")
        .build()?;

    let mut page = browser.new_page().await?;
    page.goto("https://example.com").await?;

    let el = page.wait_for_selector("a", Duration::from_secs(5)).await?;
    println!("{} -> {:?}", el.text(), el.attribute("href"));

    Ok(())
}
```

See `examples/basic.rs` for a runnable version (`cargo run --example basic`).
