//! Service-provisioned short-lived TURN access. Signaling remains on local mTLS.

use crate::configuration_private::decode_secret_json_string;
use rptadv_json::{BorrowedChildren, BorrowedValue, decode_borrowed};
use rptadv_peer_support::{
    https::CertificateDer,
    sealed_records::{KeyId, SecretBytes, load_systemd_tls, load_systemd_turn},
    webrtc::{RelaySocket, UdpDriver, relay::RelayTransport},
};
use std::{
    net::SocketAddr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::net::{TcpStream, UdpSocket};

pub(crate) struct TurnAccess {
    server: SocketAddr,
    transport: RelayTransport,
    username: SecretBytes,
    password: SecretBytes,
    expires: Instant,
}

impl TurnAccess {
    pub(crate) fn load(id: &str) -> Result<Self, ()> {
        let credential = KeyId::new(id).map_err(|_| ())?;
        let bytes = load_systemd_turn(&credential).map_err(|_| ())?;
        let decoded = decode_borrowed(&bytes).map_err(|_| ())?;
        let BorrowedChildren::Object(fields) = decoded.children else {
            return Err(());
        };
        let raw = |name| fields.get(name).map(BorrowedValue::raw).ok_or(());
        let secret = |name| decode_secret_json_string(raw(name)?.as_bytes()).map_err(|_| ());
        let server: SocketAddr = serde_json::from_str(raw("server")?).map_err(|_| ())?;
        let expires: u64 = serde_json::from_str(raw("expires_unix")?).map_err(|_| ())?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ())?;
        let left = Duration::from_secs(expires).checked_sub(now).ok_or(())?;
        if left.is_zero()
            || server.port() == 0
            || server.ip().is_unspecified()
            || server.ip().is_multicast()
        {
            return Err(());
        }
        let transport = match serde_json::from_str::<&str>(raw("transport")?).map_err(|_| ())? {
            "udp" if fields.len() == 5 => RelayTransport::Udp,
            "tcp" if fields.len() == 5 => RelayTransport::Tcp,
            "tls" if fields.len() == 7 => {
                let name = serde_json::from_str(raw("tls_name")?).map_err(|_| ())?;
                let ca: String = serde_json::from_str(raw("tls_issuer")?).map_err(|_| ())?;
                let ca = KeyId::new(&ca).map_err(|_| ())?;
                let der = load_systemd_tls(&ca).map_err(|_| ())?;
                RelayTransport::tls(name, CertificateDer::from(der.to_vec())).map_err(|_| ())?
            }
            _ => return Err(()),
        };
        let username = secret("username")?;
        let password = secret("password")?;
        if username.expose_bytes().is_empty() || password.expose_bytes().is_empty() {
            return Err(());
        }
        Ok(Self {
            server,
            transport,
            username,
            password,
            expires: Instant::now().checked_add(left).ok_or(())?,
        })
    }

    pub(super) fn expires(&self) -> Instant {
        self.expires
    }

    pub(super) async fn gather(
        &self,
        driver: &mut UdpDriver,
        binding: std::net::IpAddr,
    ) -> Result<(), ()> {
        let now = Instant::now();
        let deadline = self.expires.min(now + Duration::from_secs(5));
        if deadline <= now {
            return Err(());
        }
        let socket = tokio::time::timeout_at(deadline.into(), async {
            match &self.transport {
                RelayTransport::Udp => {
                    let socket = UdpSocket::bind(SocketAddr::new(binding, 0)).await?;
                    socket.connect(self.server).await?;
                    Ok(RelaySocket::Udp(socket))
                }
                RelayTransport::Tcp | RelayTransport::Tls { .. } => {
                    TcpStream::connect(self.server).await.map(RelaySocket::Tcp)
                }
            }
        })
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
        let username = std::str::from_utf8(self.username.expose_bytes()).map_err(|_| ())?;
        let password = std::str::from_utf8(self.password.expose_bytes()).map_err(|_| ())?;
        driver
            .add_relay(socket, self.transport.clone(), username, password, deadline)
            .await
            .map_err(|_| ())
    }
}
