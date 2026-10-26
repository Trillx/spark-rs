//! TLS for IMAP and SMTP. A server's certificate is checked as the system
//! checks it (Windows' certificate store, so company authorities count),
//! and one the user chose to trust is accepted where that check fails: that
//! exact certificate, by its SHA-256 fingerprint. A certificate that's
//! refused comes back as `Error::Certificate`, so the user can be asked
//! about it, as Spark asks.
//!
//! Trusting a certificate only skips the check of who issued it. The
//! handshake still has to prove the server holds the certificate's key.

use std::sync::{Arc, Mutex, OnceLock};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::{Error, Result, Service, Untrusted};

/// A certificate's SHA-256, as colon-separated hex pairs.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// The system's check, then the trusted fingerprint; it remembers a
/// certificate it refused.
#[derive(Debug)]
struct Verifier {
    system: Arc<rustls_platform_verifier::Verifier>,
    trusted: Option<String>,
    /// The refused certificate's fingerprint, and why it was refused.
    refused: Mutex<Option<(String, String)>>,
}

impl Verifier {
    fn new(trusted: Option<&str>) -> Result<Self> {
        Ok(Self {
            system: system()?,
            trusted: trusted.map(str::to_owned),
            refused: Mutex::new(None),
        })
    }

    fn refused(&self) -> Option<(String, String)> {
        self.refused
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let checked = self.system.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        let Err(e) = checked else {
            return checked;
        };
        let seen = fingerprint(end_entity);
        if self.trusted.as_deref() == Some(seen.as_str()) {
            return Ok(ServerCertVerified::assertion());
        }
        *self
            .refused
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((seen, reason(&e)));
        Err(e)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.system.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.system.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.system.supported_verify_schemes()
    }
}

/// The system's verifier, made once: elsewhere than Windows it reads the
/// system's certificates, which only needs doing once.
fn system() -> Result<Arc<rustls_platform_verifier::Verifier>> {
    static SYSTEM: OnceLock<Arc<rustls_platform_verifier::Verifier>> = OnceLock::new();
    if let Some(system) = SYSTEM.get() {
        return Ok(system.clone());
    }
    let made = rustls_platform_verifier::Verifier::new(provider())
        .map_err(|e| Error::Server(format!("Couldn't load this computer's certificates: {e}")))?;
    Ok(SYSTEM.get_or_init(|| Arc::new(made)).clone())
}

/// Why a certificate was refused, for the user: it finishes "The
/// certificate of imap.example.com isn't trusted: …".
fn reason(e: &rustls::Error) -> String {
    use CertificateError as C;
    let rustls::Error::InvalidCertificate(e) = e else {
        return e.to_string();
    };
    match e {
        C::UnknownIssuer => "it isn't from an authority this computer trusts",
        C::Expired | C::ExpiredContext { .. } => "it has expired",
        C::NotValidYet | C::NotValidYetContext { .. } => "it isn't valid yet",
        C::NotValidForName | C::NotValidForNameContext { .. } => "it was issued for another server",
        C::Revoked => "it has been revoked",
        C::InvalidPurpose | C::InvalidPurposeContext { .. } => "it isn't meant for mail servers",
        _ => "it isn't valid",
    }
    .into()
}

/// Starts TLS with `host` over `tcp`. `trusted` is the fingerprint the
/// user trusts for this server, if any.
pub(crate) async fn connect(
    service: Service,
    host: &str,
    tcp: TcpStream,
    trusted: Option<&str>,
) -> Result<TlsStream<TcpStream>> {
    let name = ServerName::try_from(host.to_owned()).map_err(|e| Error::network(host, e))?;
    let verifier = Arc::new(Verifier::new(trusted)?);
    let config = ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .expect("aws-lc-rs supports the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await
        .map_err(|e| match verifier.refused() {
            Some((fingerprint, reason)) => untrusted(service, host, fingerprint, reason),
            None => Error::network(host, e),
        })
}

/// Whether the system trusts a certificate chain (leaf first) for `host`.
pub(crate) fn system_trusts(host: &str, chain: &[Vec<u8>]) -> Result<bool> {
    let Some((leaf, intermediates)) = chain.split_first() else {
        return Ok(false);
    };
    let name = ServerName::try_from(host.to_owned()).map_err(|e| Error::network(host, e))?;
    let intermediates: Vec<CertificateDer> = intermediates
        .iter()
        .map(|der| CertificateDer::from(der.as_slice()))
        .collect();
    Ok(system()?
        .verify_server_cert(
            &CertificateDer::from(leaf.as_slice()),
            &intermediates,
            &name,
            &[],
            UnixTime::now(),
        )
        .is_ok())
}

pub(crate) fn untrusted(
    service: Service,
    host: &str,
    fingerprint: String,
    reason: String,
) -> Error {
    Error::Certificate(Box::new(Untrusted {
        service,
        host: host.to_owned(),
        fingerprint,
        reason,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_are_sha256_in_hex_pairs() {
        let f = fingerprint(b"");
        assert!(f.starts_with("E3:B0:C4:42:98:FC:1C:14"));
        assert_eq!(f.len(), 32 * 3 - 1);
    }

    #[test]
    fn reasons_read_as_the_end_of_a_sentence() {
        let e = rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer);
        assert_eq!(
            reason(&e),
            "it isn't from an authority this computer trusts"
        );
        let e = rustls::Error::InvalidCertificate(CertificateError::Expired);
        assert_eq!(reason(&e), "it has expired");
    }
}
