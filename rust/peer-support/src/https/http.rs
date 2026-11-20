//! Bound HTTP/1 framing, TLS connection tasks and credential-bearing body ownership.
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Bytes, Incoming};
use hyper::header::{self, HeaderName, HeaderValue};
use hyper::http::uri::{Authority, PathAndQuery};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;
use zeroize::Zeroizing;

use super::{CONNECTIONS, Chain, HANDSHAKES, RequestBody, Shared, TransportError, dispatch};

const HEADERS: usize = 64;
const HEADER_BYTES: usize = 16 * 1024;
const TARGET_BYTES: usize = 2048;
const BODY_BYTES: usize = 1024 * 1024;
const RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const HANDSHAKE_TIME: Duration = Duration::from_secs(3);
const HEADER_TIME: Duration = Duration::from_secs(8);
const BODY_TIME: Duration = Duration::from_secs(5);
const CONNECTION_TIME: Duration = Duration::from_secs(120);

/// Trusted backend authorities and browser origins from host provisioning.
pub struct OriginPolicy {
    hosts: Vec<Authority>,
    origins: Vec<HeaderValue>,
}
impl OriginPolicy {
    /// Configure exact authorities and origins. An absent Origin is valid for a direct CLI.
    pub fn new(hosts: Vec<String>, origins: Vec<String>) -> Result<Self, TransportError> {
        let hosts = hosts
            .into_iter()
            .map(|host| {
                if host.contains('@') {
                    return Err(TransportError::Policy);
                }
                host.parse::<Authority>()
                    .map_err(|_| TransportError::Policy)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if hosts.is_empty() {
            return Err(TransportError::Policy);
        }
        let origins = origins
            .into_iter()
            .map(|origin| {
                let uri = origin.parse::<Uri>().map_err(|_| TransportError::Policy)?;
                let authority = uri.authority().ok_or(TransportError::Policy)?;
                let scheme = uri.scheme_str().ok_or(TransportError::Policy)?;
                if !matches!(scheme, "http" | "https")
                    || authority.as_str().contains('@')
                    || origin != format!("{scheme}://{authority}")
                {
                    return Err(TransportError::Policy);
                }
                HeaderValue::from_str(&origin).map_err(|_| TransportError::Policy)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { hosts, origins })
    }

    fn check(&self, headers: &HeaderMap) -> Result<(), StatusCode> {
        let host = single(headers, header::HOST)?.ok_or(StatusCode::BAD_REQUEST)?;
        if !self.hosts.iter().any(|allowed| {
            host.as_bytes()
                .eq_ignore_ascii_case(allowed.as_str().as_bytes())
        }) {
            return Err(StatusCode::FORBIDDEN);
        }
        if single(headers, header::ORIGIN)?.is_some_and(|origin| !self.origins.contains(origin)) {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(())
    }
}

fn single(headers: &HeaderMap, name: HeaderName) -> Result<Option<&HeaderValue>, StatusCode> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(value)
}

pub(super) fn empty(status: StatusCode) -> Response<Vec<u8>> {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

fn target(uri: &Uri) -> Result<PathAndQuery, StatusCode> {
    let path = uri
        .path_and_query()
        .ok_or(StatusCode::BAD_REQUEST)?
        .as_str();
    if uri.scheme().is_some()
        || uri.authority().is_some()
        || !path.starts_with('/')
        || path.starts_with("//")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if path.len() > TARGET_BYTES {
        return Err(StatusCode::URI_TOO_LONG);
    }
    // Own only the visible canonical path, independently of Hyper's input slab.
    path.parse().map_err(|_| StatusCode::BAD_REQUEST)
}

fn media(method: &Method, headers: &HeaderMap) -> Result<bool, StatusCode> {
    if [
        "x-http-method-override",
        "x-method-override",
        "x-http-method",
    ]
    .iter()
    .any(|key| headers.contains_key(*key))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    if single(headers, header::CONTENT_ENCODING)?
        .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"identity"))
        || single(headers, header::TRANSFER_ENCODING)?
            .is_some_and(|v| !v.as_bytes().eq_ignore_ascii_case(b"chunked"))
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let expected: &[u8] = match *method {
        Method::POST | Method::PUT => b"application/hal+json",
        Method::PATCH => b"application/json-patch+json",
        _ => return Ok(false),
    };
    if !single(headers, header::CONTENT_TYPE)?
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(expected))
    {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    Ok(true)
}

async fn read_body(mut input: Incoming, allowed: bool) -> Result<RequestBody, StatusCode> {
    let cap = if allowed { BODY_BYTES } else { 0 };
    let error = if allowed {
        StatusCode::PAYLOAD_TOO_LARGE
    } else {
        StatusCode::BAD_REQUEST
    };
    if input.size_hint().lower() > cap as u64 {
        return Err(error);
    }
    // Reserve once, including for unknown chunked lengths, to avoid leaving old owned buffers behind.
    let capacity = input
        .size_hint()
        .upper()
        .unwrap_or(cap as u64)
        .min(cap as u64) as usize;
    let mut body = Zeroizing::new(Vec::with_capacity(capacity));
    while let Some(frame) = input.frame().await {
        let data = frame
            .map_err(|_| StatusCode::BAD_REQUEST)?
            .into_data()
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        if data.len() > cap - body.len() {
            return Err(error);
        }
        body.extend_from_slice(&data);
    }
    Ok(RequestBody(body))
}

async fn handle(
    request: Request<Incoming>,
    state: Shared,
    policy: Arc<OriginPolicy>,
    chain: Chain,
) -> Result<Response<Vec<u8>>, StatusCode> {
    let trust = state
        .lock()
        .expect("management state")
        .trust()
        .ok_or(StatusCode::FORBIDDEN)?;
    let actor = trust
        .identify(&chain, rustls::pki_types::UnixTime::now())
        .ok_or(StatusCode::FORBIDDEN)?;
    let (parts, body) = request.into_parts();
    let target = target(&parts.uri)?;
    policy.check(&parts.headers)?;
    let allowed = media(&parts.method, &parts.headers)?;
    let mut headers = HeaderMap::new();
    for name in [
        header::ACCEPT,
        header::CONTENT_TYPE,
        header::IF_MATCH,
        header::IF_NONE_MATCH,
    ] {
        for value in parts.headers.get_all(&name) {
            headers.append(
                name.clone(),
                HeaderValue::from_bytes(value.as_bytes()).map_err(|_| StatusCode::BAD_REQUEST)?,
            );
        }
    }
    let body = timeout(BODY_TIME, read_body(body, allowed))
        .await
        .map_err(|_| StatusCode::REQUEST_TIMEOUT)??;
    Ok(dispatch(state, chain, actor, parts.method, target, headers, body).await)
}

pub(super) fn encode(response: Response<Vec<u8>>) -> Response<Full<Bytes>> {
    let (parts, body) = response.into_parts();
    // The HTTP/1 owner supplies the version, canonical reason and framing.
    let mut wire = Response::new(Full::new(Bytes::from(body)));
    *wire.status_mut() = parts.status;
    *wire.headers_mut() = parts.headers;
    let headers = wire.headers_mut();
    for name in [
        header::CONTENT_LENGTH,
        header::TRANSFER_ENCODING,
        header::TRAILER,
        header::UPGRADE,
    ] {
        headers.remove(name);
    }
    let close = headers
        .get_all(header::CONNECTION)
        .iter()
        .any(|v| v.as_bytes().eq_ignore_ascii_case(b"close"));
    headers.remove(header::CONNECTION);
    if close {
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Reserve space for the canonical status line, Date, framing, a generated
    // connection header and the final CRLF, including for HTTP/1.0 clients.
    if headers.len() > HEADERS - 3
        || headers
            .iter()
            .map(|(n, v)| n.as_str().len() + v.len() + 4)
            .sum::<usize>()
            > HEADER_BYTES - 160
        || wire.body().size_hint().lower() > RESPONSE_BYTES as u64
    {
        let mut error = Response::new(Full::new(Bytes::new()));
        *error.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
        error
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        error
            .headers_mut()
            .insert(header::CONNECTION, HeaderValue::from_static("close"));
        return error;
    }
    wire
}

pub(super) async fn serve(
    mut accept: impl AsyncFnMut() -> std::io::Result<(TcpStream, std::net::SocketAddr)>,
    state: Shared,
    policy: OriginPolicy,
    mut stop: oneshot::Receiver<()>,
    health: Arc<super::Health>,
) {
    let refresh = state.lock().expect("management state").refresh.clone();
    let mut clients = JoinSet::new();
    let policy = Arc::new(policy);
    let handshakes = Arc::new(Semaphore::new(HANDSHAKES));
    loop {
        tokio::select! {
            biased;
            _ = &mut stop => break,
            _ = refresh.notified() => super::refresh_pending(&state),
            Some(result) = clients.join_next(), if !clients.is_empty() => health.joined(result),
            accepted = accept() => {
                health.accept_failed(accepted.is_err());
                let Ok((socket, _)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                };
                if clients.len() >= CONNECTIONS { continue; }
                let Ok(permit) = handshakes.clone().try_acquire_owned() else { continue; };
                let config = state.lock().expect("management state").snapshot.as_ref().map(|s| s.config.clone());
                let Some(config) = config else { break; };
                let state = state.clone();
                let policy = policy.clone();
                clients.spawn(async move {
                    let tls = timeout(HANDSHAKE_TIME, TlsAcceptor::from(config).accept(socket)).await;
                    drop(permit);
                    let Ok(Ok(stream)) = tls else { return; };
                    let Some(certs) = stream.get_ref().1.peer_certificates() else { return; };
                    let chain: Chain = Arc::from(certs);
                    let service = service_fn(move |req| {
                        let response = handle(req, state.clone(), policy.clone(), chain.clone());
                        async move { Ok::<_, Infallible>(encode(response.await.unwrap_or_else(empty))) }
                    });
                    let mut http = http1::Builder::new();
                    http.timer(TokioTimer::new()).header_read_timeout(HEADER_TIME)
                        .max_headers(HEADERS).max_header_size(HEADER_BYTES).max_buf_size(HEADER_BYTES);
                    let _ = timeout(CONNECTION_TIME, http.serve_connection(TokioIo::new(stream), service)).await;
                });
            }
        }
    }
    clients.shutdown().await;
}
