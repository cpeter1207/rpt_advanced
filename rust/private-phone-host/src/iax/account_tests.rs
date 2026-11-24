//! Native provider receipts must match both the selected registration and the listener.
use super::*;
use std::net::UdpSocket;

#[test]
fn private_listener_requires_a_native_registration_before_it_accepts_any_call() {
    let remote = UdpSocket::bind("127.0.0.1:0").unwrap();
    let profile = crate::phone::PreparedAccount::copy(crate::phone::AccountRow {
        port_id: [1;16], account_id: [2;16], generation: 17, protocol: crate::phone::Protocol::Iax2,
        server: "127.0.0.1", port: remote.local_addr().unwrap().port(), username:"native-account",
        credential:b"fixture-only", jitter_ms:[20,40,100],
    }).unwrap();
    let route = Arc::new(crate::accounts::Route::new(Arc::new(profile), remote.local_addr().unwrap()).unwrap());
    let client=IaxClient::open(Path::new("librptadviax2.so.2"), Path::new("librptadv_codec_g711.so.1")).unwrap();
    let mut listener=Listener::bind(client, Arc::clone(&route), "127.0.0.1:0".parse().unwrap()).unwrap();
    assert!(listener.registered().is_none());
    // The unrelated UDP socket is not an IAX registrar and can never grant account ownership.
    assert!(listener.register(false, 100).is_err());
    assert!(listener.registered().is_none());
    assert!(listener.dial("123").is_err());
    assert!(listener.poll().unwrap().is_none());
}
