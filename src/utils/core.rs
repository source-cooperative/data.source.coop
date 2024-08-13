
use std::pin::Pin;
use std::task::{Context, Poll};
use pin_project_lite::pin_project;
use futures::Stream;
use actix_web::{body::{BodySize, MessageBody}, web, Error as ActixError};

pin_project! {
    pub struct S3ObjectStream<S> {
        #[pin]
        inner: S,
        size: u64,
    }
}

pub struct FakeBody {
    pub size: usize,
}

impl MessageBody for FakeBody {
    type Error = actix_web::Error;
    fn size(&self) -> BodySize {
        BodySize::Sized(self.size as u64)
    }

    fn poll_next(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<web::Bytes, actix_web::Error>>> {
        Poll::Ready(None)
    }
}

impl<S> S3ObjectStream<S> {
    pub fn new(inner: S, size: u64) -> Self {
        Self { inner, size }
    }
}

impl<S> MessageBody for S3ObjectStream<S>
where
    S: Stream,
    S::Item: Into<Result<web::Bytes, ActixError>>,
{
    type Error = ActixError;

    fn size(&self) -> BodySize {
        BodySize::Sized(self.size)
    }

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<web::Bytes, Self::Error>>> {
        let this = self.project();
        match this.inner.poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item.into())),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
