//! Explicit first-use certificate inspection; never used by normal adapters.
use crate::{Result, androidtv::Pinned, transport::Timed};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme, StreamOwned,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Debug)]
struct Inspect {
    signatures: Pinned,
    observed: Arc<Mutex<Option<[u8; 32]>>>,
}
impl ServerCertVerifier for Inspect {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        *self
            .observed
            .lock()
            .map_err(|_| rustls::Error::General("Certificate inspector unavailable".into()))? =
            Some(Sha256::digest(end.as_ref()).into());
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.signatures.verify_tls12_signature(msg, cert, sig)
    }
    fn verify_tls13_signature(
        &self,
        msg: &[u8],
        cert: &CertificateDer<'_>,
        sig: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.signatures.verify_tls13_signature(msg, cert, sig)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.signatures.supported_verify_schemes()
    }
}
pub fn certificate_sha256(address: SocketAddr) -> Result<String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let observed = Arc::new(Mutex::new(None));
    let mut cfg = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Inspect {
            signatures: Pinned {
                fingerprint: [0; 32],
                provider,
            },
            observed: observed.clone(),
        }))
        .with_no_client_auth();
    cfg.resumption = rustls::client::Resumption::disabled();
    cfg.max_fragment_size = Some(1024);
    let mut conn =
        ClientConnection::new(Arc::new(cfg), ServerName::IpAddress(address.ip().into()))?;
    conn.set_buffer_limit(Some(8192));
    let tcp = Timed::connect(address, Instant::now() + Duration::from_secs(8))?;
    let mut stream = StreamOwned::new(conn, tcp);
    while stream.conn.is_handshaking() {
        stream.conn.complete_io(&mut stream.sock)?;
    }
    let hash = observed
        .lock()
        .map_err(|_| crate::error("Certificate inspector unavailable"))?
        .ok_or_else(|| crate::error("No endpoint certificate"))?;
    Ok(hash.iter().map(|b| format!("{b:02x}")).collect())
}
