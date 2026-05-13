//! HTTPS DNS RR lookup and cache entries for opportunistic HTTP/3.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Instant;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::rdata::svcb::{SvcParamKey, SvcParamValue};
use hickory_resolver::proto::rr::{RData, RecordType};

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

pub(crate) async fn lookup(origin: &Origin) -> Result<Option<HttpsRrEntry>> {
    let resolver = TokioResolver::builder_tokio()
        .map_err(|e| Error::Dns(e.to_string()))?
        .build();
    let lookup = resolver
        .lookup(origin.host.as_str(), RecordType::HTTPS)
        .await
        .map_err(|e| Error::Dns(e.to_string()))?;

    let expires_at = lookup.valid_until();
    let mut best = None;
    for record in lookup.iter() {
        let RData::HTTPS(https) = record else {
            continue;
        };
        let parsed = parse_svcb(https.0.svc_params(), origin.port, expires_at);
        if parsed.has_h3 {
            return Ok(Some(parsed));
        }
        best.get_or_insert(parsed);
    }

    Ok(best)
}

pub(crate) fn parse_svcb(
    params: &[(SvcParamKey, SvcParamValue)],
    origin_port: u16,
    expires_at: Instant,
) -> HttpsRrEntry {
    let mut port = origin_port;
    let mut addresses = Vec::new();
    let mut has_h3 = false;

    for (key, value) in params {
        match (key, value) {
            (SvcParamKey::Alpn, SvcParamValue::Alpn(alpns)) => {
                has_h3 = alpns.0.iter().any(|alpn| alpn == "h3");
            }
            (SvcParamKey::Port, SvcParamValue::Port(discovered_port)) => {
                port = *discovered_port;
            }
            (SvcParamKey::Ipv4Hint, SvcParamValue::Ipv4Hint(hints)) => {
                addresses.extend(
                    hints
                        .0
                        .iter()
                        .filter_map(|hint| hint.to_string().parse::<IpAddr>().ok()),
                );
            }
            (SvcParamKey::Ipv6Hint, SvcParamValue::Ipv6Hint(hints)) => {
                addresses.extend(
                    hints
                        .0
                        .iter()
                        .filter_map(|hint| hint.to_string().parse::<IpAddr>().ok()),
                );
            }
            _ => {}
        }
    }

    HttpsRrEntry {
        port,
        addresses,
        has_h3,
        expires_at,
    }
}
