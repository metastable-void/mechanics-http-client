//! Error model for `mechanics-http-client`.
//!
//! Inspired by reqwest's `Error` but narrower in scope — the
//! variants reflect the actual failure modes the workspace's call
//! sites need to discriminate. Non-2xx responses are **not** errors
//! at this layer; the caller inspects [`Response::status`](crate::Response::status)
//! and decides.

use thiserror::Error;

/// Convenience alias: `Result<T, Error>` with this crate's [`enum@Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Failures surfaced by [`Client`](crate::Client) / [`RequestBuilder`](crate::RequestBuilder)
/// / [`Response`](crate::Response). Non-2xx responses are intentionally
/// **not** modelled here; callers inspect
/// [`Response::status`](crate::Response::status) instead.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum Error {
    /// The per-request timeout fired before the response (or one of
    /// its body frames) arrived.
    #[error("request timed out")]
    Timeout,

    /// TCP refused, DNS failed, mid-flight network drop, etc. Not a
    /// TLS-handshake issue (see [`Error::Tls`]); not a server-side
    /// non-success (see [`Response::status`](crate::Response::status)).
    #[error("upstream unreachable: {0}")]
    Unreachable(String),

    /// TLS handshake failure: certificate validation, ALPN mismatch,
    /// supported-version mismatch, etc.
    #[error("TLS error: {0}")]
    Tls(String),

    /// Body or header decoding failed: invalid Content-Encoding,
    /// non-UTF-8 in [`Response::text`](crate::Response::text), invalid
    /// JSON in [`Response::json`](crate::Response::json), unsupported
    /// compression scheme, etc.
    #[error("response decode error: {0}")]
    Decode(String),

    /// The response body exceeded the cap passed to
    /// [`Response::bytes_with_cap`](crate::Response::bytes_with_cap).
    /// The cap applies to **wire bytes** (post-TLS, pre-decompression).
    #[error("response body exceeded cap: limit={limit}, seen={seen}")]
    BodyTooLarge {
        /// The cap, in wire bytes, that was exceeded.
        limit: usize,
        /// How many wire bytes had been buffered when the cap was hit.
        seen: usize,
    },

    /// The URL passed to a request builder did not parse, or had a
    /// scheme other than `http` / `https`.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// A header name or value supplied via
    /// [`RequestBuilder::header`](crate::RequestBuilder::header) /
    /// [`ClientBuilder::default_headers`](crate::ClientBuilder::default_headers)
    /// was not valid per RFC 7230.
    #[error("invalid header: {0}")]
    InvalidHeader(String),

    /// JSON serialisation of the request body failed (rare —
    /// `serde_json` only errors here for non-`Serialize`-able types or
    /// keys that aren't strings).
    #[error("request JSON serialise: {0}")]
    SerializeJson(String),

    /// The connection was cancelled mid-flight (e.g. peer reset,
    /// HTTP/2 GOAWAY).
    #[error("request cancelled")]
    Cancelled,

    /// Catch-all for client-construction or hyper-internal errors
    /// that don't map cleanly to the variants above.
    #[error("internal HTTP client error: {0}")]
    Internal(String),
}

impl Error {
    /// True iff the error was caused by the per-request timeout
    /// firing.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Timeout)
    }

    /// True iff the error indicates the upstream was unreachable
    /// (TCP / DNS / mid-flight drop) — distinct from non-2xx
    /// responses (which are not errors at this layer).
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Error::Unreachable(_))
    }
}
