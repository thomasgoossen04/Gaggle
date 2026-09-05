//! A minimal async HTTP(S) client shared by [`crate::admin::AdminClient`] and
//! [`crate::rendezvous::RendezvousClient`] — the two clients that need a
//! fully custom `rustls::ClientConfig` (see [`crate::tls`]).
//!
//! `reqwest`'s documented way to supply one (`ClientBuilder::tls_backend_preconfigured`,
//! née `use_preconfigured_tls`) downcasts the config through `dyn Any` and,
//! at least as of `reqwest` 0.13.4, that downcast fails at runtime
//! ("Unknown TLS backend") even with a single, version-matched `rustls` in
//! the dependency graph — reproduced standalone outside this workspace, and a
//! widely reported issue upstream, not something specific here.
//! `hyper-rustls`'s `HttpsConnectorBuilder::with_tls_config` is the
//! actually-supported way to do this, so this client is built directly on
//! `hyper`/`hyper-util` instead. `invite.rs`'s client has no custom-TLS need
//! and stays on plain `reqwest`.

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;

pub(crate) struct HttpClient {
    inner: Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl HttpClient {
    /// `tls` governs `https://` connections. `http://` still works
    /// unencrypted (some tests spin up a bare `axum::serve` with no TLS at
    /// all to exercise the signed-request scheme in isolation) — exactly what
    /// a default `reqwest::Client` would have done.
    pub(crate) fn new(tls: rustls::ClientConfig) -> Self {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .wrap_connector(http);
        Self { inner: Client::builder(TokioExecutor::new()).build(connector) }
    }

    /// Send one request and buffer the whole response body.
    pub(crate) async fn send(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> anyhow::Result<(StatusCode, HeaderMap, Bytes)> {
        let mut builder = http::Request::builder().method(method).uri(url);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let req = builder.body(Full::new(Bytes::from(body)))?;
        let resp = self.inner.request(req).await?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp.into_body().collect().await?.to_bytes();
        Ok((status, headers, body))
    }
}
