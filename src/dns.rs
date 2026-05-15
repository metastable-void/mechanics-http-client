//! DNS adapter between `mechanics-dns` and hyper-util.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper_util::client::legacy::connect::dns::Name;
use tower_service::Service;

#[derive(Clone)]
pub(crate) struct HyperDnsResolver {
    resolver: mechanics_dns::Resolver,
}

impl HyperDnsResolver {
    pub(crate) fn new(resolver: mechanics_dns::Resolver) -> Self {
        Self { resolver }
    }
}

impl std::fmt::Debug for HyperDnsResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HyperDnsResolver").finish_non_exhaustive()
    }
}

impl Service<Name> for HyperDnsResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Self::Response>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let resolver = self.resolver.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = resolver
                .lookup_socket_addrs(&host, 0)
                .await
                .map_err(io::Error::other)?;
            if addrs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no DNS addresses for `{host}`"),
                ));
            }
            Ok(addrs.into_iter())
        })
    }
}
