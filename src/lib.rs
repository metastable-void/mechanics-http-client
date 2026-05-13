//! `mechanics-http-client` — a small, reqwest-shaped HTTP client built
//! on `hyper-rustls` + `webpki-roots`, with `aws-lc-rs` as the sole
//! crypto provider.
//!
//! The crate is owned by the mechanics family and **does not depend
//! on any philharmonic-family crate**. Consumers throughout the
//! workspace (mechanics-core, the connector implementations, the
//! API server binary, etc.) drive their outbound HTTPS through this
//! crate's `Client`.
//!
//! ## Public surface
//!
//! - [`Client`] / [`ClientBuilder`] — connection-pool-managed
//!   HTTPS client. ALPN negotiates HTTP/1.1 or HTTP/2; TLS trust
//!   store is the bundled Mozilla CA bundle (`webpki-roots`) only.
//! - [`RequestBuilder`] — chainable per-request configuration
//!   (`.header(...)`, `.bearer_auth(...)`, `.timeout(...)`,
//!   `.body(...)`, `.json(...)`, `.send().await`).
//! - [`Response`] — `.status()`, `.headers()`, `.bytes()`,
//!   `.bytes_with_cap()`, `.text()`, `.json::<T>()`.
//!   `Content-Encoding: gzip`, `deflate`, `br` decompressed
//!   transparently in `bytes()` / `text()` / `json()` paths.
//! - [`Error`] / [`Result`] — structured error model
//!   (`Timeout`, `Unreachable`, `Tls`, `Decode`, `Status`,
//!   `BodyTooLarge`, `InvalidUri`, `InvalidHeader`, `Cancelled`,
//!   `Internal`).
//!
//! ## What's deliberately out of scope (for now)
//!
//! - Multipart form bodies, cookies, proxies, redirect-following
//!   knobs. None of the workspace's call sites need them today;
//!   the API can grow as call sites do.
//! - Streaming chunk()-style response iteration. `bytes_with_cap`
//!   covers the workspace's existing "cap-on-body-bytes" use
//!   case. The cap applies to **wire bytes** (post-TLS, pre-
//!   decompression) — a slight tightening of the previous
//!   reqwest-based behavior, but operationally equivalent for
//!   the bounded response sizes the workspace handles.
//! - HTTP/3. Reserved for D22 (later session).
//!
//! ## TLS posture (load-bearing)
//!
//! Trust store: `webpki-roots` bundled Mozilla CA bundle, frozen
//! at this crate's compile time. No OS-native trust, no
//! `rustls-platform-verifier`, no `rustls-native-certs`. The
//! crypto provider is `aws-lc-rs` and is installed lazily on
//! first `Client::builder().build()` via the rustls
//! `CryptoProvider::install_default()` API. No `ring`.

mod body;
mod client;
mod error;
mod request;
mod response;
mod tls;

pub use client::{Client, ClientBuilder};
pub use error::{Error, Result};
pub use request::RequestBuilder;
pub use response::Response;

// Re-export the standard `http` types so consumers don't need a
// parallel `http` dep just to pass HeaderName / HeaderValue /
// Method / StatusCode through the API.
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
