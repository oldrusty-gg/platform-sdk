//! TLS for every connection that leaves the machine (rustls with the
//! `ring` provider). A client trusts the server either through the public
//! web PKI (Mozilla's roots: a Let's Encrypt certificate on a real host
//! name) or through a pinned SHA-256 fingerprint of the server's own
//! certificate (a self-signed certificate on a small private server; the
//! fingerprint is handed to friends with the address). Unencrypted
//! connections stay loopback-only ([`crate::transport::FramedTcp::connect`]).

use crate::transport::{FramedTcp, Stream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection, SignatureScheme, StreamOwned};
use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{self, ErrorKind, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// TLS client over TCP.
pub type ClientTls = StreamOwned<ClientConnection, TcpStream>;
/// TLS server side over TCP.
pub type ServerTls = StreamOwned<ServerConnection, TcpStream>;

impl Stream for ClientTls {
    fn shutdown(&mut self) {
        self.conn.send_close_notify();
        let _ = self.flush();
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

impl Stream for ServerTls {
    fn shutdown(&mut self) {
        self.conn.send_close_notify();
        let _ = self.flush();
        let _ = self.sock.shutdown(std::net::Shutdown::Both);
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// How a client decides the server is the right one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// A certificate for the host name from a public authority (Let's
    /// Encrypt), checked against Mozilla's roots.
    WebPki,
    /// Exactly this certificate (SHA-256 of its DER), whoever signed it.
    Pinned([u8; 32]),
}

/// `sha256:<64 hex>` of a certificate's DER (what a self-signed server
/// prints and its friends put in their `online.json`).
pub fn fingerprint(cert_der: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(cert_der))
}

/// Read a fingerprint written as `sha256:<hex>` (colons and spaces between
/// the hex digits are ignored, case does not matter).
pub fn parse_fingerprint(text: &str) -> Result<[u8; 32], String> {
    let t = text.trim();
    let t = t.strip_prefix("sha256:").or_else(|| t.strip_prefix("SHA256:")).unwrap_or(t);
    let hex: String = t.chars().filter(|c| !matches!(c, ':' | ' ' | '-')).collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("{text:?} is not a SHA-256 fingerprint (64 hex digits)"));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Accepts exactly the pinned certificate; the handshake signatures are
/// still verified with the provider's algorithms.
#[derive(Debug)]
struct Pinned {
    sha256: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        // Not secret: a plain comparison is fine.
        if got == self.sha256 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "the server's certificate {} is not the pinned one",
                fingerprint(end_entity.as_ref())
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// The client configuration for `trust`.
pub fn client_config(trust: &Trust) -> io::Result<Arc<ClientConfig>> {
    let provider = provider();
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?;
    let config = match trust {
        Trust::WebPki => {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        Trust::Pinned(sha256) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Pinned { sha256: *sha256, provider }))
            .with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// Connect to `host:port` over TLS, finish the handshake (so a wrong
/// certificate fails here, with its reason) and frame messages over it.
pub fn connect<Out: Serialize, In: DeserializeOwned>(
    host: &str,
    port: u16,
    trust: &Trust,
    timeout: Duration,
) -> io::Result<FramedTcp<Out, In, ClientTls>> {
    let config = client_config(trust)?;
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, format!("{host} is not a host name or address")))?;
    let mut last = io::Error::new(ErrorKind::NotFound, format!("{host} has no address"));
    let mut sock = None;
    for addr in (host, port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                sock = Some(s);
                break;
            }
            Err(e) => last = e,
        }
    }
    let mut sock = sock.ok_or(last)?;
    sock.set_nodelay(true)?;
    sock.set_read_timeout(Some(timeout))?;
    sock.set_write_timeout(Some(timeout))?;
    let mut conn = ClientConnection::new(config, name).map_err(io::Error::other)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).map_err(|e| {
            io::Error::new(e.kind(), format!("TLS handshake with {host}:{port} failed: {e}"))
        })?;
    }
    sock.set_read_timeout(None)?;
    sock.set_write_timeout(None)?;
    sock.set_nonblocking(true)?;
    Ok(FramedTcp::over(StreamOwned::new(conn, sock)))
}

/// A server configuration from PEM files: the certificate chain (Let's
/// Encrypt's `fullchain.pem`, or a self-signed `cert.pem`) and its private
/// key (`privkey.pem` / `key.pem`).
pub fn server_config(cert_pem: &Path, key_pem: &Path) -> io::Result<Arc<ServerConfig>> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_pem)
        .map_err(|e| io::Error::other(format!("{}: {e}", cert_pem.display())))?
        .collect::<Result<_, _>>()
        .map_err(|e| io::Error::other(format!("{}: {e}", cert_pem.display())))?;
    if certs.is_empty() {
        return Err(io::Error::other(format!("{} holds no certificate", cert_pem.display())));
    }
    let key = PrivateKeyDer::from_pem_file(key_pem).map_err(|e| io::Error::other(format!("{}: {e}", key_pem.display())))?;
    server_config_from(certs, key)
}

/// A server configuration from DER parts.
pub fn server_config_from(certs: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> io::Result<Arc<ServerConfig>> {
    let config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(io::Error::other)?;
    Ok(Arc::new(config))
}

/// The first certificate of a PEM file (its fingerprint is what clients pin).
pub fn first_certificate(cert_pem: &Path) -> io::Result<Vec<u8>> {
    CertificateDer::from_pem_file(cert_pem)
        .map(|c| c.as_ref().to_vec())
        .map_err(|e| io::Error::other(format!("{}: {e}", cert_pem.display())))
}

/// Wrap an accepted TCP stream as the server side of a TLS connection (the
/// handshake runs as the connection is polled).
pub fn accept_stream(config: Arc<ServerConfig>, sock: TcpStream) -> io::Result<ServerTls> {
    sock.set_nonblocking(true)?;
    sock.set_nodelay(true)?;
    let conn = ServerConnection::new(config).map_err(io::Error::other)?;
    Ok(StreamOwned::new(conn, sock))
}
