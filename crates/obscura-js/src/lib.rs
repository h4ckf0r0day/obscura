#[macro_use]
extern crate html5ever;

pub mod module_loader;
pub mod runtime;
pub mod ops;
pub mod v8_lock;
pub mod markdown;

pub use markdown::HTML_TO_MARKDOWN_JS;
