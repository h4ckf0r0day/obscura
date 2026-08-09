use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use napi::{Error, Result, Status};
use napi_derive::napi;
use obscura_js::runtime::ObscuraJsRuntime;

const DEFAULT_EVALUATE_TIMEOUT_MS: u32 = 5_000;
const MAX_EVALUATE_TIMEOUT_MS: u32 = 60_000;

/// A minimal Node-API owner for an Obscura V8 isolate.
///
/// Values cross the addon boundary as JSON strings. V8 handles never leave
/// `ObscuraJsRuntime`, which also keeps the embedded isolate separate from the
/// Node process's isolate at the Rust API boundary.
#[napi(js_name = "EmbeddedRuntime")]
pub struct NodeRuntime {
    runtime: Option<ObscuraJsRuntime>,
}

impl NodeRuntime {
    fn dispose(&mut self) -> std::result::Result<(), Box<dyn Any + Send>> {
        let Some(runtime) = self.runtime.take() else {
            return Ok(());
        };
        catch_unwind(AssertUnwindSafe(|| drop(runtime)))
    }
}

impl Drop for NodeRuntime {
    fn drop(&mut self) {
        // A destructor invoked by Node's GC cannot report a Rust panic, but it
        // must never allow one to unwind through the N-API frame.
        let _ = self.dispose();
    }
}

#[napi]
impl NodeRuntime {
    #[napi(constructor)]
    pub fn new(base_url: Option<String>) -> Result<Self> {
        let runtime = catch_unwind(AssertUnwindSafe(|| match base_url {
            Some(base_url) => ObscuraJsRuntime::with_base_url(&base_url),
            None => ObscuraJsRuntime::new(),
        }))
        .map_err(panic_error)?;

        Ok(Self {
            runtime: Some(runtime),
        })
    }

    /// Evaluate JavaScript inside Obscura's embedded V8 isolate.
    ///
    /// The watchdog bounds synchronous execution. The result is JSON text so
    /// napi-rs never needs to translate an embedded-V8 value into a Node-V8
    /// handle.
    #[napi]
    pub fn evaluate(&mut self, expression: String, timeout_ms: Option<u32>) -> Result<String> {
        let timeout_ms = timeout_ms.unwrap_or(DEFAULT_EVALUATE_TIMEOUT_MS);
        if timeout_ms == 0 || timeout_ms > MAX_EVALUATE_TIMEOUT_MS {
            return Err(Error::new(
                Status::InvalidArg,
                format!("timeoutMs must be between 1 and {MAX_EVALUATE_TIMEOUT_MS} milliseconds"),
            ));
        }

        let runtime = self.runtime.as_mut().ok_or_else(|| {
            Error::new(
                Status::GenericFailure,
                "EmbeddedRuntime is closed".to_string(),
            )
        })?;
        let value = catch_unwind(AssertUnwindSafe(|| {
            runtime.evaluate_with_timeout(&expression, Duration::from_millis(u64::from(timeout_ms)))
        }))
        .map_err(panic_error)?
        .map_err(runtime_error)?;

        serde_json::to_string(&value).map_err(|error| {
            Error::new(
                Status::GenericFailure,
                format!("failed to serialize evaluation result: {error}"),
            )
        })
    }

    /// Dispose the embedded isolate deterministically. Calling `close` more
    /// than once is harmless; later evaluations fail with a JavaScript error.
    #[napi]
    pub fn close(&mut self) -> Result<()> {
        self.dispose().map_err(panic_error)
    }
}

/// Construct an embedded runtime and execute one bounded expression.
///
/// This intentionally small entrypoint is useful for load-time feasibility
/// checks before callers retain an `EmbeddedRuntime` instance.
#[napi]
pub fn probe(expression: Option<String>) -> Result<String> {
    let mut runtime = NodeRuntime::new(None)?;
    let evaluation = runtime.evaluate(
        expression.unwrap_or_else(|| "({ engine: 'obscura', answer: 6 * 7 })".to_string()),
        Some(DEFAULT_EVALUATE_TIMEOUT_MS),
    );
    let close = runtime.close();

    match evaluation {
        Ok(value) => {
            close?;
            Ok(value)
        }
        Err(error) => {
            // Preserve the evaluation error while still disposing explicitly.
            let _ = close;
            Err(error)
        }
    }
}

fn runtime_error(message: String) -> Error {
    Error::new(Status::GenericFailure, message)
}

fn panic_error(payload: Box<dyn Any + Send>) -> Error {
    let message = if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "unknown Rust panic".to_string()
    };
    Error::new(
        Status::GenericFailure,
        format!("Obscura runtime panicked: {message}"),
    )
}
