# wreq-util

A collection of utilities to do common things with [wreq](https://github.com/0x676e67/wreq).

[![Crates.io](https://img.shields.io/crates/v/wreq-util.svg?logo=rust)](https://crates.io/crates/wreq-util)
[![GitHub License](https://img.shields.io/github/license/0x676e67/wreq-util)](https://github.com/0x676e67/wreq-util/blob/main/LICENSE)
[![Discord chat][discord-badge]][discord-url]

[discord-badge]: https://img.shields.io/discord/1486741856397164788.svg?logo=discord
[discord-url]: https://discord.gg/rfbvyFkgq3

See the [crate documentation](https://docs.rs/wreq-util/latest/wreq_util) for more details.

## Features

**wreq-util** offers a set of [tower](https://github.com/tower-rs/tower) middleware and utilities designed specifically for the [wreq](https://github.com/0x676e67/wreq) HTTP client:

- **Emulation** various mainstream browsers (Chrome, Firefox, Safari, Opera, OkHttp) and their versions.
- **Delay/JitterDelay**: Add fixed or jittered delays to HTTP [request](https://docs.rs/http/latest/http/request/index.html) with customizable strategies and predicates.

## Example

The following example uses the [Tokio](https://tokio.rs) runtime with optional features enabled by adding this to your `Cargo.toml`:

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
wreq = "6.0.0-rc"
wreq-util = "3.0.0-rc"
```

And then the code:

```rust
use wreq::Client;
use wreq_util::Emulation;

#[tokio::main]
async fn main() -> wreq::Result<()> {
    // Build a client
    let client = Client::builder()
        .emulation(Emulation::Chrome147)
        .build()?;

    // Use the API you're already familiar with
    let resp = client.get("https://www.google.com").send().await?;
    println!("{}", resp.text().await?);
    Ok(())
}
```

## License

Licensed under either of Apache License, Version 2.0 ([LICENSE](./LICENSE) or http://www.apache.org/licenses/LICENSE-2.0).

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the [Apache-2.0](./LICENSE) license, shall be licensed as above, without any additional terms or conditions.

## FAQ

**For all issues and feedback, please head over to the [wreq](https://github.com/0x676e67/wreq) repository.**
