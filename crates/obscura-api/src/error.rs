use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("navigation error: {0}")]
    Navigation(String),

    #[error("JS evaluation error: {0}")]
    JsEval(String),

    #[error("timeout: {0}")]
    Timeout(String),

    #[error("element not found: {0}")]
    ElementNotFound(String),

    #[error("no page session")]
    NoPage,

    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}
