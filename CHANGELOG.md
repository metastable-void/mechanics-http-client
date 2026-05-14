# Changelog

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
