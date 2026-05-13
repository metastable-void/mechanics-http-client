//! [`Client`] / [`ClientBuilder`]: connection-pool-managed HTTPS
//! client. ALPN negotiates HTTP/1.1 or HTTP/2 on TCP/TLS, and
//! HTTP/3 is attempted opportunistically over QUIC when enabled.

use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "http3")]
use std::{collections::HashMap, sync::RwLock, time::Instant};

use http::{HeaderMap, HeaderName, HeaderValue, Method};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client as HyperLegacyClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

use crate::body::RequestBody;
use crate::error::{Error, Result};
use crate::request::RequestBuilder;
use crate::tls;
#[cfg(feature = "http3")]
use crate::{alt_svc, http3, https_rr};

/// Type alias for the hyper-util client backing each [`Client`].
pub(crate) type HyperClient = HyperLegacyClient<HttpsConnector<HttpConnector>, RequestBody>;

/// HTTPS client built on hyper-rustls + webpki-roots + aws-lc-rs.
///
/// Cheap to clone — internal state is `Arc`-wrapped. Build via
/// [`Client::builder`] (preferred) or [`Client::new`] (defaults).
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<ClientInner>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("default_timeout", &self.inner.default_timeout)
            .field("default_headers", &self.inner.default_headers)
            .finish_non_exhaustive()
    }
}

pub(crate) struct ClientInner {
    pub(crate) hyper: HyperClient,
    pub(crate) default_timeout: Option<Duration>,
    pub(crate) default_headers: HeaderMap,
    #[cfg(feature = "http3")]
    pub(crate) http3_enabled: bool,
    #[cfg(feature = "http3")]
    pub(crate) http3_negative_cache_duration: Duration,
    #[cfg(feature = "http3")]
    pub(crate) https_rr_cache: Arc<RwLock<https_rr::HttpsRrCache>>,
    #[cfg(feature = "http3")]
    pub(crate) alt_svc_cache: Arc<RwLock<alt_svc::AltSvcCache>>,
    #[cfg(feature = "http3")]
    pub(crate) negative_cache: Arc<RwLock<HashMap<Origin, Instant>>>,
    #[cfg(feature = "http3")]
    pub(crate) http3: Arc<http3::Http3State>,
}

/// Scheme/authority tuple used as a cache key for per-origin transport state.
#[cfg(feature = "http3")]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Origin {
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl Client {
    /// Build a [`Client`] with default settings.
    pub fn new() -> Result<Self> {
        ClientBuilder::new().build()
    }

    /// Start building a [`Client`] with custom settings.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::new()
    }

    /// Start a GET request.
    pub fn get(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::GET, url)
    }

    /// Start a POST request.
    pub fn post(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::POST, url)
    }

    /// Start a PUT request.
    pub fn put(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::PUT, url)
    }

    /// Start a PATCH request.
    pub fn patch(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::PATCH, url)
    }

    /// Start a DELETE request.
    pub fn delete(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::DELETE, url)
    }

    /// Start a HEAD request.
    pub fn head(&self, url: impl AsRef<str>) -> RequestBuilder {
        self.request(Method::HEAD, url)
    }

    /// Start a request with an explicit method.
    pub fn request(&self, method: Method, url: impl AsRef<str>) -> RequestBuilder {
        RequestBuilder::new(self.clone(), method, url.as_ref())
    }

    #[cfg(all(feature = "http3", test))]
    pub(crate) fn insert_https_rr_for_test(&self, origin: Origin, entry: https_rr::HttpsRrEntry) {
        if let Ok(mut cache) = self.inner.https_rr_cache.write() {
            cache.insert(origin, entry);
        }
    }

    #[cfg(all(feature = "http3", test))]
    pub(crate) fn insert_alt_svc_for_test(&self, origin: Origin, entry: alt_svc::AltSvcEntry) {
        if let Ok(mut cache) = self.inner.alt_svc_cache.write() {
            cache.insert(origin, entry);
        }
    }

    #[cfg(all(feature = "http3", test))]
    pub(crate) fn insert_negative_for_test(&self, origin: Origin, expires_at: Instant) {
        if let Ok(mut cache) = self.inner.negative_cache.write() {
            cache.insert(origin, expires_at);
        }
    }

    #[cfg(all(feature = "http3", test))]
    pub(crate) fn has_https_rr_for_test(&self, origin: &Origin) -> bool {
        self.inner
            .https_rr_cache
            .read()
            .map(|cache| cache.contains_key(origin))
            .unwrap_or(false)
    }

    #[cfg(all(feature = "http3", test))]
    pub(crate) fn has_alt_svc_for_test(&self, origin: &Origin) -> bool {
        self.inner
            .alt_svc_cache
            .read()
            .map(|cache| cache.contains_key(origin))
            .unwrap_or(false)
    }

    #[cfg(all(feature = "http3", test))]
    pub(crate) fn has_negative_for_test(&self, origin: &Origin) -> bool {
        self.inner
            .negative_cache
            .read()
            .map(|cache| cache.contains_key(origin))
            .unwrap_or(false)
    }
}

/// Builder for [`Client`].
pub struct ClientBuilder {
    timeout: Option<Duration>,
    pool_max_idle_per_host: Option<usize>,
    pool_idle_timeout: Option<Duration>,
    default_headers: HeaderMap,
    user_agent: Option<HeaderValue>,
    invalid_default_header: Option<Error>,
    #[cfg(feature = "http3")]
    http3_enabled: bool,
    #[cfg(feature = "http3")]
    http3_negative_cache_duration: Option<Duration>,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            timeout: None,
            pool_max_idle_per_host: None,
            pool_idle_timeout: None,
            default_headers: HeaderMap::new(),
            user_agent: None,
            invalid_default_header: None,
            #[cfg(feature = "http3")]
            http3_enabled: true,
            #[cfg(feature = "http3")]
            http3_negative_cache_duration: None,
        }
    }
}

impl ClientBuilder {
    /// Create a fresh builder with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Default per-request timeout, applied when the per-call
    /// [`RequestBuilder::timeout`] is unset.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Maximum idle connections kept open per host. `0` disables
    /// connection reuse entirely (useful for bursty workloads
    /// where stale-pool issues outweigh reuse benefits).
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.pool_max_idle_per_host = Some(max);
        self
    }

    /// Drop pooled idle connections after this duration.
    pub fn pool_idle_timeout(mut self, timeout: Duration) -> Self {
        self.pool_idle_timeout = Some(timeout);
        self
    }

    /// Set a default `User-Agent` header for every request from
    /// this client. The per-request `.header("User-Agent", ...)`
    /// overrides this.
    pub fn user_agent(mut self, ua: impl TryInto<HeaderValue>) -> Self {
        match ua.try_into() {
            Ok(value) => self.user_agent = Some(value),
            Err(_) => {
                self.invalid_default_header.get_or_insert_with(|| {
                    Error::InvalidHeader("User-Agent value rejected".to_owned())
                });
            }
        }
        self
    }

    /// Set default request headers. Per-request headers override
    /// these on a name-by-name basis.
    pub fn default_headers(mut self, headers: HeaderMap) -> Self {
        self.default_headers = headers;
        self
    }

    /// Set a single default header. Convenience over
    /// [`Self::default_headers`].
    pub fn default_header(
        mut self,
        name: impl TryInto<HeaderName>,
        value: impl TryInto<HeaderValue>,
    ) -> Self {
        let Ok(name) = name.try_into() else {
            self.invalid_default_header.get_or_insert_with(|| {
                Error::InvalidHeader("default header name rejected".to_owned())
            });
            return self;
        };
        let Ok(value) = value.try_into() else {
            self.invalid_default_header.get_or_insert_with(|| {
                Error::InvalidHeader(format!("default header `{name}` value rejected"))
            });
            return self;
        };
        self.default_headers.insert(name, value);
        self
    }

    /// Enable or disable opportunistic HTTP/3 at runtime.
    #[cfg(feature = "http3")]
    pub fn http3(mut self, enabled: bool) -> Self {
        self.http3_enabled = enabled;
        self
    }

    /// Override the negative-cache duration after HTTP/3 probe failure.
    #[cfg(feature = "http3")]
    pub fn http3_negative_cache_duration(mut self, duration: Duration) -> Self {
        self.http3_negative_cache_duration = Some(duration);
        self
    }

    /// Finalise the builder.
    pub fn build(mut self) -> Result<Client> {
        if let Some(err) = self.invalid_default_header.take() {
            return Err(err);
        }
        if let Some(ua) = self.user_agent.take() {
            self.default_headers.insert(http::header::USER_AGENT, ua);
        }

        let tls_config = tls::webpki_roots_client_config()?;

        let mut http = HttpConnector::new();
        http.enforce_http(false);

        let https = HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http);

        let mut builder = HyperLegacyClient::builder(TokioExecutor::new());
        if let Some(max_idle) = self.pool_max_idle_per_host {
            builder.pool_max_idle_per_host(max_idle);
        }
        if let Some(idle_timeout) = self.pool_idle_timeout {
            builder.pool_idle_timeout(idle_timeout);
        }
        let hyper = builder.build(https);

        Ok(Client {
            inner: Arc::new(ClientInner {
                hyper,
                default_timeout: self.timeout,
                default_headers: self.default_headers,
                #[cfg(feature = "http3")]
                http3_enabled: self.http3_enabled,
                #[cfg(feature = "http3")]
                http3_negative_cache_duration: self
                    .http3_negative_cache_duration
                    .unwrap_or_else(|| Duration::from_secs(5 * 60)),
                #[cfg(feature = "http3")]
                https_rr_cache: Arc::new(RwLock::new(HashMap::new())),
                #[cfg(feature = "http3")]
                alt_svc_cache: Arc::new(RwLock::new(HashMap::new())),
                #[cfg(feature = "http3")]
                negative_cache: Arc::new(RwLock::new(HashMap::new())),
                #[cfg(feature = "http3")]
                http3: Arc::new(http3::Http3State::new()),
            }),
        })
    }
}
