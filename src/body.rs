//! Request-body plumbing. The crate's request paths all carry either
//! no body or a fully-buffered `Bytes` blob; this keeps the body
//! type simple ([`http_body_util::Full<Bytes>`]) and avoids the
//! complexity of streaming request bodies, which no current call
//! site needs.

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use std::convert::Infallible;

/// Type-erased request body. Either empty or a single buffered
/// `Bytes` chunk.
pub(crate) type RequestBody = BoxBody<Bytes, Infallible>;

pub(crate) fn empty_body() -> RequestBody {
    Empty::<Bytes>::new().boxed()
}

pub(crate) fn full_body(bytes: Bytes) -> RequestBody {
    Full::new(bytes).boxed()
}
