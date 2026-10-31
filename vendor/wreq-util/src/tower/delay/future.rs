use std::{
    pin::Pin,
    task::{Context, Poll, ready},
};

use pin_project_lite::pin_project;
use tokio::time::Sleep;
use tower::BoxError;

pin_project! {
    /// Response future for [`Delay`].
    ///
    /// [`Delay`]: super::Delay
    #[derive(Debug)]
    pub struct ResponseFuture<S> {
        #[pin]
        response: S,
        #[pin]
        sleep: Sleep,
    }
}

impl<S> ResponseFuture<S> {
    // Create a new [`ResponseFuture`]
    #[inline]
    pub(crate) fn new(response: S, sleep: Sleep) -> Self {
        ResponseFuture { response, sleep }
    }
}

impl<F, S, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<S, E>>,
    E: Into<BoxError>,
{
    type Output = Result<S, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        ready!(this.sleep.poll(cx));
        this.response.poll(cx).map_err(Into::into)
    }
}
