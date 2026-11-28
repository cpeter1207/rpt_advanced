//! Bounded mutually authenticated requests on the controller's loopback origin.
use crate::{Error, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use serde_json::Value;
use std::{
    io::{self, Read},
    net::ToSocketAddrs,
    sync::Arc,
    time::Duration,
};
use url::Url;

/// Report the class without printing request paths, bodies or credential material.
#[derive(Debug)]
pub struct TransportFailure(ureq::ErrorKind);

impl std::fmt::Display for TransportFailure {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(out, "management transport: {}", self.0)
    }
}

impl std::error::Error for TransportFailure {}

#[derive(Debug)]
pub struct HttpStatus(pub u16);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.0)
    }
}
impl std::error::Error for HttpStatus {}

/// Read at most the client's transfer limit, including on bodies of unknown length.
pub fn bounded(reader: impl Read) -> Result<Vec<u8>> {
    const LIMIT: usize = 4 * 1024 * 1024;
    let mut data = Vec::new();
    reader.take((LIMIT + 1) as u64).read_to_end(&mut data)?;
    if data.len() > LIMIT {
        return Err("management body too large".into());
    }
    Ok(data)
}

/// One HAL representation and its target-specific strong validator.
pub struct Reply {
    pub body: Value,
    pub etag: Option<String>,
    /// Transfer bytes reserved against the command's aggregate response budget.
    pub body_bytes: usize,
}

/// This client never follows redirects, and connects only to loopback addresses.
pub struct Client {
    agent: ureq::Agent,
    pub root: Url,
}

impl Client {
    pub fn new(endpoint: &str, ca: &str, cert: &str, key: &str) -> Result<Self> {
        let root = Url::parse(endpoint)?;
        if root.scheme() != "https"
            || root.host().is_none()
            || !root.username().is_empty()
            || root.password().is_some()
            || root.fragment().is_some()
            || root.query().is_some()
        {
            return Err("expected an HTTPS API root".into());
        }
        let mut trust = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_file_iter(ca)? {
            trust.add(certificate?)?;
        }
        let chain =
            CertificateDer::pem_file_iter(cert)?.collect::<std::result::Result<Vec<_>, _>>()?;
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(trust)
            .with_client_auth_cert(chain, PrivateKeyDer::from_pem_file(key)?)?;
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .redirects(0)
            .timeout(Duration::from_secs(15))
            .tls_config(Arc::new(tls))
            .resolver(|address: &str| {
                let all: Vec<_> = address.to_socket_addrs()?.collect();
                if all.is_empty() || all.iter().any(|v| !v.ip().is_loopback()) {
                    return Err(io::Error::other("API endpoint must resolve to loopback"));
                }
                Ok(all)
            })
            .build();
        Ok(Self { agent, root })
    }

    pub fn href(&self, link: &str) -> Result<Url> {
        if !link.starts_with('/') || link.starts_with("//") {
            return Err("expected an origin-relative HAL link".into());
        }
        let url = self.root.join(link)?;
        if url.origin() != self.root.origin() || url.fragment().is_some() {
            return Err("invalid HAL origin or fragment".into());
        }
        Ok(url)
    }

    pub fn request(
        &self,
        method: &str,
        url: &Url,
        data: Option<&[u8]>,
        condition: Option<(&str, &str)>,
    ) -> Result<Reply> {
        let mut request = self.agent.request(method, url.as_str()).set(
            "Accept",
            "application/hal+json, application/schema+json, application/json",
        );
        if let Some((name, value)) = condition {
            request = request.set(name, value);
        }
        let response = if let Some(body) = data {
            let content_type = if method == "PATCH" {
                "application/json-patch+json"
            } else {
                "application/hal+json"
            };
            request.set("Content-Type", content_type).send_bytes(body)
        } else {
            request.call()
        };
        let response = match response {
            Ok(r) if (200..300).contains(&r.status()) => r,
            Ok(r) | Err(ureq::Error::Status(_, r)) => {
                return Err(HttpStatus(r.status()).into());
            }
            Err(ureq::Error::Transport(e)) => return Err(TransportFailure(e.kind()).into()),
        };
        let etag = response.header("ETag").map(str::to_owned);
        let bytes = bounded(response.into_reader())?;
        Ok(Reply {
            body_bytes: bytes.len(),
            body: if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes)?
            },
            etag,
        })
    }

    pub fn get(&self, url: &Url) -> Result<Reply> {
        self.request("GET", url, None, None)
    }

    pub fn follow(&self, doc: &Value, rel: &str) -> Result<Url> {
        let link = &doc["_links"][rel];
        let href = link["href"]
            .as_str()
            .ok_or_else(|| Error::from("missing or ambiguous HAL relation"))?;
        self.href(href)
    }

    pub fn target(&self, name: Option<&str>) -> Result<Url> {
        match name {
            None => Ok(self.root.clone()),
            Some(n) if n.starts_with('/') => self.href(n),
            Some(n) => self.follow(&self.get(&self.root)?.body, n),
        }
    }
}
