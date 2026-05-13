//! [`RequestBuilder`]: chainable per-request configuration. Mirrors
//! reqwest's surface for the subset of operations the workspace's
//! call sites actually need.

use std::time::Duration;

use bytes::Bytes;
use http::{HeaderName, HeaderValue, Method, Request, Uri};
use serde::Serialize;

use crate::body::{RequestBody, empty_body, full_body};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::response::Response;

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
    body: Option<Bytes>,
    timeout: Option<Duration>,
    deferred_error: Option<Error>,
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
            body: None,
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
        self.body = Some(bytes.into());
        self
    }

    /// Serialise `value` as JSON, set the request body to the
    /// bytes, and set `Content-Type: application/json` unless the
    /// caller already supplied one.
    pub fn json<T: ?Sized + Serialize>(mut self, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(bytes) => {
                self.body = Some(Bytes::from(bytes));
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

        let body: RequestBody = match self.body {
            None => empty_body(),
            Some(bytes) => full_body(bytes),
        };

        let request = req
            .body(body)
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

        Ok(Response::new(response))
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
