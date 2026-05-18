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
use mechanics_dns::Resolver;
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
    Stream { retry_without_h3: bool },
}

impl Http3State {
    pub(crate) fn new() -> Self {
        Self {
            endpoint: Mutex::new(None),
        }
    }

    pub(crate) async fn request(
        &self,
        resolver: &Resolver,
        origin: Origin,
        authority_host: &str,
        addresses: &[IpAddr],
        request: Request<()>,
        body: Option<Bytes>,
    ) -> std::result::Result<Response, Http3AttemptError> {
        let mut sender = match self
            .connection(resolver, authority_host, origin.port, addresses)
            .await
        {
            Ok(sender) => sender,
            Err(err) => return Err(Http3AttemptError::Handshake(err)),
        };

        let mut stream = tokio::time::timeout(H3_STREAM_OPEN_TIMEOUT, sender.send_request(request))
            .await
            .map_err(|_| Http3AttemptError::Stream {
                retry_without_h3: true,
            })?
            .map_err(|_| Http3AttemptError::Stream {
                retry_without_h3: true,
            })?;

        if let Some(body) = body {
            h3_stream_phase(H3_STREAM_UPLOAD_TIMEOUT, stream.send_data(body)).await?;
        }
        h3_stream_phase(H3_STREAM_UPLOAD_TIMEOUT, stream.finish()).await?;

        let response = stream
            .recv_response()
            .await
            .map_err(|_| stream_error_after_request_started())?;
        let (parts, ()) = response.into_parts();
        Ok(Response::new_h3(
            HttpResponse::from_parts(parts, ()),
            H3ResponseBody::new(stream, sender),
        ))
    }

    async fn connection(
        &self,
        resolver: &Resolver,
        authority_host: &str,
        port: u16,
        addresses: &[IpAddr],
    ) -> std::result::Result<H3SendRequest, String> {
        let endpoint = self.endpoint()?;
        let addr = first_socket_addr(resolver, authority_host, port, addresses).await?;
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
    _sender: H3SendRequest,
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
    fn new(stream: H3RequestStream, sender: H3SendRequest) -> Self {
        Self {
            state: H3ResponseBodyState::Ready(Some(Box::new(stream))),
            _sender: sender,
        }
    }
}

impl Drop for H3ResponseBody {
    fn drop(&mut self) {
        if let H3ResponseBodyState::Ready(Some(stream)) = &mut self.state {
            cancel_request_stream(stream);
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

fn cancel_request_stream(stream: &mut H3RequestStream) {
    stream.stop_sending(h3::error::Code::H3_REQUEST_CANCELLED);
    stream.stop_stream(h3::error::Code::H3_REQUEST_CANCELLED);
}

async fn first_socket_addr(
    resolver: &Resolver,
    host: &str,
    port: u16,
    addresses: &[IpAddr],
) -> std::result::Result<SocketAddr, String> {
    if let Some(addr) = addresses.first().copied() {
        return Ok(SocketAddr::new(addr, port));
    }

    let addrs = tokio::time::timeout(
        H3_DNS_LOOKUP_TIMEOUT,
        resolver.lookup_socket_addrs(host, port),
    )
    .await
    .map_err(|_| format!("HTTP/3 DNS lookup timed out after {H3_DNS_LOOKUP_TIMEOUT:?}"))?
    .map_err(|e| e.to_string())?;
    addrs
        .into_iter()
        .next()
        .ok_or_else(|| format!("no socket addresses for {host}:{port}"))
}

async fn h3_stream_phase<T, E>(
    timeout: Duration,
    future: impl Future<Output = std::result::Result<T, E>>,
) -> std::result::Result<T, Http3AttemptError>
where
    E: std::fmt::Display,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| stream_error_after_request_started())?
        .map_err(|_| stream_error_after_request_started())
}

fn stream_error_after_request_started() -> Http3AttemptError {
    Http3AttemptError::Stream {
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

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use bytes::Buf;
    use http::{Method, Request, Response as HttpResponse, StatusCode};
    use http_body_util::BodyExt;
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn h3_response_body_keeps_connection_owner_until_body_drop() {
        let fixture = H3Fixture::start().await;
        let mut send_request = connect_h3_client(fixture.addr, fixture.cert.clone()).await;
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://localhost/test")
            .body(())
            .unwrap();
        let mut stream = send_request.send_request(request).await.unwrap();
        stream.finish().await.unwrap();

        let response = stream.recv_response().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let mut body = H3ResponseBody::new(stream, send_request);
        let mut out = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Ok(mut data) = frame.into_data() {
                let remaining = data.remaining();
                out.extend_from_slice(&data.copy_to_bytes(remaining));
            }
        }

        assert_eq!(out, b"hello from h3");
        drop(body);
        fixture.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_h3_send_request_before_body_reproduces_client_close() {
        let fixture = H3Fixture::start().await;
        let mut send_request = connect_h3_client(fixture.addr, fixture.cert.clone()).await;
        let request = Request::builder()
            .method(Method::GET)
            .uri("https://localhost/test")
            .body(())
            .unwrap();
        let mut stream = send_request.send_request(request).await.unwrap();
        stream.finish().await.unwrap();

        let response = stream.recv_response().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        drop(send_request);
        let err = match stream.recv_data().await {
            Err(err) => err,
            Ok(_) => panic!("expected H3 body read to fail after dropping SendRequest"),
        };
        let message = err.to_string();
        assert!(
            message.contains("Connection closed by client") || message.contains("H3_NO_ERROR"),
            "unexpected H3 error after dropping SendRequest: {message}"
        );

        fixture.shutdown().await;
    }

    struct H3Fixture {
        endpoint: quinn::Endpoint,
        addr: SocketAddr,
        cert: CertificateDer<'static>,
        task: tokio::task::JoinHandle<()>,
    }

    impl H3Fixture {
        async fn start() -> Self {
            let (cert_chain, key) = test_tls_material();
            let cert = cert_chain[0].clone();
            let endpoint = quinn::Endpoint::server(
                test_server_config(cert_chain, key),
                SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            )
            .unwrap();
            let addr = endpoint.local_addr().unwrap();
            let task_endpoint = endpoint.clone();
            let task = tokio::spawn(async move {
                serve_single_delayed_body(task_endpoint).await;
            });

            Self {
                endpoint,
                addr,
                cert,
                task,
            }
        }

        async fn shutdown(self) {
            self.endpoint.close(0_u32.into(), b"test shutdown");
            let _ = tokio::time::timeout(Duration::from_secs(1), self.task).await;
        }
    }

    async fn serve_single_delayed_body(endpoint: quinn::Endpoint) {
        let incoming = endpoint.accept().await.unwrap();
        let connecting = incoming.accept().unwrap();
        let connection = connecting.await.unwrap();
        let quic = h3_quinn::Connection::new(connection);
        let mut h3_conn = h3::server::Connection::new(quic).await.unwrap();
        let resolver = h3_conn.accept().await.unwrap().unwrap();
        let (_request, mut stream) = resolver.resolve_request().await.unwrap();

        stream
            .send_response(HttpResponse::builder().status(200).body(()).unwrap())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        if stream
            .send_data(Bytes::from_static(b"hello from h3"))
            .await
            .is_ok()
        {
            let _ = stream.finish().await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(1), h3_conn.accept()).await;
    }

    async fn connect_h3_client(addr: SocketAddr, cert: CertificateDer<'static>) -> H3SendRequest {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut tls_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls_config.alpn_protocols = vec![b"h3".to_vec()];
        let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(tls_config).unwrap();
        let client_config = quinn::ClientConfig::new(Arc::new(quic_config));
        let mut endpoint =
            quinn::Endpoint::client(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).unwrap();
        endpoint.set_default_client_config(client_config);
        let connection = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        let quic = h3_quinn::Connection::new(connection);
        let (mut driver, send_request) = h3::client::builder().build(quic).await.unwrap();
        tokio::spawn(async move {
            let _ = driver.wait_idle().await;
            drop(endpoint);
        });
        send_request
    }

    fn test_tls_material() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
        let CertifiedKey { cert, key_pair } =
            generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert_chain = vec![cert.der().clone()];
        let key = PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
        (cert_chain, key)
    }

    fn test_server_config(
        cert_chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    ) -> quinn::ServerConfig {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let mut tls_config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(cert_chain, key)
            .unwrap();
        tls_config.alpn_protocols = vec![b"h3".to_vec()];
        let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(tls_config).unwrap();
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
        server_config.transport_config(Arc::new(h3_transport_config().unwrap()));
        server_config
    }
}
