//! Rust API for the Obscura headless browser.
//!
//! ```rust,no_run
//! use obscura_api::Browser;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let browser = Browser::builder()
//!         .stealth(true)
//!         .build()?;
//!     let mut page = browser.new_page().await?;
//!     page.goto("https://example.com").await?;
//!     println!("Title: {}", page.title().await?);
//!     Ok(())
//! }
//! ```

mod browser;
mod config;
mod error;
mod page;

pub use browser::Browser;
pub use config::BrowserConfig;
pub use error::Error;
pub use page::Page;
