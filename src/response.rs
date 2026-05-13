//! [`Response`]: status / header inspection + buffered body reading
//! with optional cap. `Content-Encoding: gzip` / `deflate` / `br`
//! responses are transparently decompressed in `bytes` / `text` /
//! `json` paths. `bytes_with_cap` caps **wire bytes** (post-TLS,
//! pre-decompression).

use std::io::Read;

use bytes::{Bytes, BytesMut};
use http::{HeaderMap, StatusCode, Version};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};

/// Successful HTTP response. Construct by calling
/// [`RequestBuilder::send`](crate::RequestBuilder::send) on a
/// [`Client`](crate::Client)-issued request.
#[derive(Debug)]
pub struct Response {
    parts: http::response::Parts,
    body: Option<ResponseBody>,
}

#[derive(Debug)]
enum ResponseBody {
    Hyper(hyper::body::Incoming),
    #[cfg(feature = "http3")]
    Buffered(Bytes),
}

impl Response {
    pub(crate) fn new(response: http::Response<hyper::body::Incoming>) -> Self {
        let (parts, body) = response.into_parts();
        Self {
            parts,
            body: Some(ResponseBody::Hyper(body)),
        }
    }

    #[cfg(feature = "http3")]
    pub(crate) fn new_buffered(response: http::Response<Bytes>) -> Self {
        let (parts, body) = response.into_parts();
        Self {
            parts,
            body: Some(ResponseBody::Buffered(body)),
        }
    }

    /// HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.parts.status
    }

    /// Response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.parts.headers
    }

    /// HTTP protocol version negotiated for this response.
    pub fn version(&self) -> Version {
        self.parts.version
    }

    /// Value of the `Content-Length` response header, if present and
    /// parseable as a `u64`. Useful for pre-flighting body-size caps
    /// before reading the body.
    pub fn content_length(&self) -> Option<u64> {
        self.parts
            .headers
            .get(http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
    }

    /// Read the response body fully (no wire-byte cap), then
    /// transparently decompress per `Content-Encoding`.
    pub async fn bytes(self) -> Result<Bytes> {
        self.bytes_with_cap(usize::MAX).await
    }

    /// Read the response body with a wire-byte cap, then
    /// transparently decompress per `Content-Encoding`. The cap
    /// applies to bytes received from the network — decompressed
    /// payloads may exceed the cap if the server sent a small
    /// compressed body.
    pub async fn bytes_with_cap(mut self, max_wire_bytes: usize) -> Result<Bytes> {
        let body = self
            .body
            .take()
            .ok_or_else(|| Error::Internal("response body already consumed".to_owned()))?;
        let raw = match body {
            ResponseBody::Hyper(body) => collect_body_with_cap(body, max_wire_bytes).await?,
            #[cfg(feature = "http3")]
            ResponseBody::Buffered(body) => {
                if body.len() > max_wire_bytes {
                    return Err(Error::BodyTooLarge {
                        limit: max_wire_bytes,
                        seen: max_wire_bytes,
                    });
                }
                body
            }
        };
        decompress(
            self.parts
                .headers
                .get(http::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            raw,
        )
    }

    /// Read the response body fully, decompress, then interpret as
    /// UTF-8. Errors with [`Error::Decode`] on invalid UTF-8.
    pub async fn text(self) -> Result<String> {
        let bytes = self.bytes().await?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| Error::Decode(format!("response body is not valid UTF-8: {e}")))
    }

    /// Read the response body fully, decompress, then deserialise
    /// as JSON.
    pub async fn json<T: DeserializeOwned>(self) -> Result<T> {
        let bytes = self.bytes().await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Decode(format!("response body is not valid JSON: {e}")))
    }
}

async fn collect_body_with_cap(mut body: hyper::body::Incoming, max_bytes: usize) -> Result<Bytes> {
    let mut buf = BytesMut::new();
    loop {
        match body.frame().await {
            None => break,
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    let new_total = buf.len().saturating_add(data.len());
                    if new_total > max_bytes {
                        return Err(Error::BodyTooLarge {
                            limit: max_bytes,
                            seen: buf.len(),
                        });
                    }
                    buf.extend_from_slice(&data);
                }
                // Trailers / unknown frame kinds: ignore.
            }
            Some(Err(e)) => {
                let message = e.to_string();
                let lower = message.to_lowercase();
                if lower.contains("canceled") || lower.contains("cancelled") {
                    return Err(Error::Cancelled);
                }
                return Err(Error::Unreachable(message));
            }
        }
    }
    Ok(buf.freeze())
}

fn decompress(encoding: Option<&str>, body: Bytes) -> Result<Bytes> {
    let kind = encoding.unwrap_or("identity").trim().to_ascii_lowercase();
    match kind.as_str() {
        "" | "identity" => Ok(body),
        "gzip" | "x-gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(body.as_ref());
            let mut out = Vec::with_capacity(body.len() * 4);
            decoder
                .read_to_end(&mut out)
                .map_err(|e| Error::Decode(format!("gzip decode: {e}")))?;
            Ok(Bytes::from(out))
        }
        "deflate" => {
            let mut decoder = flate2::read::ZlibDecoder::new(body.as_ref());
            let mut out = Vec::with_capacity(body.len() * 4);
            decoder
                .read_to_end(&mut out)
                .map_err(|e| Error::Decode(format!("deflate decode: {e}")))?;
            Ok(Bytes::from(out))
        }
        "br" => {
            let mut decoder = brotli::Decompressor::new(body.as_ref(), 4096);
            let mut out = Vec::with_capacity(body.len() * 4);
            decoder
                .read_to_end(&mut out)
                .map_err(|e| Error::Decode(format!("brotli decode: {e}")))?;
            Ok(Bytes::from(out))
        }
        other => Err(Error::Decode(format!(
            "unsupported Content-Encoding `{other}`"
        ))),
    }
}
