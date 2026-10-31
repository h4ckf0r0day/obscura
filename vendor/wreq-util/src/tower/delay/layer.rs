use std::time::Duration;

use tower::Layer;

use super::service::{Delay, DelayWith, JitterDelay, JitterDelayWith};

/// A Tower [`Layer`] that introduces a fixed delay before each request.
#[derive(Clone, Debug)]
pub struct DelayLayer {
    delay: Duration,
}

/// Conditional delay [`Layer`], applies delay based on a predicate.
///
/// Created via [`DelayLayer::when`].
#[derive(Clone, Debug)]
pub struct DelayLayerWith<P> {
    delay: Duration,
    predicate: P,
}

/// A Tower [`Layer`] that introduces a jittered delay before each request.
///
/// The actual delay for each request will be randomly chosen within the range
/// `[base - base*pct, base + base*pct]`.
///
/// # Example
///
/// ```no_run
/// use std::time::Duration;
/// use wreq::Client;
/// use wreq_util::middleware::delay::JitterDelayLayer;
///
/// // Creates delays in range [0.8s, 1.2s] (1s ± 20%)
/// let client = Client::builder()
///     .layer(JitterDelayLayer::new(Duration::from_secs(1), 0.2))
///     .build()?;
/// # Ok::<(), wreq::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct JitterDelayLayer {
    base: Duration,
    pct: f64,
}

/// Conditional jitter delay [`Layer`], applies delay based on a predicate.
///
/// Created via [`JitterDelayLayer::when`].
#[derive(Clone, Debug)]
pub struct JitterDelayLayerWith<P> {
    base: Duration,
    pct: f64,
    predicate: P,
}

// ===== impl DelayLayer =====

impl DelayLayer {
    /// Create a new [`DelayLayer`] with the given delay duration.
    #[inline]
    pub const fn new(delay: Duration) -> Self {
        DelayLayer { delay }
    }

    /// Apply delay only to requests that satisfy a predicate.
    ///
    /// Requests that don't match the predicate will pass through without delay.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::time::Duration;
    /// use http::Request;
    /// use wreq_util::middleware::delay::DelayLayer;
    ///
    /// // Only delay POST requests
    /// let layer = DelayLayer::new(Duration::from_secs(1))
    ///     .when(|req: &Request<_>| req.method() == http::Method::POST);
    /// ```
    pub fn when<P, Req>(self, predicate: P) -> DelayLayerWith<P>
    where
        P: Fn(&Req) -> bool + Clone,
    {
        DelayLayerWith::new(self.delay, predicate)
    }
}

impl<S> Layer<S> for DelayLayer {
    type Service = Delay<S>;

    #[inline]
    fn layer(&self, service: S) -> Self::Service {
        Delay::new(service, self.delay)
    }
}

// ===== impl DelayLayerWith =====

impl<P> DelayLayerWith<P> {
    /// Creates a new [`DelayLayerWith`].
    #[inline]
    pub fn new(delay: Duration, predicate: P) -> Self {
        Self { delay, predicate }
    }
}

impl<P, S> Layer<S> for DelayLayerWith<P>
where
    P: Clone,
{
    type Service = DelayWith<S, P>;

    #[inline]
    fn layer(&self, inner: S) -> Self::Service {
        DelayWith::new(inner, self.delay, self.predicate.clone())
    }
}

// ===== impl JitterDelayLayer =====

impl JitterDelayLayer {
    /// Create a new [`JitterDelayLayer`] with the given base delay and jitter percentage.
    ///
    /// # Arguments
    /// * `base` - The base delay duration
    /// * `pct` - The jitter percentage (0.0 to 1.0), representing ±pct deviation
    ///
    /// # Example
    ///
    /// ```
    /// use std::time::Duration;
    /// use wreq_util::middleware::delay::JitterDelayLayer;
    ///
    /// // Creates delays in range [800ms, 1200ms]
    /// let layer = JitterDelayLayer::new(Duration::from_secs(1), 0.2);
    /// ```
    #[inline]
    pub fn new(base: Duration, pct: f64) -> Self {
        Self {
            base,
            pct: pct.clamp(0.0, 1.0),
        }
    }

    /// Apply jitter delay only to requests that satisfy a predicate.
    ///
    /// Requests that don't match the predicate will pass through without delay.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use std::time::Duration;
    /// use http::Request;
    /// use wreq_util::middleware::delay::JitterDelayLayer;
    ///
    /// // Only delay requests to paths starting with "/slow"
    /// let layer = JitterDelayLayer::new(Duration::from_secs(1), 0.2)
    ///     .when(|req: &Request<_>| req.uri().path().starts_with("/slow"));
    /// ```
    pub fn when<P, Req>(self, predicate: P) -> JitterDelayLayerWith<P>
    where
        P: Fn(&Req) -> bool + Clone,
    {
        JitterDelayLayerWith::new(self.base, self.pct, predicate)
    }
}

impl<S> Layer<S> for JitterDelayLayer {
    type Service = JitterDelay<S>;

    #[inline]
    fn layer(&self, inner: S) -> Self::Service {
        JitterDelay::new(inner, self.base, self.pct)
    }
}

// ===== impl JitterDelayLayerWith =====

impl<P> JitterDelayLayerWith<P> {
    /// Creates a new [`JitterDelayLayerWith`].
    #[inline]
    pub fn new(base: Duration, pct: f64, predicate: P) -> Self {
        Self {
            base,
            pct: pct.clamp(0.0, 1.0),
            predicate,
        }
    }
}

impl<P, S> Layer<S> for JitterDelayLayerWith<P>
where
    P: Clone,
{
    type Service = JitterDelayWith<S, P>;

    #[inline]
    fn layer(&self, inner: S) -> Self::Service {
        JitterDelayWith::new(inner, self.base, self.pct, self.predicate.clone())
    }
}
