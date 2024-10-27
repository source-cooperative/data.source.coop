pub mod auth;
pub mod context;
pub mod core;
pub mod errors;
pub mod repository;

use actix_web::body::{BodySize, MessageBody};
use bytes::Bytes;
use std::pin::Pin;
use std::task::{Context, Poll};

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
    ) -> Poll<Option<Result<Bytes, Self::Error>>> {
        Poll::Ready(None)
    }
}
