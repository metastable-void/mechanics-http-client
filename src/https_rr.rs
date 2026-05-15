//! HTTPS DNS RR lookup and cache entries for opportunistic HTTP/3.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use mechanics_dns::{HttpsRecord, Resolver};

use crate::client::Origin;
use crate::error::{Error, Result};

/// Cached HTTPS RR data for an origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpsRrEntry {
    /// UDP port to use for the QUIC attempt.
    pub port: u16,
    /// Address hints from `ipv4hint` and `ipv6hint`.
    pub addresses: Vec<IpAddr>,
    /// Whether the RR advertised `h3` in `alpn`.
    pub has_h3: bool,
    /// Expiry derived from the RR TTL.
    pub expires_at: Instant,
}

/// Per-client HTTPS RR cache.
pub type HttpsRrCache = HashMap<Origin, HttpsRrEntry>;

pub(crate) fn fresh(entry: &HttpsRrEntry, now: Instant) -> bool {
    entry.expires_at > now
}

pub(crate) async fn lookup(resolver: &Resolver, origin: &Origin) -> Result<Option<HttpsRrEntry>> {
    let records = resolver
        .lookup_https(origin.host.as_str())
        .await
        .map_err(|e| Error::Dns(e.to_string()))?;

    let mut best = None;
    for record in records {
        let parsed = parse_https_record(&record, origin.port);
        if parsed.has_h3 {
            return Ok(Some(parsed));
        }
        best.get_or_insert(parsed);
    }

    Ok(best)
}

pub(crate) fn parse_https_record(record: &HttpsRecord, origin_port: u16) -> HttpsRrEntry {
    HttpsRrEntry {
        port: record.port.unwrap_or(origin_port),
        addresses: record.address_hints().collect(),
        has_h3: record.has_alpn("h3"),
        expires_at: record.expires_at,
    }
}
