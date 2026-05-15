//! Opportunistic HTTP/3 over QUIC.

use std::future::Future;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::{Buf, Bytes};
use http::{Request, Response as HttpResponse};
use http_body::Frame;
use quinn::crypto::rustls::QuicClientConfig;

use crate::client::Origin;
use crate::error::Error;
use crate::response::Response;
use crate::tls;

type H3SendRequest = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;
type H3RequestStream = h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;
type RecvDataFuture =
    Pin<Box<dyn Future<Output = (Box<H3RequestStream>, Result<Option<Bytes>, Error>)> + Send>>;

const H3_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(15);
const H3_MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const H3_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const H3_DNS_LOOKUP_TIMEOUT: Duration = Duration::from_millis(150);
const H3_STREAM_OPEN_TIMEOUT: Duration = Duration::from_millis(150);
const H3_STREAM_UPLOAD_TIMEOUT: Duration = Duration::from_millis(500);

/// HTTP/3 endpoint state owned by a [`Client`](crate::Client).
pub(crate) struct Http3State {
    endpoint: Mutex<Option<quinn::Endpoint>>,
}

pub(crate) enum Http3AttemptError {
    Handshake(String),
    Stream {
        error: Error,
        retry_without_h3: bool,
    },
}

impl Http3State {
    pub(crate) fn new() -> Self {
        Self {
            endpoint: Mutex::new(None),
        }
    }

    pub(crate) async fn request(
        &self,
        origin: Origin,
        authority_host: &str,
        addresses: &[IpAddr],
        request: Request<()>,
        body: Option<Bytes>,
    ) -> std::result::Result<Response, Http3AttemptError> {
        let mut sender = match self
            .connection(authority_host, origin.port, addresses)
            .await
        {
            Ok(sender) => sender,
            Err(err) => return Err(Http3AttemptError::Handshake(err)),
        };

        let mut stream = tokio::time::timeout(H3_STREAM_OPEN_TIMEOUT, sender.send_request(request))
            .await
            .map_err(|_| Http3AttemptError::Stream {
                error: Error::Cancelled(format!(
                    "HTTP/3 request stream open timed out after {H3_STREAM_OPEN_TIMEOUT:?}"
                )),
                retry_without_h3: true,
            })?
            .map_err(|e| Http3AttemptError::Stream {
                error: Error::Cancelled(e.to_string()),
                retry_without_h3: true,
            })?;

        if let Some(body) = body {
            h3_stream_phase(
                H3_STREAM_UPLOAD_TIMEOUT,
                "request body send",
                stream.send_data(body),
            )
            .await?;
        }
        h3_stream_phase(H3_STREAM_UPLOAD_TIMEOUT, "request finish", stream.finish()).await?;

        let response = stream
            .recv_response()
            .await
            .map_err(|e| stream_error_after_request_started(Error::Cancelled(e.to_string())))?;
        let (parts, ()) = response.into_parts();
        Ok(Response::new_h3(
            HttpResponse::from_parts(parts, ()),
            H3ResponseBody::new(stream),
        ))
    }

    async fn connection(
        &self,
        authority_host: &str,
        port: u16,
        addresses: &[IpAddr],
    ) -> std::result::Result<H3SendRequest, String> {
        let endpoint = self.endpoint()?;
        let addr = first_socket_addr(authority_host, port, addresses).await?;
        let connecting = endpoint
            .connect(addr, authority_host)
            .map_err(|e| e.to_string())?;
        let connection = tokio::time::timeout(H3_CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| format!("HTTP/3 connect timed out after {H3_CONNECT_TIMEOUT:?}"))?
            .map_err(|e| e.to_string())?;
        let quic = h3_quinn::Connection::new(connection);
        let (mut driver, send_request) =
            tokio::time::timeout(H3_CONNECT_TIMEOUT, h3::client::builder().build(quic))
                .await
                .map_err(|_| format!("HTTP/3 setup timed out after {H3_CONNECT_TIMEOUT:?}"))?
                .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            let _ = driver.wait_idle().await;
        });

        Ok(send_request)
    }

    fn endpoint(&self) -> std::result::Result<quinn::Endpoint, String> {
        let mut guard = self
            .endpoint
            .lock()
            .map_err(|_| "HTTP/3 endpoint lock poisoned".to_owned())?;
        if let Some(endpoint) = guard.as_ref().cloned() {
            return Ok(endpoint);
        }

        let mut tls_config = tls::webpki_roots_client_config().map_err(|e| e.to_string())?;
        tls_config.alpn_protocols = vec![b"h3".to_vec()];
        let quic_config = QuicClientConfig::try_from(tls_config).map_err(|e| e.to_string())?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(quic_config));
        client_config.transport_config(Arc::new(h3_transport_config()?));
        let bind = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let mut endpoint = quinn::Endpoint::client(bind).map_err(|e| e.to_string())?;
        endpoint.set_default_client_config(client_config);
        *guard = Some(endpoint.clone());
        Ok(endpoint)
    }
}

/// Streaming HTTP/3 response body backed by an h3 client request stream.
pub(crate) struct H3ResponseBody {
    state: H3ResponseBodyState,
}

enum H3ResponseBodyState {
    Ready(Option<Box<H3RequestStream>>),
    Reading(RecvDataFuture),
    Done,
}

impl std::fmt::Debug for H3ResponseBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H3ResponseBody").finish_non_exhaustive()
    }
}

impl H3ResponseBody {
    fn new(stream: H3RequestStream) -> Self {
        Self {
            state: H3ResponseBodyState::Ready(Some(Box::new(stream))),
        }
    }
}

impl http_body::Body for H3ResponseBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        loop {
            match &mut this.state {
                H3ResponseBodyState::Ready(stream) => {
                    let Some(mut stream) = stream.take() else {
                        this.state = H3ResponseBodyState::Done;
                        continue;
                    };
                    this.state = H3ResponseBodyState::Reading(Box::pin(async move {
                        let result = stream
                            .recv_data()
                            .await
                            .map_err(|e| Error::Cancelled(e.to_string()))
                            .map(|chunk| {
                                chunk.map(|mut chunk| {
                                    let remaining = chunk.remaining();
                                    chunk.copy_to_bytes(remaining)
                                })
                            });
                        (stream, result)
                    }));
                }
                H3ResponseBodyState::Reading(future) => {
                    let (stream, result) = std::task::ready!(future.as_mut().poll(cx));
                    match result {
                        Ok(Some(bytes)) => {
                            this.state = H3ResponseBodyState::Ready(Some(stream));
                            return Poll::Ready(Some(Ok(Frame::data(bytes))));
                        }
                        Ok(None) => {
                            this.state = H3ResponseBodyState::Done;
                            return Poll::Ready(None);
                        }
                        Err(error) => {
                            this.state = H3ResponseBodyState::Done;
                            return Poll::Ready(Some(Err(error)));
                        }
                    }
                }
                H3ResponseBodyState::Done => return Poll::Ready(None),
            }
        }
    }
}

async fn first_socket_addr(
    host: &str,
    port: u16,
    addresses: &[IpAddr],
) -> std::result::Result<SocketAddr, String> {
    if let Some(addr) = addresses.first().copied() {
        return Ok(SocketAddr::new(addr, port));
    }

    let mut addrs =
        tokio::time::timeout(H3_DNS_LOOKUP_TIMEOUT, tokio::net::lookup_host((host, port)))
            .await
            .map_err(|_| format!("HTTP/3 DNS lookup timed out after {H3_DNS_LOOKUP_TIMEOUT:?}"))?
            .map_err(|e| e.to_string())?;
    addrs
        .next()
        .ok_or_else(|| format!("no socket addresses for {host}:{port}"))
}

async fn h3_stream_phase<T, E>(
    timeout: Duration,
    phase: &'static str,
    future: impl Future<Output = std::result::Result<T, E>>,
) -> std::result::Result<T, Http3AttemptError>
where
    E: std::fmt::Display,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| stream_error_after_request_started(Error::Timeout))?
        .map_err(|e| {
            stream_error_after_request_started(Error::Cancelled(format!(
                "HTTP/3 {phase} failed: {e}"
            )))
        })
}

fn stream_error_after_request_started(error: Error) -> Http3AttemptError {
    Http3AttemptError::Stream {
        error,
        retry_without_h3: false,
    }
}

fn h3_transport_config() -> std::result::Result<quinn::TransportConfig, String> {
    let mut transport = quinn::TransportConfig::default();
    transport
        .keep_alive_interval(Some(H3_KEEP_ALIVE_INTERVAL))
        .max_idle_timeout(Some(
            H3_MAX_IDLE_TIMEOUT.try_into().map_err(|e| format!("{e}"))?,
        ));
    Ok(transport)
}
