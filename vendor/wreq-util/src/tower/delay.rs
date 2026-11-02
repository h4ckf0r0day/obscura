//! Request delay middleware.
//!
//! Adds configurable delays before HTTP requests — useful for rate limiting,
//! testing under slow network conditions, or just being polite to APIs.
//!
//! # Quick Start
//!
//! Fixed 1-second delay:
//!
//! ```no_run
//! use std::time::Duration;
//! use wreq::Client;
//! use wreq_util::middleware::delay::DelayLayer;
//!
//! let client = Client::builder()
//!     .layer(DelayLayer::new(Duration::from_secs(1)))
//!     .build()?;
//! # Ok::<(), wreq::Error>(())
//! ```
//!
//! Random jitter (0.8s ~ 1.2s):
//!
//! ```no_run
//! use std::time::Duration;
//! use wreq::Client;
//! use wreq_util::middleware::delay::JitterDelayLayer;
//!
//! let client = Client::builder()
//!     .layer(JitterDelayLayer::new(Duration::from_secs(1), 0.2))
//!     .build()?;
//! # Ok::<(), wreq::Error>(())
//! ```
//!
//! # Conditional Delays
//!
//! Use `.when()` to apply delays only to matching requests:
//!
//! ```ignore
//! // Only delay POST requests
//! DelayLayer::new(Duration::from_secs(1))
//!     .when(|req: &http::Request<_>| req.method() == http::Method::POST)
//!
//! // Jitter on specific paths
//! JitterDelayLayer::new(Duration::from_millis(500), 0.3)
//!     .when(|req: &http::Request<_>| req.uri().path().starts_with("/api"))
//! ```
//!
//! # Notes
//!
//! - Delays are async and won't block the runtime
//! - Not a substitute for proper rate limiters — servers can still see timing patterns
//! - Keep delays short in hot paths

mod future;
mod layer;
mod service;

use std::time::Duration;

pub use self::{
    future::ResponseFuture,
    layer::{DelayLayer, DelayLayerWith, JitterDelayLayer, JitterDelayLayerWith},
    service::{Delay, DelayWith, JitterDelay, JitterDelayWith},
};

/// Compute a randomized duration in `[base * (1 - pct), base * (1 + pct)]`.
fn jittered_duration(base: Duration, pct: f64) -> Duration {
    let jitter = base.mul_f64(pct);
    let low = base.saturating_sub(jitter);
    let high = base.saturating_add(jitter);

    if low >= high {
        return base;
    }

    // Generate pseudo-random value using multiple entropy sources
    let time_entropy = {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    };

    // Use stack address as additional entropy source
    let addr_entropy = (&time_entropy as *const u64 as u64).wrapping_mul(0x517cc1b727220a95);

    // Mix entropy sources with a simple but effective hash
    let mixed = time_entropy
        .wrapping_add(addr_entropy)
        .wrapping_mul(0x9e3779b97f4a7c15);

    // Convert to fraction in [0, 1)
    let frac = (mixed as f64) / (u64::MAX as f64);

    let span = (high - low).as_secs_f64();
    low + Duration::from_secs_f64(span * frac)
}
