# mechanics-http-client

A small, [`reqwest`](https://docs.rs/reqwest)-shaped HTTP client
built on [`hyper`](https://docs.rs/hyper) 1.x +
[`hyper-rustls`](https://docs.rs/hyper-rustls), with
[`aws-lc-rs`](https://docs.rs/aws-lc-rs) as the sole crypto
provider and the bundled
[`webpki-roots`](https://docs.rs/webpki-roots) Mozilla CA
bundle as the only TLS trust store.

Owned by the mechanics family. **Does not depend on any
`philharmonic-*` crate** — Philharmonic-family consumers depend
on this crate, never the reverse, per the workspace's
Mechanics-Philharmonic independence rule.

## What you get

- `Client` / `ClientBuilder` — connection-pool-managed HTTPS
  client. ALPN negotiates **HTTP/1.1 or HTTP/2**. Cheap to
  clone (`Arc`-wrapped state).
- `RequestBuilder` — chainable per-request configuration:
  `.header(...)`, `.bearer_auth(...)`, `.timeout(...)`,
  `.body(...)`, `.json(...)`, `.send().await`.
- `Response` — `.status()`, `.headers()`, `.version()`,
  `.content_length()`, `.bytes()`, `.bytes_with_cap()`,
  `.text()`, `.json::<T>()`. Transparent decompression of
  `Content-Encoding: gzip`, `deflate`, or `br`.
- `Error` / `Result` — structured error model (`Timeout`,
  `Unreachable`, `Tls`, `Decode`, `BodyTooLarge`,
  `InvalidUrl`, `InvalidHeader`, `SerializeJson`,
  `Cancelled`, `Internal`). Non-2xx responses are
  intentionally **not** errors; inspect `Response::status`.

## TLS posture (load-bearing)

This is the reason the crate exists, so spelling it out:

- **Trust store:** bundled Mozilla CA bundle via
  `webpki-roots`, frozen at this crate's compile time. No
  OS-native trust, no `rustls-platform-verifier`, no
  `rustls-native-certs`. Reproducibility and portability are
  preferred over picking up the host's local trust additions.
- **Crypto provider:** `aws-lc-rs`. Installed lazily as
  rustls's default `CryptoProvider` on the first
  `Client::builder().build()` call. No `ring`.
- **Protocol versions:** rustls defaults (TLS 1.2 and 1.3).

## Quick example

```rust,no_run
use mechanics_http_client::{Client, StatusCode};
use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct Echo {
    json: serde_json::Value,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("my-app/1.0")
        .build()?;

    let response = client
        .post("https://httpbin.org/anything")
        .bearer_auth("secret-token")
        .header("x-trace-id", "abc123")
        .json(&serde_json::json!({ "hello": "world" }))
        .send()
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let echo: Echo = response.json().await?;
    println!("server received: {}", echo.json);
    Ok(())
}
```

For bounded-memory body reading (e.g. cap at 1 MiB of wire
bytes, surface `Error::BodyTooLarge` if exceeded):

```rust,no_run
# use mechanics_http_client::Client;
# async fn run() -> Result<(), mechanics_http_client::Error> {
let client = Client::new()?;
let response = client.get("https://example.com/big").send().await?;
let bytes = response.bytes_with_cap(1024 * 1024).await?;
# Ok(()) }
```

The cap applies to **wire bytes** (post-TLS, pre-decompression).
A small compressed body that expands past the cap is allowed
through; defend against decompression bombs at a higher layer
if your call site cares.

## Out of scope (for now)

These are deliberately omitted; add them when a call site
actually needs them:

- Multipart form bodies.
- Cookies.
- Proxy configuration.
- Redirect-following knobs (the underlying
  `hyper_util::client::legacy::Client` does not follow
  redirects by default; a 3xx response surfaces verbatim).
- Response streaming via a `chunk()`-style iterator.
  `bytes_with_cap` covers the existing wire-byte-cap use case.
- HTTP/3.

## MSRV

Rust 1.88. Workspace-wide policy lives in
[`CONTRIBUTING.md`](https://github.com/metastable-void/philharmonic-workspace/blob/main/CONTRIBUTING.md).

## License

Dual-licensed under `Apache-2.0 OR MPL-2.0`.

## Contributing

Developed as part of the
[mechanics-rs](https://github.com/metastable-void/mechanics-rs)
family, under the
[philharmonic-workspace](https://github.com/metastable-void/philharmonic-workspace)
parent. Workspace-wide development conventions — git workflow,
script wrappers, Rust code rules, versioning, terminology —
live in the meta-repo, authoritatively in its
[`CONTRIBUTING.md`](https://github.com/metastable-void/philharmonic-workspace/blob/main/CONTRIBUTING.md).

SPDX-License-Identifier: Apache-2.0 OR MPL-2.0
