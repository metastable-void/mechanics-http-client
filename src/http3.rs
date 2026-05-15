//! Opportunistic HTTP/3 over QUIC.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use bytes::{Buf, Bytes, BytesMut};
use http::{Request, Response as HttpResponse};
use quinn::crypto::rustls::QuicClientConfig;
use tokio::sync::Mutex as AsyncMutex;

use crate::client::Origin;
use crate::error::Error;
use crate::response::Response;
use crate::tls;

type H3SendRequest = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// Cached HTTP/3 transport state owned by a [`Client`](crate::Client).
pub(crate) struct Http3State {
    endpoint: Mutex<Option<quinn::Endpoint>>,
    connections: Mutex<HashMap<Origin, Arc<AsyncMutex<H3SendRequest>>>>,
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
            connections: Mutex::new(HashMap::new()),
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
        let sender = match self
            .connection(origin.clone(), authority_host, addresses)
            .await
        {
            Ok(sender) => sender,
            Err(err) => return Err(Http3AttemptError::Handshake(err)),
        };

        let mut sender = sender.lock().await;
        let mut stream =
            sender
                .send_request(request)
                .await
                .map_err(|e| Http3AttemptError::Stream {
                    error: Error::Cancelled(e.to_string()),
                    retry_without_h3: true,
                })?;

        if let Some(body) = body {
            stream
                .send_data(body)
                .await
                .map_err(|e| stream_error_after_request_started(Error::Cancelled(e.to_string())))?;
        }
        stream
            .finish()
            .await
            .map_err(|e| stream_error_after_request_started(Error::Cancelled(e.to_string())))?;

        let response = stream
            .recv_response()
            .await
            .map_err(|e| stream_error_after_request_started(Error::Cancelled(e.to_string())))?;
        let (parts, ()) = response.into_parts();

        let mut body = BytesMut::new();
        while let Some(mut chunk) = stream
            .recv_data()
            .await
            .map_err(|e| stream_error_after_request_started(Error::Cancelled(e.to_string())))?
        {
            let remaining = chunk.remaining();
            body.extend_from_slice(&chunk.copy_to_bytes(remaining));
        }

        Ok(Response::new_buffered(HttpResponse::from_parts(
            parts,
            body.freeze(),
        )))
    }

    async fn connection(
        &self,
        origin: Origin,
        authority_host: &str,
        addresses: &[IpAddr],
    ) -> std::result::Result<Arc<AsyncMutex<H3SendRequest>>, String> {
        if let Ok(guard) = self.connections.lock()
            && let Some(connection) = guard.get(&origin).cloned()
        {
            return Ok(connection);
        }

        let endpoint = self.endpoint()?;
        let addr = first_socket_addr(authority_host, origin.port, addresses).await?;
        let connecting = endpoint
            .connect(addr, authority_host)
            .map_err(|e| e.to_string())?;
        let connection = connecting.await.map_err(|e| e.to_string())?;
        let quic = h3_quinn::Connection::new(connection);
        let (mut driver, send_request) = h3::client::builder()
            .build(quic)
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            let _ = driver.wait_idle().await;
        });

        let send_request = Arc::new(AsyncMutex::new(send_request));
        if let Ok(mut guard) = self.connections.lock() {
            guard.insert(origin, send_request.clone());
        }
        Ok(send_request)
    }

    pub(crate) fn remove_connection(&self, origin: &Origin) {
        if let Ok(mut guard) = self.connections.lock() {
            guard.remove(origin);
        }
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
        let client_config = quinn::ClientConfig::new(Arc::new(quic_config));
        let bind = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let mut endpoint = quinn::Endpoint::client(bind).map_err(|e| e.to_string())?;
        endpoint.set_default_client_config(client_config);
        *guard = Some(endpoint.clone());
        Ok(endpoint)
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

    let mut addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| e.to_string())?;
    addrs
        .next()
        .ok_or_else(|| format!("no socket addresses for {host}:{port}"))
}

fn stream_error_after_request_started(error: Error) -> Http3AttemptError {
    Http3AttemptError::Stream {
        error,
        retry_without_h3: false,
    }
}
