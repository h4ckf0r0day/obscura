pub mod context;
pub mod lifecycle;
pub mod page;
pub mod profiles;

pub use context::{BrowserContext, BrowserContextOptions, CssMode};
pub use lifecycle::{LifecycleState, WaitUntil};
pub use obscura_js::HTML_TO_MARKDOWN_JS;
pub use page::{NetworkEvent, Page, PageError};
// Re-exported so the embeddable `obscura` crate (which depends on obscura-browser,
// not obscura-js) can surface the interception channel types.
pub use obscura_js::ops::{InterceptResolution, InterceptedRequest};
