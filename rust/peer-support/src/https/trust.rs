//! Dedicated management issuer, exact service identities and mandatory TLS configuration.
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use ring::digest::{SHA256, digest};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{NoServerSessionStorage, WebPkiClientVerifier};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error, RootCertStore, ServerConfig,
    SignatureScheme,
};

use super::TransportError;

/// Stable operator-configured service name suitable for value-free admission diagnostics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceId(String);

impl ServiceId {
    /// Accept a bounded, printable identifier independent of certificate rotation.
    pub fn new(value: &str) -> Result<Self, TransportError> {
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        {
            return Err(TransportError::Policy);
        }
        Ok(Self(value.to_owned()))
    }

    /// Return the provisioned identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The host owns these TLS credentials separately from radio configuration.
pub struct ServerIdentity {
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

impl ServerIdentity {
    /// Take ownership of the supplied server leaf, intermediates and private key.
    pub fn new(chain: Vec<CertificateDer<'static>>, key: PrivateKeyDer<'static>) -> Self {
        Self { chain, key }
    }

    pub(super) fn into_resolver(mut self) -> Result<Arc<SingleCertAndKey>, TransportError> {
        // ring borrows the delivered DER only for parsing. Drop wipes it at this boundary.
        let key = rustls::crypto::ring::sign::any_supported_type(&self.key)
            .map_err(|_| TransportError::Tls)?;
        let cert = CertifiedKey::new(std::mem::take(&mut self.chain), key);
        cert.keys_match().map_err(|_| TransportError::Tls)?;
        Ok(Arc::new(SingleCertAndKey::from(cert)))
    }
}

pub(super) fn configure(
    server: Arc<SingleCertAndKey>,
    trust: Arc<ServiceTrust>,
) -> Result<ServerConfig, TransportError> {
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| TransportError::Tls)?
            .with_client_cert_verifier(trust)
            .with_cert_resolver(server);
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    config.max_early_data_size = 0;
    // Rustls' server builder starts with a disabled stateless ticketer.
    if config.ticketer.enabled() {
        return Err(TransportError::Tls);
    }
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

impl Drop for ServerIdentity {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.key);
    }
}

/// Current client issuer trust and authorized exact SHA-256 leaf pins.
pub struct ServiceTrust {
    webpki: Arc<dyn ClientCertVerifier>,
    pins: BTreeMap<[u8; 32], ServiceId>,
}

impl fmt::Debug for ServiceTrust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceTrust")
    }
}

impl ServiceTrust {
    /// Compile a complete replacement issuer and identity set; ambiguous pins are invalid.
    pub fn new(
        issuers: Vec<CertificateDer<'static>>,
        pins: Vec<([u8; 32], ServiceId)>,
    ) -> Result<Self, TransportError> {
        let mut roots = RootCertStore::empty();
        for ca in issuers {
            roots.add(ca).map_err(|_| TransportError::Tls)?;
        }
        let webpki = WebPkiClientVerifier::builder_with_provider(
            roots.into(),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|_| TransportError::Tls)?;
        let mut identities = BTreeMap::new();
        for (pin, service) in pins {
            if identities.insert(pin, service).is_some() {
                return Err(TransportError::Policy);
            }
        }
        Ok(Self {
            webpki,
            pins: identities,
        })
    }

    /// Calculate the configured pin over the complete DER leaf certificate.
    pub fn leaf_pin(leaf: &CertificateDer<'_>) -> [u8; 32] {
        digest(&SHA256, leaf.as_ref())
            .as_ref()
            .try_into()
            .expect("SHA-256 size")
    }

    fn actor(
        &self,
        leaf: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<&ServiceId, Error> {
        self.webpki.verify_client_cert(leaf, intermediates, now)?;
        self.pins
            .get(&Self::leaf_pin(leaf))
            .ok_or(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
    }

    pub(super) fn identify(
        &self,
        chain: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Option<ServiceId> {
        let (leaf, rest) = chain.split_first()?;
        self.actor(leaf, rest, now).ok().cloned()
    }
}

// All certificate-path and proof-of-possession operations are delegated to rustls.
// Add only an exact authorization check after successful webpki validation.
impl ClientCertVerifier for ServiceTrust {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.webpki.root_hint_subjects()
    }
    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        chain: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        self.actor(cert, chain, now)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.webpki.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.webpki.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.webpki.supported_verify_schemes()
    }
}
