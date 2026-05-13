//! TLS configuration: bundled Mozilla CA roots (`webpki-roots`) +
//! aws-lc-rs crypto provider, explicitly wired so we never depend on
//! whatever provider the global default happens to be.

use std::sync::Arc;

use crate::error::{Error, Result};

/// Build a `rustls::ClientConfig` whose root store is sourced from
/// the bundled Mozilla CA bundle (`webpki-roots::TLS_SERVER_ROOTS`)
/// and whose crypto provider is `aws-lc-rs`. Suitable for wrapping a
/// hyper-rustls `HttpsConnector` via `with_tls_config`.
///
/// Errors only if rustls reports that the provider doesn't support
/// the default protocol-version set — in practice this never
/// happens with aws-lc-rs (TLS 1.2 + 1.3 are unconditionally
/// supported), so callers may treat it as an unrecoverable
/// internal error.
pub(crate) fn webpki_roots_client_config() -> Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| {
            Error::Internal(format!(
                "aws-lc-rs provider rejected default protocol versions: {e}"
            ))
        })?
        .with_root_certificates(roots)
        .with_no_client_auth();

    Ok(config)
}
