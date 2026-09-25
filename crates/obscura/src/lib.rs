//! Rust API for the Obscura headless browser.
//!
//! ```rust,no_run
//! use obscura::Browser;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let browser = Browser::builder()
//!         .stealth(true)
//!         .build()?;
//!     let mut page = browser.new_page().await?;
//!     page.goto("https://example.com").await?;
//!     println!("Content: {} bytes", page.content().len());
//!     Ok(())
//! }
//! ```

mod browser;
mod config;
mod cookie;
mod error;
mod page;

pub use browser::Browser;
pub use config::BrowserConfig;
pub use cookie::{Cookie, CookieStore};
pub use error::Error;
pub use page::Page;

// Request/response interception types (issue #306).
pub use obscura_browser::{InterceptedRequest, InterceptResolution};
pub use obscura_net::{RequestCallback, RequestInfo, ResourceType, Response, ResponseCallback};
