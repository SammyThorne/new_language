//! TLS for the HTTP server, the HTTP client and WebSockets (docs/06, milestone 12), using rustls
//! with the `ring` provider.
//!
//! A TLS connection is the same non-blocking socket as a plain one with a rustls session beside
//! it. The session sits behind a short-held mutex, because a WebSocket reads in one task while
//! another writes; the lock is never held while a task waits for the socket, so a reader parked
//! for the next frame does not hold up a writer.
//!
//! Client connections trust the Mozilla roots built into the program (`webpki-roots`), plus the
//! certificates in the file named by `SSL_CERT_FILE` when it is set, which is how a test or a
//! private network adds its own authority.

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{ClientConfig, ClientConnection, Connection, RootCertStore, ServerConfig, ServerConnection};
use std::sync::{Arc, Mutex, OnceLock};

/// A server configuration from a PEM certificate chain and a PEM private key.
pub(crate) fn server_config(cert_file: &str, key_file: &str) -> Result<Arc<ServerConfig>, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_file)
        .map_err(|e| format!("cannot read the certificate file `{cert_file}`: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cannot read the certificate file `{cert_file}`: {e}"))?;
    if certs.is_empty() {
        return Err(format!("`{cert_file}` holds no certificates"));
    }
    let key = PrivateKeyDer::from_pem_file(key_file)
        .map_err(|e| format!("cannot read the private key file `{key_file}`: {e}"))?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("the certificate and key do not make a TLS identity: {e}"))?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// What a client connection will speak inside TLS, offered with ALPN.
#[derive(Clone, Copy)]
pub(crate) enum Protocol {
    Http,
    Postgres,
}

/// The client configurations, built once each: the built-in roots and those from `SSL_CERT_FILE`.
fn client_config(protocol: Protocol) -> Result<Arc<ClientConfig>, String> {
    static HTTP: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    static POSTGRES: OnceLock<Result<Arc<ClientConfig>, String>> = OnceLock::new();
    let (cell, alpn): (_, &[u8]) = match protocol {
        Protocol::Http => (&HTTP, b"http/1.1"),
        Protocol::Postgres => (&POSTGRES, b"postgresql"),
    };
    cell.get_or_init(|| {
        let mut config = ClientConfig::builder().with_root_certificates(roots()?).with_no_client_auth();
        config.alpn_protocols = vec![alpn.to_vec()];
        Ok(Arc::new(config))
    })
    .clone()
}

fn roots() -> Result<RootCertStore, String> {
    let mut roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    if let Some(file) = std::env::var_os("SSL_CERT_FILE") {
        let name = file.to_string_lossy().into_owned();
        let certs =
            CertificateDer::pem_file_iter(&file).map_err(|e| format!("cannot read SSL_CERT_FILE `{name}`: {e}"))?;
        for cert in certs {
            let cert = cert.map_err(|e| format!("cannot read SSL_CERT_FILE `{name}`: {e}"))?;
            roots.add(cert).map_err(|e| format!("a certificate in SSL_CERT_FILE `{name}`: {e}"))?;
        }
    }
    Ok(roots)
}

/// The TLS half of a socket.
pub(crate) struct Session(Mutex<Connection>);

impl Session {
    pub(crate) fn server(config: &Arc<ServerConfig>) -> Result<Session, String> {
        let connection = ServerConnection::new(config.clone()).map_err(|e| e.to_string())?;
        Ok(Session(Mutex::new(connection.into())))
    }

    pub(crate) fn client(host: &str, protocol: Protocol) -> Result<Session, String> {
        let name = ServerName::try_from(host.to_string()).map_err(|_| format!("`{host}` is not a valid host name"))?;
        let connection = ClientConnection::new(client_config(protocol)?, name).map_err(|e| e.to_string())?;
        Ok(Session(Mutex::new(connection.into())))
    }

    /// A PostgreSQL client session that encrypts but accepts any certificate: libpq's
    /// `sslmode=require`. The handshake's signature is still checked, against whatever key the
    /// certificate holds.
    pub(crate) fn client_unverified(host: &str) -> Result<Session, String> {
        static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        let config = CONFIG.get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut config = ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
                .with_no_client_auth();
            config.alpn_protocols = vec![b"postgresql".to_vec()];
            Arc::new(config)
        });
        // A server name is still needed for SNI; an address works too.
        let name = ServerName::try_from(host.to_string()).map_err(|_| format!("`{host}` is not a valid host name"))?;
        let connection = ClientConnection::new(config.clone(), name).map_err(|e| e.to_string())?;
        Ok(Session(Mutex::new(connection.into())))
    }

    pub(crate) fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn missing_files_are_reported() {
        let error = super::server_config("/nonexistent/cert.pem", "/nonexistent/key.pem").unwrap_err();
        assert!(error.contains("certificate file"), "{error}");
    }
}
