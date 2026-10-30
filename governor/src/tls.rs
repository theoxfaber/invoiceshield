//! Optional TLS for `governor-server` (Phase 4): rustls with the `ring` provider, PEM certificate
//! chain and private key read from files (`GOVERNOR_TLS_CERT_FILE`, `GOVERNOR_TLS_KEY_FILE`).
//! TLS 1.2 and 1.3 only (rustls has nothing older); ALPN h2 + http/1.1.
//!
//! The same axum `Router` is served; each connection gets its peer address as
//! `ConnectInfo<SocketAddr>` so per-IP rate limiting works identically with and without TLS.
use axum::extract::connect_info::ConnectInfo;
use axum::Router;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// Load and check a certificate chain + key. Errors name the file and what was wrong.
pub fn server_config(cert_file: &Path, key_file: &Path) -> Result<rustls::ServerConfig, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_file)
        .map_err(|e| format!("GOVERNOR_TLS_CERT_FILE {}: {e}", cert_file.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("GOVERNOR_TLS_CERT_FILE {}: {e}", cert_file.display()))?;
    if certs.is_empty() {
        return Err(format!("GOVERNOR_TLS_CERT_FILE {}: no PEM certificate found", cert_file.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key_file).map_err(|e| format!("GOVERNOR_TLS_KEY_FILE {}: {e}", key_file.display()))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS: {e}"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS: certificate and key do not form a usable pair: {e}"))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

/// Accept TLS connections forever. A failed handshake drops that connection only.
pub async fn serve(listener: tokio::net::TcpListener, cfg: rustls::ServerConfig, app: Router) -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                eprintln!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let tls = match tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(tcp)).await {
                Ok(Ok(s)) => s,
                _ => return, // handshake failed or timed out (e.g. plain HTTP on the TLS port)
            };
            let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo::<SocketAddr>(peer));
                let mut app = app.clone();
                async move { tower::Service::call(&mut app, req.map(axum::body::Body::new)).await }
            });
            let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(hyper_util::rt::TokioIo::new(tls), svc)
                .await;
        });
    }
}

/// Peer address of a connection, `None` if unknown.
pub fn peer(ext: &axum::http::Extensions) -> Option<SocketAddr> {
    ext.get::<ConnectInfo<SocketAddr>>().map(|c| c.0)
}
