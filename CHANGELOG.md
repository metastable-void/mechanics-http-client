# Changelog

## [0.2.4] - 2026-05-15

### Fixed
- QUIC client transport config now sets
  `keep_alive_interval = 15s` and `max_idle_timeout = 120s`.
  Without keep-alives the QUIC connection silently dies at
  NAT / stateful-firewall idle-eviction (typically 30-60s),
  and the next request through the cached h3 connection
  surfaces as `Error::Cancelled` with no recovery path. Pairs
  with the matching mhs 0.1.4 server-side setting.
- New `request_http3_with_stale_retry` helper: when an h3
  attempt against a CACHED connection fails on a pre-wire
  `send_request` (cached connection went stale between
  requests), `try_http3` now transparently re-handshakes a
  fresh h3 connection and retries ONCE before falling through
  to h1/h2. The negative-cache insertion is deferred until
  after both attempts fail, so a one-off stale-connection
  hiccup doesn't poison the origin for the negative-cache
  TTL.
- The HTTPS-RR and Alt-Svc branches in `try_http3` now share
  this retry helper instead of duplicating the
  match-on-Http3AttemptError arms. Same behaviour, less
  drift surface.
- 500 ms `H3_CONNECT_TIMEOUT` wraps both
  `quinn::Endpoint::connect().await` and
  `h3::client::builder().build(quic).await` so a half-finished
  QUIC handshake or h3-setup can't block the request
  indefinitely; on timeout the request falls back to h1/h2
  via the existing retry / negative-cache path. Tightened from
  3 s — the support-chat path must fall back quickly when H3
  is stale or unreachable.
- 150 ms `H3_STREAM_OPEN_TIMEOUT` wraps
  `sender.send_request()`. A cached `h3::client::SendRequest`
  whose underlying QUIC connection has gone stale can hang on
  the bidi-stream-open step — pre-wire, no bytes ever sent —
  and consume the full outer mechanics timeout. The new
  bound surfaces a stale sender as
  `Error::Cancelled { retry_without_h3: true }`, identical
  to an immediate stream-open failure: cached connection
  evicted, fresh H3 retry attempted, then TCP fallback.
- 150 ms `H3_HTTPS_RR_LOOKUP_TIMEOUT` wraps the
  `https_rr::lookup` DNS probe. Slow DNS no longer blocks
  the H3 attempt; lookup-timeout falls through to the
  TCP-HTTPS path.
- `try_http3` now checks cached Alt-Svc **before** HTTPS-RR
  lookup. Second-and-later requests to an origin that
  already advertised `Alt-Svc: h3=...` on the first
  response skip the DNS probe entirely. (HTTPS-RR is still
  checked as a fallback when no Alt-Svc entry is cached.)
- Cached `h3::client::SendRequest` mutex is now released as
  soon as `send_request` returns the stream, rather than
  being held for the lifetime of the stream. Concurrent
  requests against the same cached h3 connection no longer
  serialise on stream-data-send / response-read.
- H3 response path now drains trailers via
  `stream.recv_trailers().await` after the DATA-frame loop
  before returning the buffered `Response`. Without this,
  the response stream stays "unfinished" on the reused QUIC
  connection — the first request succeeds, but the next
  request on the same connection hangs until the mechanics
  300 s timeout fires. Trailers themselves are discarded
  (no API surface yet); reading them is what completes the
  stream.

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
