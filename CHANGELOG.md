# Changelog

## [Unreleased]

### Changed
- DNS resolution now goes through the shared `mechanics-dns`
  crate for TCP/TLS connection resolution, HTTP/3 fallback
  address resolution, and HTTPS RR lookup. Hosts with a normal
  resolver configuration keep using that configuration; hosts
  where `/etc/resolv.conf` is absent now fall back to the
  documented Cloudflare resolver set instead of failing during
  resolver setup.

## [0.2.4] - 2026-05-15

### Fixed
- **H3 connections are now per-request disposable.** The
  per-origin cached `Arc<AsyncMutex<H3SendRequest>>` table is
  gone; every H3 attempt builds a fresh QUIC connection and a
  fresh `h3::client` against the (still cached) QUIC endpoint,
  uses it for one request, then lets the driver task wind it
  down. This removes the entire class of "non-first request
  against a stale cached H3 sender" failures (`Error::Cancelled`
  before any wire bytes are sent, concurrent jobs serialised on
  one sender's mutex, response-stream-unfinished hang on
  reuse) at the cost of one extra QUIC handshake per H3 request.
  For the support-chat path that trade is well worth it — chat
  flows are dominated by the round-trip to the LLM provider,
  not by the local connect cost; for the connector-router
  hop the cost is loopback-latency anyway. The QUIC endpoint
  itself is still cached, so the rustls / crypto-provider /
  UDP-socket setup happens once per `Client`.
- QUIC client transport config sets `keep_alive_interval = 15s`
  and `max_idle_timeout = 120s` on the still-cached QUIC
  endpoint. With per-request H3 connections, keep-alives
  matter mostly for streaming responses that span the 30–60 s
  NAT-state TTL window; the longer max-idle prevents the
  driver task from spuriously closing a connection that's
  still in active use. Pairs with the matching mhs 0.1.4
  server-side setting.
- `request_http3_with_stale_retry` retries an H3 attempt once
  when the first attempt fails pre-wire (handshake completed,
  `send_request` rejected before reading any request bytes,
  `retry_without_h3 = true`). The retry opens an independent
  fresh QUIC + H3 connection — no cache to evict, no shared
  state between attempts — and falls through to h1/h2 if it
  also fails. Negative-cache insertion is deferred until
  both attempts fail, so a one-off pre-wire hiccup doesn't
  poison the origin for the negative-cache TTL.
- The HTTPS-RR and Alt-Svc branches in `try_http3` share this
  retry helper instead of duplicating the
  match-on-`Http3AttemptError` arms. Same behaviour, less
  drift surface.
- 500 ms `H3_CONNECT_TIMEOUT` wraps both
  `quinn::Endpoint::connect().await` and
  `h3::client::builder().build(quic).await` so a half-finished
  QUIC handshake or h3-setup can't block the request
  indefinitely; on timeout the request falls back to h1/h2
  via the existing retry / negative-cache path. Tightened from
  3 s — the support-chat path must fall back quickly when H3
  is stale or unreachable.
- 150 ms `H3_STREAM_OPEN_TIMEOUT` wraps `send_request()`. A
  fresh `h3::client::SendRequest` that hangs on the
  bidi-stream-open step (pre-wire, no bytes ever sent)
  surfaces as `Error::Cancelled { retry_without_h3: true }`,
  identical to an immediate stream-open failure: fresh H3
  retry attempted, then TCP fallback. With no cached sender
  mutex, there's no lock-acquisition latency to bound.
- 150 ms `H3_HTTPS_RR_LOOKUP_TIMEOUT` wraps the
  `https_rr::lookup` DNS probe. Slow DNS no longer blocks
  the H3 attempt; lookup-timeout falls through to the
  TCP-HTTPS path.
- 150 ms `H3_DNS_LOOKUP_TIMEOUT` wraps the fallback
  `tokio::net::lookup_host` resolution used when the H3
  target host isn't already an IP literal and the
  Alt-Svc / HTTPS-RR records gave a host name rather than
  an address. Previously this was unbounded — a slow
  system resolver could stall the H3 attempt for the full
  outer mechanics timeout; now it surfaces as a handshake-
  level failure that falls back to TCP HTTPS via the
  existing retry / negative-cache path.
- 500 ms `H3_STREAM_UPLOAD_TIMEOUT` wraps both
  `stream.send_data()` (request body upload) and
  `stream.finish()` (request half-close) via a new
  `h3_stream_phase` helper. Previously these were
  unbounded — a peer that ACKed the stream open but stopped
  reading could hang the entire request indefinitely; now
  the request fails with
  `Error::Cancelled { retry_without_h3: false }` so the
  request is not duplicated on h1/h2 (already on the
  wire), with the phase name ("request body send" /
  "request finish") in the error string for operator
  diagnosis.
- 3 s `DEFAULT_CONNECT_TIMEOUT` set on the underlying
  `hyper_util::HttpConnector` for the h1/h2/HTTPS path.
  Previously the TCP `connect()` was bounded only by the
  OS-level handshake timeout (typically 30–75 s) plus the
  outer mechanics timeout; now a black-holed peer surfaces
  as `Error::Unreachable` quickly enough for the
  support-chat path to render a recoverable error to the
  user instead of stalling for minutes.
- `try_http3` checks cached Alt-Svc **before** HTTPS-RR
  lookup. Second-and-later requests to an origin that
  already advertised `Alt-Svc: h3=...` on the first
  response skip the DNS probe entirely. (HTTPS-RR is still
  checked as a fallback when no Alt-Svc entry is cached.)
- H3 response path treats DATA EOF as response-body
  completion and does not wait for optional trailers. The
  prior `stream.recv_trailers().await` call was added to
  finish the response stream so a *reused* QUIC connection
  stayed in a reusable state; with per-request connections
  there's nothing to keep reusable, and waiting on trailers
  the h3 stack may never resolve promptly turns a complete
  response body into a stuck mechanics endpoint future. The
  caller has no trailer API surface, so this is a pure
  behaviour fix — no API change.
- **H3 response body now streams.** Previously, the H3 path
  buffered every DATA frame into a `BytesMut` before
  constructing a `Response`, so an H3 caller had to wait for
  upstream EOF before even reading response headers — the
  symmetric mistake the connector-router forwarder fix
  already corrected on the h1/h2 buffering side. A new
  internal `H3ResponseBody` type implements
  `http_body::Body<Data = Bytes, Error = Error>`, polling
  each `recv_data` call as its own DATA frame; `Response`
  now carries either `Hyper(Incoming)` (h1/h2) or
  `H3(Box<H3ResponseBody>)` (h3) and flows both through
  `bytes()` / `bytes_with_cap()` / `into_body()` identically.
  Streaming forwarders (`connector-router` `HyperForwarder`)
  now get the same headers-first-then-DATA-as-it-arrives
  behaviour on H3 that they already get on h1/h2.

### Added
- `RequestBuilder::body_streaming(body)` accepts any
  `http_body::Body<Data = Bytes>` impl, forwarding it as-is
  to the upstream HTTP/1.1 or HTTP/2 connection without
  buffering. Internally, the request body now goes through
  a `RequestPayload` enum (`Empty | Replayable(Bytes) |
  Streaming(RequestBody)`). H3 attempts only fire for
  `Empty` / `Replayable` payloads — a `Streaming` body
  can't be safely replayed after an H3 failure, so those
  requests bypass H3 and use the negotiated TCP transport
  path directly. The connector-router uses this to forward
  inbound bodies without the prior `BodyExt::collect()`
  buffering step that blocked the upstream TCP dial.
- `Response::into_body()` returns the raw response body as
  `UnsyncBoxBody<Bytes, Error>` for forwarders that need to
  stream the response back to their caller without
  buffering. This path does **not** transparently
  decompress `Content-Encoding`, so callers forwarding the
  body must preserve the `Content-Encoding` header verbatim.
  The decompressing `bytes()` / `text()` / `json()` paths
  are unchanged; new path is opt-in via `into_body()`.

### Changed
- Body-frame error mapping factored into a private
  `map_hyper_body_error` helper used by both
  `collect_body_with_cap` and the new streaming-body path.
  Same `Cancelled` / `Unreachable` classification as before;
  the move just stops duplicating the heuristic across two
  consumers.

### Fixed (mildly breaking on the internal `Http3AttemptError`)
- `try_http3` failures now distinguish "no wire bytes sent
  yet, safe to fall back to h1/h2" from "request started on
  the wire, retrying would be a duplicate." `Http3AttemptError::Stream`
  is now a struct variant `{ error, retry_without_h3 }`:
  - `send_request` failures (handshake completed, but the
    server rejected before reading any request bytes) carry
    `retry_without_h3 = true` — `try_http3` then evicts the
    cached h3 connection, inserts a negative-cache entry,
    and returns `Ok(None)` so the caller transparently falls
    through to the h1/h2 path. This is the case the
    `mhc 0.2.3` operator hit when `Error: request cancelled`
    propagated all the way to the JS layer instead of
    silently degrading to HTTP/2 over TLS.
  - `send_data` / `finish` / `recv_response` / `recv_data`
    failures carry `retry_without_h3 = false`. The request
    is already on the wire; falling back would send a
    duplicate, so the error propagates.
- `try_http3` also evicts the cached h3 connection on any
  stream-level error (both retry-paths) and inserts a
  negative-cache entry so the next request to the same
  origin skips h3 for a while.
- Added a per-attempt timeout for the h3 path so an h3
  handshake or stream that never completes can't block the
  whole request indefinitely.

`Http3AttemptError` is an internal-only `pub(crate)` enum, so
this variant reshape is not a SemVer break of the public
surface; tracking the change here for the audit trail.

## [0.2.3] - 2026-05-15

### Changed (mildly breaking; no in-workspace pattern matchers)
- `Error::Cancelled` now carries a `String` detail. Display is
  `"request cancelled: <detail>"`. Previously it was a unit
  variant whose Display was just `"request cancelled"`, hiding
  the underlying hyper / h3 reason from operators — the
  symptom "Error: request cancelled" with no further context
  is exactly what an api-server operator hit on 2026-05-14
  when the connector-router's plain-HTTP forwarder triggered
  a peer-side stream close mid-response. The detail now
  surfaces hyper's actual message ("connection closed before
  message completed", "stream cancel from peer", etc.) so the
  next diagnostic round doesn't need a guess-and-recompile
  loop.
- Removed the (dead) `ErrorWithMessage` trait that
  special-cased `Cancelled` to drop the message it claimed to
  attach. The h3 emission sites now construct
  `Error::Cancelled(e.to_string())` directly.

## [0.2.2] - 2026-05-14

### Changed
- Internal Cargo.toml audit: `default-features = false` set on
  direct dependencies with explicit feature lists for what the
  crate actually uses. No behaviour change. (D24)

## [0.2.1] - 2026-05-13

- Security: bump `hickory-resolver` 0.25.2 -> 0.26.1 to pick up upstream
  fixes for `RUSTSEC-2026-0118` (HIGH; only triggerable with DNSSEC features
  on, not applicable to mhc's config) and `RUSTSEC-2026-0119` (MEDIUM;
  BinEncoder O(n^2) during DNS message encoding, triggerable in mhc's config
  when resolving HTTPS RRs against an attacker-influenced authoritative
  server).
- Internal: imports under `hickory_resolver::proto::*` now use
  `hickory_resolver::net::proto::*` per upstream's `hickory-proto` ->
  `hickory-net` rename. No change to mhc's public API.

## [0.2.0] - 2026-05-13

- Add default-on `http3` feature for ROADMAP D22 client-side HTTP/3:
  HTTPS RR discovery, Alt-Svc caching, opportunistic QUIC, and fallback
  to the existing hyper HTTP/2 / HTTP/1.1 path.

## [0.1.0] - 2026-05-13

- Initial hyper-rustls client with bundled webpki roots and aws-lc-rs.
