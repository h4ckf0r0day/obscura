use std::{
    task::{Context, Poll},
    time::Duration,
};

use tower::{BoxError, Service};

use super::{future::ResponseFuture, jittered_duration};

/// A Tower [`Service`] that introduces a fixed delay before each request.
#[derive(Debug, Clone)]
pub struct Delay<S> {
    inner: S,
    delay: Duration,
}

/// A Tower [`Service`] that conditionally applies fixed delay based on a predicate.
///
/// Requests that match the predicate will have the delay applied;
/// other requests pass through immediately.
#[derive(Clone, Debug)]
pub struct DelayWith<S, P> {
    inner: Delay<S>,
    predicate: P,
}

/// A Tower [`Service`] that applies jittered delay to requests.
///
/// This service wraps an inner service and introduces a random delay
/// (within a configured range) before each request.
#[derive(Clone, Debug)]
pub struct JitterDelay<S> {
    inner: S,
    base: Duration,
    pct: f64,
}

/// A Tower [`Service`] that conditionally applies jittered delay based on a predicate.
///
/// Requests that match the predicate will have a jittered delay applied;
/// other requests pass through immediately.
#[derive(Clone, Debug)]
pub struct JitterDelayWith<S, P> {
    inner: JitterDelay<S>,
    predicate: P,
}

// ===== impl Delay =====

impl<S> Delay<S> {
    /// Create a new [`Delay`] service wrapping the given inner service
    #[inline]
    pub fn new(inner: S, delay: Duration) -> Self {
        Delay { inner, delay }
    }
}

impl<S, Request> Service<Request> for Delay<S>
where
    S: Service<Request>,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let response = self.inner.call(req);
        let sleep = tokio::time::sleep(self.delay);
        ResponseFuture::new(response, sleep)
    }
}

// ===== impl DelayWith =====

impl<S, P> DelayWith<S, P> {
    /// Creates a new [`DelayWith`].
    #[inline]
    pub fn new(inner: S, delay: Duration, predicate: P) -> Self {
        Self {
            inner: Delay::new(inner, delay),
            predicate,
        }
    }
}

impl<S, Req, P> Service<Req> for DelayWith<S, P>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
    P: Fn(&Req) -> bool,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        if !(self.predicate)(&req) {
            self.inner.delay = Duration::ZERO;
        }
        self.inner.call(req)
    }
}

// ===== impl JitterDelay =====

impl<S> JitterDelay<S> {
    /// Creates a new [`JitterDelay`].
    #[inline]
    pub fn new(inner: S, base: Duration, pct: f64) -> Self {
        Self {
            inner,
            base,
            pct: pct.clamp(0.0, 1.0),
        }
    }
}

impl<S, Req> Service<Req> for JitterDelay<S>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let delay = jittered_duration(self.base, self.pct);
        let sleep = tokio::time::sleep(delay);
        let fut = self.inner.call(req);
        ResponseFuture::new(fut, sleep)
    }
}

// ===== impl JitterDelayWith =====

impl<S, P> JitterDelayWith<S, P> {
    /// Creates a new [`JitterDelayWith`].
    #[inline]
    pub fn new(inner: S, base: Duration, pct: f64, predicate: P) -> Self {
        Self {
            inner: JitterDelay::new(inner, base, pct),
            predicate,
        }
    }
}

impl<S, Req, P> Service<Req> for JitterDelayWith<S, P>
where
    S: Service<Req>,
    S::Error: Into<BoxError>,
    P: Fn(&Req) -> bool,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = ResponseFuture<S::Future>;

    #[inline]
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let delay = if (self.predicate)(&req) {
            jittered_duration(self.inner.base, self.inner.pct)
        } else {
            Duration::ZERO
        };

        let sleep = tokio::time::sleep(delay);
        let fut = self.inner.inner.call(req);
        ResponseFuture::new(fut, sleep)
    }
}
