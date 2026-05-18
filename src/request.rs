//! [`RequestBuilder`]: chainable per-request configuration. Mirrors
//! reqwest's surface for the subset of operations the workspace's
//! call sites actually need.

#[cfg(feature = "http3")]
use std::net::IpAddr;
use std::time::Duration;
#[cfg(feature = "http3")]
use std::time::Instant;

use bytes::Bytes;
use http::{HeaderName, HeaderValue, Method, Request, Uri};
use serde::Serialize;

use crate::body::{BoxError, RequestBody, empty_body, full_body, streaming_body};
use crate::client::Client;
#[cfg(feature = "http3")]
use crate::client::Origin;
use crate::error::{Error, Result};
use crate::response::Response;
#[cfg(feature = "http3")]
use crate::{alt_svc, http3, https_rr};

#[cfg(feature = "http3")]
const H3_HTTPS_RR_LOOKUP_TIMEOUT: Duration = Duration::from_millis(150);

/// Chainable per-request builder. Construct via [`Client::get`] /
/// [`Client::post`] / [`Client::request`] / etc.
///
/// Configuration errors (invalid header, invalid URL, JSON
/// serialise failure) are stored on the builder and surfaced at
/// [`Self::send`]; intermediate chain methods never fail directly.
pub struct RequestBuilder {
    client: Client,
    method: Method,
    uri: Result<Uri>,
    headers: http::HeaderMap,
    body: RequestPayload,
    timeout: Option<Duration>,
    deferred_error: Option<Error>,
}

enum RequestPayload {
    Empty,
    Replayable(Bytes),
    Streaming(RequestBody),
}

impl RequestPayload {
    #[cfg(feature = "http3")]
    fn replayable_for_h3(&self) -> Option<Option<Bytes>> {
        match self {
            Self::Empty => Some(None),
            Self::Replayable(bytes) => Some(Some(bytes.clone())),
            Self::Streaming(_) => None,
        }
    }

    fn into_body(self) -> RequestBody {
        match self {
            Self::Empty => empty_body(),
            Self::Replayable(bytes) => full_body(bytes),
            Self::Streaming(body) => body,
        }
    }
}

impl RequestBuilder {
    pub(crate) fn new(client: Client, method: Method, url: &str) -> Self {
        let uri = url
            .parse::<Uri>()
            .map_err(|e| Error::InvalidUrl(format!("could not parse `{url}`: {e}")))
            .and_then(|uri| {
                let scheme = uri.scheme_str().unwrap_or("");
                if scheme == "http" || scheme == "https" {
                    Ok(uri)
                } else {
                    Err(Error::InvalidUrl(format!(
                        "unsupported scheme `{scheme}` in `{url}`"
                    )))
                }
            });

        Self {
            client,
            method,
            uri,
            headers: http::HeaderMap::new(),
            body: RequestPayload::Empty,
            timeout: None,
            deferred_error: None,
        }
    }

    /// Add a header. Invalid name/value pairs defer until
    /// [`Self::send`].
    pub fn header(
        mut self,
        name: impl TryInto<HeaderName>,
        value: impl TryInto<HeaderValue>,
    ) -> Self {
        let Ok(name) = name.try_into() else {
            self.deferred_error
                .get_or_insert_with(|| Error::InvalidHeader("header name rejected".to_owned()));
            return self;
        };
        let Ok(value) = value.try_into() else {
            self.deferred_error.get_or_insert_with(|| {
                Error::InvalidHeader(format!("header `{name}` value rejected"))
            });
            return self;
        };
        self.headers.append(name, value);
        self
    }

    /// Add an `Authorization: Bearer <token>` header.
    pub fn bearer_auth(self, token: impl AsRef<str>) -> Self {
        self.header(
            http::header::AUTHORIZATION,
            format!("Bearer {}", token.as_ref()),
        )
    }

    /// Override the client-default timeout for this single request.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Set the request body to the given bytes.
    pub fn body(mut self, bytes: impl Into<Bytes>) -> Self {
        self.body = RequestPayload::Replayable(bytes.into());
        self
    }

    /// Set the request body to a streaming [`http_body::Body`].
    ///
    /// Streaming bodies are forwarded over the negotiated TCP
    /// transport path. Opportunistic HTTP/3 is reserved for empty or
    /// replayable byte bodies because the client may need to retry on
    /// a fresh connection before falling back to TCP.
    pub fn body_streaming<B>(mut self, body: B) -> Self
    where
        B: http_body::Body<Data = Bytes> + Send + 'static,
        B::Error: Into<BoxError>,
    {
        self.body = RequestPayload::Streaming(streaming_body(body));
        self
    }

    /// Serialise `value` as JSON, set the request body to the
    /// bytes, and set `Content-Type: application/json` unless the
    /// caller already supplied one.
    pub fn json<T: ?Sized + Serialize>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(bytes) => {
                self.body = RequestPayload::Replayable(Bytes::from(bytes));
                if !self.headers.contains_key(http::header::CONTENT_TYPE) {
                    self.headers.insert(
                        http::header::CONTENT_TYPE,
                        HeaderValue::from_static("application/json"),
                    );
                }
            }
            Err(e) => {
                self.deferred_error
                    .get_or_insert_with(|| Error::SerializeJson(e.to_string()));
            }
        }
        self
    }

    /// Send the request, returning a [`Response`].
    pub async fn send(self) -> Result<Response> {
        if let Some(err) = self.deferred_error {
            return Err(err);
        }
        let uri = self.uri?;

        #[cfg(feature = "http3")]
        let uri_for_h3 = uri.clone();
        let mut req = Request::builder().method(self.method.clone()).uri(uri);

        // Default headers from the client, then per-request headers
        // (which can override on a name-by-name basis since the
        // request builder's own insert pass uses append + post-merge
        // semantics).
        if let Some(req_headers) = req.headers_mut() {
            for (name, value) in self.client.inner.default_headers.iter() {
                req_headers.append(name.clone(), value.clone());
            }
            for (name, value) in self.headers.iter() {
                req_headers.append(name.clone(), value.clone());
            }
        }

        #[cfg(feature = "http3")]
        let h3_body = self.body.replayable_for_h3();

        #[cfg(feature = "http3")]
        if self.client.inner.http3_enabled
            && let Some(origin) = Origin::from_uri(&uri_for_h3)
            && let Some(body_bytes) = h3_body
        {
            let timeout = self.timeout.or(self.client.inner.default_timeout);
            let h3_request = request_for_http3(
                self.method.clone(),
                uri_for_h3.clone(),
                req.headers_ref().cloned().unwrap_or_default(),
            )?;
            let h3_attempt = try_http3(&self.client, origin.clone(), h3_request, body_bytes);
            let h3_result = match timeout {
                Some(d) => match tokio::time::timeout(d, h3_attempt).await {
                    Ok(result) => result,
                    Err(_) => {
                        insert_negative(&self.client, origin, Instant::now());
                        return Err(Error::Timeout);
                    }
                },
                None => h3_attempt.await,
            };
            match h3_result {
                Ok(Some(response)) => return Ok(response),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
        }

        let request = req
            .body(self.body.into_body())
            .map_err(|e| Error::Internal(format!("request build: {e}")))?;

        let timeout = self.timeout.or(self.client.inner.default_timeout);
        let send_fut = self.client.inner.hyper.request(request);

        let response = match timeout {
            Some(d) => match tokio::time::timeout(d, send_fut).await {
                Ok(result) => result,
                Err(_) => return Err(Error::Timeout),
            },
            None => send_fut.await,
        }
        .map_err(map_legacy_error)?;

        let response = Response::new(response);
        #[cfg(feature = "http3")]
        maybe_update_alt_svc(&self.client, &uri_for_h3, &response);
        Ok(response)
    }
}

#[cfg(feature = "http3")]
impl Origin {
    pub(crate) fn from_uri(uri: &Uri) -> Option<Self> {
        if uri.scheme_str()? != "https" {
            return None;
        }
        let host = uri.host()?.to_owned();
        let port = uri.port_u16().unwrap_or(443);
        Some(Self { host, port })
    }
}

#[cfg(feature = "http3")]
fn request_for_http3(method: Method, uri: Uri, headers: http::HeaderMap) -> Result<Request<()>> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(out) = builder.headers_mut() {
        *out = headers;
    }
    builder
        .body(())
        .map_err(|e| Error::Internal(format!("HTTP/3 request build: {e}")))
}

#[cfg(feature = "http3")]
async fn try_http3(
    client: &Client,
    origin: Origin,
    request: Request<()>,
    body: Option<Bytes>,
) -> Result<Option<Response>> {
    let now = Instant::now();
    if negative_cache_hit(client, &origin, now) {
        return Ok(None);
    }

    if let Some(entry) = alt_svc_entry(client, &origin, now) {
        let authority_host = entry.host.as_deref().unwrap_or(origin.host.as_str());
        let target = Origin {
            host: authority_host.to_owned(),
            port: entry.port,
        };
        let target = Http3RequestTarget {
            origin: &origin,
            target,
            authority_host,
            addresses: &[],
        };
        if let Some(response) =
            request_http3_with_stale_retry(client, target, request.clone(), body.clone(), now)
                .await?
        {
            return Ok(Some(response));
        }
    }

    if let Some(entry) = https_rr_entry(client, &origin, now).await
        && entry.has_h3
    {
        let mut target = origin.clone();
        target.port = entry.port;
        let target = Http3RequestTarget {
            origin: &origin,
            target,
            authority_host: &origin.host,
            addresses: &entry.addresses,
        };
        if let Some(response) =
            request_http3_with_stale_retry(client, target, request, body, now).await?
        {
            return Ok(Some(response));
        }
    }

    Ok(None)
}

#[cfg(feature = "http3")]
struct Http3RequestTarget<'a> {
    origin: &'a Origin,
    target: Origin,
    authority_host: &'a str,
    addresses: &'a [IpAddr],
}

#[cfg(feature = "http3")]
async fn request_http3_with_stale_retry(
    client: &Client,
    target: Http3RequestTarget<'_>,
    request: Request<()>,
    body: Option<Bytes>,
    now: Instant,
) -> Result<Option<Response>> {
    for attempt in 0..2 {
        let mut cancellation_guard = H3AttemptCancellationGuard::new(client, target.origin);
        let result = client
            .inner
            .http3
            .request(
                &client.inner.dns,
                target.target.clone(),
                target.authority_host,
                target.addresses,
                request.clone(),
                body.clone(),
            )
            .await;
        cancellation_guard.disarm();

        match result {
            Ok(response) => return Ok(Some(response)),
            Err(http3::Http3AttemptError::Handshake(message)) => {
                let _ = message;
                insert_negative(client, target.origin.clone(), now);
                return Ok(None);
            }
            Err(http3::Http3AttemptError::Stream {
                retry_without_h3, ..
            }) => match h3_stream_failure_action(retry_without_h3, attempt) {
                H3StreamFailureAction::RetryFreshH3 => continue,
                H3StreamFailureAction::UseTcp => {
                    insert_negative(client, target.origin.clone(), now);
                    return Ok(None);
                }
            },
        }
    }

    insert_negative(client, target.origin.clone(), now);
    Ok(None)
}

#[cfg(feature = "http3")]
#[derive(Debug, Eq, PartialEq)]
enum H3StreamFailureAction {
    RetryFreshH3,
    UseTcp,
}

#[cfg(feature = "http3")]
fn h3_stream_failure_action(retry_without_h3: bool, attempt: usize) -> H3StreamFailureAction {
    if retry_without_h3 && attempt == 0 {
        H3StreamFailureAction::RetryFreshH3
    } else {
        H3StreamFailureAction::UseTcp
    }
}

#[cfg(feature = "http3")]
struct H3AttemptCancellationGuard<'a> {
    client: &'a Client,
    origin: &'a Origin,
    armed: bool,
}

#[cfg(feature = "http3")]
impl<'a> H3AttemptCancellationGuard<'a> {
    fn new(client: &'a Client, origin: &'a Origin) -> Self {
        Self {
            client,
            origin,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

#[cfg(feature = "http3")]
impl Drop for H3AttemptCancellationGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            insert_negative(self.client, self.origin.clone(), Instant::now());
        }
    }
}

#[cfg(feature = "http3")]
async fn https_rr_entry(
    client: &Client,
    origin: &Origin,
    now: Instant,
) -> Option<https_rr::HttpsRrEntry> {
    if let Ok(mut cache) = client.inner.https_rr_cache.write() {
        if let Some(entry) = cache.get(origin)
            && https_rr::fresh(entry, now)
        {
            return Some(entry.clone());
        }
        cache.remove(origin);
    }

    let lookup = tokio::time::timeout(
        H3_HTTPS_RR_LOOKUP_TIMEOUT,
        https_rr::lookup(&client.inner.dns, origin),
    )
    .await;
    match lookup {
        Err(_) => None,
        Ok(result) => match result {
            Ok(Some(entry)) => {
                if let Ok(mut cache) = client.inner.https_rr_cache.write() {
                    cache.insert(origin.clone(), entry.clone());
                }
                Some(entry)
            }
            Ok(None) | Err(_) => None,
        },
    }
}

#[cfg(feature = "http3")]
fn alt_svc_entry(client: &Client, origin: &Origin, now: Instant) -> Option<alt_svc::AltSvcEntry> {
    let Ok(mut cache) = client.inner.alt_svc_cache.write() else {
        return None;
    };
    if let Some(entry) = cache.get(origin)
        && alt_svc::fresh(entry, now)
    {
        return Some(entry.clone());
    }
    cache.remove(origin);
    None
}

#[cfg(feature = "http3")]
fn negative_cache_hit(client: &Client, origin: &Origin, now: Instant) -> bool {
    let Ok(mut cache) = client.inner.negative_cache.write() else {
        return false;
    };
    if let Some(expires_at) = cache.get(origin).copied()
        && expires_at > now
    {
        return true;
    }
    cache.remove(origin);
    false
}

#[cfg(feature = "http3")]
fn insert_negative(client: &Client, origin: Origin, now: Instant) {
    if let Ok(mut cache) = client.inner.negative_cache.write() {
        cache.insert(
            origin.clone(),
            now + client.inner.http3_negative_cache_duration,
        );
    }
    if let Ok(mut cache) = client.inner.alt_svc_cache.write() {
        cache.remove(&origin);
    }
}

#[cfg(feature = "http3")]
fn maybe_update_alt_svc(client: &Client, uri: &Uri, response: &Response) {
    let Some(origin) = Origin::from_uri(uri) else {
        return;
    };
    let Some(value) = response.headers().get(http::header::ALT_SVC) else {
        return;
    };
    let update = alt_svc::parse_header(value, Instant::now(), origin.port);
    if let Ok(mut cache) = client.inner.alt_svc_cache.write() {
        match update {
            alt_svc::AltSvcUpdate::Clear => {
                cache.remove(&origin);
            }
            alt_svc::AltSvcUpdate::Entry(entry) => {
                cache.insert(origin, entry);
            }
            alt_svc::AltSvcUpdate::None => {}
        }
    }
}

fn map_legacy_error(err: hyper_util::client::legacy::Error) -> Error {
    let message = err.to_string();
    // hyper-util's legacy::Error doesn't expose a clean kind enum;
    // heuristic-classify based on the message. Less precise than a
    // structured downcast, but the workspace's call sites only
    // discriminate timeout / unreachable / other, which match well.
    if message.contains("connection") || message.contains("connect") || message.contains("dns") {
        Error::Unreachable(message)
    } else if message.contains("certificate") || message.contains("tls") || message.contains("TLS")
    {
        Error::Tls(message)
    } else {
        Error::Internal(message)
    }
}

#[cfg(all(feature = "http3", test))]
mod tests {
    use super::*;

    #[test]
    fn h3_attempt_cancellation_guard_negative_caches_on_drop() {
        let client = Client::builder().build().expect("client");
        let origin = Origin {
            host: "example.com".to_owned(),
            port: 443,
        };
        client.insert_alt_svc_for_test(
            origin.clone(),
            alt_svc::AltSvcEntry {
                host: None,
                port: 443,
                expires_at: Instant::now() + Duration::from_secs(60),
            },
        );
        assert!(client.has_alt_svc_for_test(&origin));

        {
            let _guard = H3AttemptCancellationGuard::new(&client, &origin);
        }

        assert!(client.has_negative_for_test(&origin));
        assert!(!client.has_alt_svc_for_test(&origin));
    }

    #[test]
    fn h3_attempt_cancellation_guard_disarms_after_completion() {
        let client = Client::builder().build().expect("client");
        let origin = Origin {
            host: "example.com".to_owned(),
            port: 443,
        };

        {
            let mut guard = H3AttemptCancellationGuard::new(&client, &origin);
            guard.disarm();
        }

        assert!(!client.has_negative_for_test(&origin));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn h3_missing_udp_listener_falls_back_before_request_timeout() {
        let blackhole = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("bind local UDP blackhole");
        let port = blackhole.local_addr().expect("local addr").port();
        let client = Client::builder().build().expect("client");
        let origin = Origin {
            host: "localhost".to_owned(),
            port,
        };
        let target_origin = origin.clone();
        let uri = format!("https://localhost:{port}/")
            .parse()
            .expect("test URI");
        let request =
            request_for_http3(Method::GET, uri, http::HeaderMap::new()).expect("h3 request");
        let addresses = [std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)];
        let target = Http3RequestTarget {
            origin: &origin,
            target: target_origin,
            authority_host: "localhost",
            addresses: &addresses,
        };

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            request_http3_with_stale_retry(&client, target, request, None, Instant::now()),
        )
        .await
        .expect("missing H3 service must not inherit endpoint timeout")
        .expect("H3 fallback result");

        assert!(result.is_none());
        assert!(client.has_negative_for_test(&origin));
        drop(blackhole);
    }

    #[test]
    fn h3_stream_failure_after_retry_budget_uses_tcp_fallback() {
        assert_eq!(
            h3_stream_failure_action(true, 0),
            H3StreamFailureAction::RetryFreshH3
        );
        assert_eq!(
            h3_stream_failure_action(true, 1),
            H3StreamFailureAction::UseTcp
        );
    }

    #[test]
    fn h3_stream_failure_after_request_started_uses_tcp_fallback() {
        assert_eq!(
            h3_stream_failure_action(false, 0),
            H3StreamFailureAction::UseTcp
        );
    }
}
