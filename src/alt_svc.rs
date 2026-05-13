//! Alt-Svc parsing and cache entries for opportunistic HTTP/3.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use http::HeaderValue;

use crate::client::Origin;

/// Cached `Alt-Svc` HTTP/3 alternative for an origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AltSvcEntry {
    /// Alternative host. `None` means the origin host.
    pub host: Option<String>,
    /// Alternative UDP port.
    pub port: u16,
    /// Expiry derived from the `ma` directive.
    pub expires_at: Instant,
}

/// Per-client Alt-Svc cache.
pub type AltSvcCache = HashMap<Origin, AltSvcEntry>;

/// Result of parsing an `Alt-Svc` response header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AltSvcUpdate {
    /// The header was `clear`; evict cached alternatives.
    Clear,
    /// A valid `h3` alternative was advertised.
    Entry(AltSvcEntry),
    /// No supported HTTP/3 alternative was present.
    None,
}

pub(crate) fn fresh(entry: &AltSvcEntry, now: Instant) -> bool {
    entry.expires_at > now
}

pub(crate) fn parse_header(value: &HeaderValue, now: Instant, origin_port: u16) -> AltSvcUpdate {
    let Ok(raw) = value.to_str() else {
        return AltSvcUpdate::None;
    };
    parse_str(raw, now, origin_port)
}

pub(crate) fn parse_str(raw: &str, now: Instant, origin_port: u16) -> AltSvcUpdate {
    if raw.trim().eq_ignore_ascii_case("clear") {
        return AltSvcUpdate::Clear;
    }

    for alternative in split_quoted(raw, ',') {
        let candidate = parse_alternative(alternative.trim(), now, origin_port);
        if !matches!(candidate, AltSvcUpdate::None) {
            return candidate;
        }
    }

    AltSvcUpdate::None
}

fn parse_alternative(raw: &str, now: Instant, origin_port: u16) -> AltSvcUpdate {
    let Some((protocol, rest)) = raw.split_once('=') else {
        return AltSvcUpdate::None;
    };
    if protocol.trim() != "h3" {
        return AltSvcUpdate::None;
    }

    let mut parts = split_quoted(rest, ';');
    let Some(authority_raw) = parts.next() else {
        return AltSvcUpdate::None;
    };
    let Some((host, port)) = parse_authority(authority_raw.trim(), origin_port) else {
        return AltSvcUpdate::None;
    };

    let mut max_age = Duration::from_secs(24 * 60 * 60);
    for part in parts {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("ma")
            && let Ok(seconds) = value.trim().trim_matches('"').parse::<u64>()
        {
            max_age = Duration::from_secs(seconds);
        }
    }

    AltSvcUpdate::Entry(AltSvcEntry {
        host,
        port,
        expires_at: now + max_age,
    })
}

fn parse_authority(raw: &str, origin_port: u16) -> Option<(Option<String>, u16)> {
    let authority = raw.strip_prefix('"')?.strip_suffix('"')?;
    if let Some(port) = authority.strip_prefix(':') {
        return port.parse::<u16>().ok().map(|port| (None, port));
    }

    let (host, port) = authority.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    if host.is_empty() {
        return None;
    }
    let parsed_port = port.parse::<u16>().unwrap_or(origin_port);
    Some((Some(host.to_owned()), parsed_port))
}

fn split_quoted(raw: &str, separator: char) -> impl Iterator<Item = &str> {
    struct SplitQuoted<'a> {
        raw: &'a str,
        separator: char,
        offset: usize,
    }

    impl<'a> Iterator for SplitQuoted<'a> {
        type Item = &'a str;

        fn next(&mut self) -> Option<Self::Item> {
            if self.offset > self.raw.len() {
                return None;
            }
            let start = self.offset;
            let mut quoted = false;
            for (relative, ch) in self.raw[start..].char_indices() {
                if ch == '"' {
                    quoted = !quoted;
                } else if ch == self.separator && !quoted {
                    let end = start + relative;
                    self.offset = end + ch.len_utf8();
                    return Some(&self.raw[start..end]);
                }
            }
            self.offset = self.raw.len() + 1;
            Some(&self.raw[start..])
        }
    }

    SplitQuoted {
        raw,
        separator,
        offset: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{AltSvcUpdate, parse_str};
    use std::time::Instant;

    #[test]
    fn parses_h3_authority_and_max_age() {
        let now = Instant::now();
        let parsed = parse_str(r#"h3=":8443"; ma=60"#, now, 443);
        let AltSvcUpdate::Entry(entry) = parsed else {
            panic!("expected entry");
        };
        assert_eq!(entry.host, None);
        assert_eq!(entry.port, 8443);
        assert!(entry.expires_at > now);
    }

    #[test]
    fn ignores_draft_h3_variants() {
        let parsed = parse_str(r#"h3-29=":443"; ma=60"#, Instant::now(), 443);
        assert!(matches!(parsed, AltSvcUpdate::None));
    }

    #[test]
    fn parses_clear() {
        let parsed = parse_str("clear", Instant::now(), 443);
        assert!(matches!(parsed, AltSvcUpdate::Clear));
    }
}
