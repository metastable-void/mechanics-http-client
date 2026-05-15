//! Request-body plumbing.

use bytes::Bytes;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full};
use std::convert::Infallible;
use std::error::Error as StdError;

pub(crate) type BoxError = Box<dyn StdError + Send + Sync>;

/// Type-erased request body.
pub(crate) type RequestBody = UnsyncBoxBody<Bytes, BoxError>;

pub(crate) fn empty_body() -> RequestBody {
    Empty::<Bytes>::new()
        .map_err(|error: Infallible| match error {})
        .boxed_unsync()
}

pub(crate) fn full_body(bytes: Bytes) -> RequestBody {
    Full::new(bytes)
        .map_err(|error: Infallible| match error {})
        .boxed_unsync()
}

pub(crate) fn streaming_body<B>(body: B) -> RequestBody
where
    B: http_body::Body<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    body.map_err(Into::into).boxed_unsync()
}
