//! Native descriptor fixture shared by provider and host-dispatch tests.
mod fixture;
pub use fixture::Fixture;
impl Fixture {
    pub fn peer(&self) -> std::sync::Arc<rptadv_peer_support::echolink::host::Peer> {
        use rptadv_peer_support::echolink::{abi, host::Provider};
        let library = unsafe { libloading::Library::new(&self.0) }.unwrap();
        let provider = unsafe { Provider::load(&self.0, &abi::RuntimeConfig::default()) }.unwrap();
        let account = unsafe {
            provider.prepare(
                &abi::AccountConfig {
                    enabled: 1,
                    local_node: abi::ByteSpan {
                        data: b"524950".as_ptr(),
                        length: 6,
                    },
                    ..abi::AccountConfig::default()
                },
                &abi::AccountSecrets {
                    struct_size: std::mem::size_of::<abi::AccountSecrets>() as u32,
                    abi_version: 1,
                    directory_password: abi::ByteSpan::default(),
                    proxy_password: abi::ByteSpan::default(),
                },
            )
        }
        .unwrap();
        let create =
            unsafe { library.get::<unsafe extern "C" fn() -> *mut abi::Qso>(b"fixture_peer\0") }
                .unwrap();
        let mut taken = 0;
        unsafe {
            account.take_pending(&abi::Event {
                struct_size: std::mem::size_of::<abi::Event>() as u32,
                kind: 1,
                qso: create(),
                generation: 9,
                status: abi::Status::Pending,
                taken: &mut taken,
            })
        }
        .unwrap()
    }
}
