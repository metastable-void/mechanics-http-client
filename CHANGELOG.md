# Changelog

## [0.2.4] - 2026-05-15

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
