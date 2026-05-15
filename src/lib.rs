//! A small, [`reqwest`]-shaped HTTP client built on
//! [`hyper`] 1.x + [`hyper_rustls`], with [`aws_lc_rs`] as the
//! sole crypto provider and the bundled [`webpki_roots`] Mozilla
//! CA bundle as the only TLS trust store.
//!
//! The crate is owned by the mechanics family and **does not
//! depend on any philharmonic-family crate**.
//!
//! [`reqwest`]: https://docs.rs/reqwest
//! [`hyper`]: https://docs.rs/hyper
//! [`hyper_rustls`]: https://docs.rs/hyper-rustls
//! [`aws_lc_rs`]: https://docs.rs/aws-lc-rs
//! [`webpki_roots`]: https://docs.rs/webpki-roots
//!
//! # Quick example
//!
//! ```no_run
//! use mechanics_http_client::{Client, StatusCode};
//! use std::time::Duration;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::builder()
//!     .timeout(Duration::from_secs(10))
//!     .user_agent("my-app/1.0")
//!     .build()?;
//!
//! let response = client
//!     .post("https://httpbin.org/anything")
//!     .bearer_auth("secret-token")
//!     .header("x-trace-id", "abc123")
//!     .json(&serde_json::json!({ "hello": "world" }))
//!     .send()
//!     .await?;
//!
//! assert_eq!(response.status(), StatusCode::OK);
//! let body: serde_json::Value = response.json().await?;
//! println!("server replied: {body}");
//! # Ok(()) }
//! ```
//!
//! # Public surface
//!
//! - [`Client`] / [`ClientBuilder`] — connection-pool-managed
//!   HTTPS client. ALPN negotiates HTTP/1.1 or HTTP/2; TLS trust
//!   store is the bundled Mozilla CA bundle (`webpki-roots`) only.
//! - [`RequestBuilder`] — chainable per-request configuration
//!   (`.header(...)`, `.bearer_auth(...)`, `.timeout(...)`,
//!   `.body(...)`, `.json(...)`, `.send().await`).
//! - [`Response`] — `.status()`, `.headers()`, `.version()`,
//!   `.content_length()`, `.bytes()`, `.bytes_with_cap()`,
//!   `.text()`, `.json::<T>()`. `Content-Encoding: gzip`,
//!   `deflate`, `br` decompressed transparently in `bytes()` /
//!   `text()` / `json()` paths.
//! - [`Error`] / [`Result`] — structured error model. Non-2xx
//!   responses are intentionally **not** errors at this layer
//!   (inspect [`Response::status`] instead).
//!
//! # Bounded-memory body reading
//!
//! [`Response::bytes_with_cap`] reads the body up to a cap on
//! **wire bytes** (post-TLS, pre-decompression), surfacing
//! [`Error::BodyTooLarge`] if exceeded. A small compressed body
//! that expands past the cap is allowed through; defend against
//! decompression bombs at a higher layer if your call site cares.
//!
//! # What's deliberately out of scope (for now)
//!
//! - Multipart form bodies, cookies, proxies, redirect-following
//!   knobs.
//! - Streaming `chunk()`-style response iteration.
//! - Server-side HTTP/3.
//!
//! # TLS posture (load-bearing)
//!
//! - **Trust store:** `webpki-roots` bundled Mozilla CA bundle,
//!   frozen at this crate's compile time. No OS-native trust, no
//!   `rustls-platform-verifier`, no `rustls-native-certs`.
//! - **Crypto provider:** `aws-lc-rs`. Installed lazily as
//!   rustls's default `CryptoProvider` on first
//!   `Client::builder().build()`. No `ring`.
//! - **Protocol versions:** rustls defaults (TLS 1.2 and 1.3).

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

#[cfg(feature = "http3")]
mod alt_svc;
mod body;
mod client;
mod dns;
mod error;
#[cfg(feature = "http3")]
mod http3;
#[cfg(feature = "http3")]
mod https_rr;
mod request;
mod response;
mod tls;

#[cfg(test)]
mod tests;

pub use client::{Client, ClientBuilder};
pub use error::{Error, Result};
pub use request::RequestBuilder;
pub use response::Response;

// Re-export the standard `http` types so consumers don't need a
// parallel `http` dep just to pass HeaderName / HeaderValue /
// Method / StatusCode through the API.
pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
