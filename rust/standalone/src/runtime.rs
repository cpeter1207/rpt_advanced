//! Process-lifetime owner for the portable product.

use crate::host_services::HostServicesOwner;

/// Product startup, reload, or shutdown failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeError {
    /// The product lifecycle table is incomplete.
    InvalidDescriptor,
    /// A required shared provider is missing or incompatible.
    Provider(crate::providers::ProviderError),
    /// Product start rejected the selected providers or configuration.
    Start,
    /// Product could not prepare the replacement configuration.
    Reload,
    /// Product could not quiesce all runtime owners.
    Stop,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidDescriptor => "product lifecycle descriptor is incomplete",
            Self::Start => "standalone product startup failed",
            Self::Reload => "standalone configuration reload failed",
            Self::Stop => "standalone product shutdown failed",
            Self::Provider(error) => return error.fmt(formatter),
        })
    }
}

impl std::error::Error for RuntimeError {}

/// Running product and host context. Product stop precedes host/provider release.
pub struct ProductRuntime {
    reload: unsafe extern "C" fn() -> i32,
    reload_poll: unsafe extern "C" fn() -> i32,
    stop: unsafe extern "C" fn() -> i32,
    host: Option<HostServicesOwner>,
    providers: Option<Box<crate::providers::ProviderSet>>,
    product_stopped: bool,
}

impl ProductRuntime {
    /// Start the managed process without opening any radio or network account here.
    /// On -2 the product still owns callbacks; the foreground retains and drains
    /// this host and its libraries before returning the startup failure.
    pub(crate) fn start_management(provisioning: &[u8]) -> Result<(Self, bool), RuntimeError> {
        let providers = Box::new(
            crate::providers::ProviderSet::load_management().map_err(RuntimeError::Provider)?,
        );
        // SAFETY: moving the Box does not move its ProviderSet. ProductRuntime
        // releases the host and confirmed product before releasing the boxed DSO owners.
        let retained = unsafe { &*(providers.as_ref() as *const crate::providers::ProviderSet) };
        let descriptors = retained
            .product_runtime_descriptors()
            .map_err(RuntimeError::Provider)?;
        let start = retained
            .management_descriptor()
            .map_err(RuntimeError::Provider)?
            .start
            .ok_or(RuntimeError::InvalidDescriptor)?;
        let mut host = HostServicesOwner::new(None).map_err(RuntimeError::Provider)?;
        host.install_private(descriptors.product);
        host.install_asl()?;
        host.install_managed_echo(&descriptors)?;
        let reload = descriptors
            .product
            .reload_working
            .ok_or(RuntimeError::InvalidDescriptor)?;
        let reload_poll = descriptors
            .product
            .reload_poll
            .ok_or(RuntimeError::InvalidDescriptor)?;
        let stop = descriptors
            .product
            .stop
            .ok_or(RuntimeError::InvalidDescriptor)?;
        // SAFETY: the host/provider owners survive success and an unconfirmed failure.
        // Provisioning bytes remain readable until the synchronous entry returns.
        let status = unsafe {
            start(
                host.descriptor(),
                descriptors.control,
                descriptors.file,
                descriptors.speech,
                provisioning.as_ptr().cast(),
                provisioning.len(),
            )
        };
        if !matches!(status, 0 | -2) {
            return Err(RuntimeError::Start);
        }
        if status == 0 {
            host.open_asl();
        }
        Ok((
            Self {
                stop,
                reload,
                reload_poll,
                host: Some(host),
                providers: Some(providers),
                product_stopped: false,
            },
            status == 0,
        ))
    }

    /// Begin or poll the bounded trusted-host submission without blocking signal handling.
    pub(crate) fn working_reload(&self, pending: bool) -> Result<bool, RuntimeError> {
        // SAFETY: both callbacks belong to the retained product and operate on
        // its selected working store.
        let status = unsafe {
            if pending {
                (self.reload_poll)()
            } else {
                (self.reload)()
            }
        };
        match status {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(RuntimeError::Reload),
        }
    }

    /// Stop product workers before releasing host state and provider libraries.
    pub fn stop(&mut self) -> Result<(), RuntimeError> {
        if self.host.as_ref().is_some_and(|host| !host.asl_quiescent()) {
            return Err(RuntimeError::Stop);
        }
        if !self.product_stopped {
            if unsafe { (self.stop)() } != 0 {
                return Err(RuntimeError::Stop);
            }
            self.product_stopped = true;
        }
        if self
            .host
            .as_ref()
            .is_some_and(|host| !host.private_stopped())
        {
            return Err(RuntimeError::Stop);
        }
        if self.host.as_mut().is_some_and(|host| !host.join_asl()) {
            return Err(RuntimeError::Stop);
        }
        self.host.take();
        self.providers.take();
        Ok(())
    }
}

impl Drop for ProductRuntime {
    fn drop(&mut self) {
        if self.host.is_some() && self.stop().is_err() {
            // A failed stop may still have callbacks into host state and provider code.
            // Retain both rather than unloading code under live callbacks.
            std::mem::forget(self.host.take());
            if let Some(providers) = self.providers.take() {
                std::mem::forget(providers);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::abi;
    use std::ffi::c_void;

    unsafe extern "C" fn complete() -> i32 {
        0
    }

    unsafe extern "C" fn pending() -> i32 {
        1
    }

    unsafe extern "C" fn reject() -> i32 {
        -1
    }

    #[test]
    fn managed_reload_keeps_pending_receipt_and_stop_retries_keep_host() {
        let mut runtime = ProductRuntime {
            reload: pending,
            reload_poll: complete,
            stop: reject,
            host: Some(HostServicesOwner::new(None).unwrap()),
            providers: None,
            product_stopped: false,
        };
        assert_eq!(runtime.working_reload(false), Ok(true));
        assert_eq!(runtime.working_reload(true), Ok(false));
        runtime.reload_poll = reject;
        assert_eq!(runtime.working_reload(true), Err(RuntimeError::Reload));

        assert_eq!(runtime.stop(), Err(RuntimeError::Stop));
        assert!(runtime.host.is_some());
        runtime.stop = complete;
        assert_eq!(runtime.stop(), Ok(()));
        assert!(runtime.host.is_none());
        runtime.stop = reject;
        assert_eq!(runtime.stop(), Ok(()));
    }

    unsafe extern "C" fn allow_registration_fixture(
        _: *const abi::rptadv_incoming_peer_v1,
        _: *mut c_void,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn reject_unsolicited_peer(
        _: *const abi::rptadv_incoming_peer_v1,
        _: *mut c_void,
    ) -> i32 {
        1
    }

    fn phone_product() -> abi::rptadv_product_descriptor_v1 {
        // SAFETY: all unspecified descriptor fields are nullable pointers or scalars.
        let mut product: abi::rptadv_product_descriptor_v1 = unsafe { std::mem::zeroed() };
        product.struct_size = size_of::<abi::rptadv_product_descriptor_v1>() as u32;
        product.abi_version = 8;
        product.capability = *b"rptadv.prod8\0\0\0\0";
        product.authorize_incoming = Some(allow_registration_fixture);
        product.incoming = Some(reject_unsolicited_peer);
        product
    }

    #[test]
    fn frontend_phone_group_authenticates_and_withdraws_the_actual_sip_socket() {
        let product = phone_product();
        let mut host = HostServicesOwner::new(None).unwrap();
        host.install_private(&product);
        let table = host.descriptor();
        unsafe { super::test_phone_group::register(table.context, table.phone_provider.cast()) };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !host.private_stopped() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn scoped_sip_originate_reaches_the_registered_recipient_and_releases_its_dialog() {
        use super::test_phone_group::registrar::{header, receive_register, response};
        use std::{io::ErrorKind, net::UdpSocket};

        let product = phone_product();
        let mut host = HostServicesOwner::new(None).unwrap();
        host.install_private(&product);
        let table = host.descriptor();
        // The descriptor belongs to the live HostServicesOwner for this entire case.
        let provider = unsafe { table.phone_provider.as_ref() }.unwrap();
        let begin = provider.begin.unwrap();
        let poll = provider.poll.unwrap();
        let cancel = provider.cancel.unwrap();
        let peer_info = table.peer_info.unwrap();
        let peer_destroy = table.peer_destroy.unwrap();
        let mut outcome = None;
        unsafe {
            super::test_phone_group::while_registered(
                table.context,
                table.phone_provider.cast(),
                |server, registered| {
                    let authority = server.local_addr().unwrap();
                    let media = UdpSocket::bind("127.0.0.1:0").unwrap();
                    outcome = Some(std::thread::scope(|scope| {
                        let peer = scope.spawn(|| {
                        let mut wire = [0; 8192];
                        let (invite, source) = match receive_register(server, &mut wire) {
                            Ok(received) => received,
                            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => return None,
                            Err(error) => panic!("registrar did not receive a valid INVITE: {error}"),
                        };
                        assert!(invite.starts_with("INVITE "));
                        assert_eq!(source, registered);
                        let uri = invite.split_whitespace().nth(1).unwrap();
                        let (scheme, address) = uri.split_once(':').unwrap();
                        assert!(scheme.eq_ignore_ascii_case("sip"));
                        let (user, host) = address.split_once('@').unwrap();
                        assert_eq!(host.parse::<std::net::SocketAddr>().unwrap(), authority);
                        // Decode the user exactly once. A literal pound is invalid on the wire.
                        let mut decoded = Vec::new();
                        let mut wire_user = user.bytes();
                        while let Some(byte) = wire_user.next() {
                            if byte == b'%' {
                                let mut hex = || char::from(wire_user.next().unwrap()).to_digit(16).unwrap() as u8;
                                decoded.push((hex() << 4) | hex());
                            } else {
                                assert!(byte.is_ascii_digit() || byte == b'*');
                                decoded.push(byte);
                            }
                        }
                        assert_eq!(decoded, b"18*52#");
                        let dialog = header(&invite, "Call-ID").to_owned();
                        let answer = format!("v=0\r\no=test 1 1 IN IP4 127.0.0.1\r\ns=private\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na=ptime:20\r\na=sendrecv\r\n", media.local_addr().unwrap().port());
                        let reply = response(&invite, 200, false).replace("Content-Length: 0\r\n\r\n",
                            &format!("Contact: <sip:recipient@{authority}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{answer}", answer.len()));
                        server.send_to(reply.as_bytes(), source).unwrap();
                        let (ack, from) = receive_register(server, &mut wire).unwrap();
                        assert!(ack.starts_with("ACK "));
                        assert_eq!(header(&ack, "Call-ID"), dialog);
                        assert_eq!(from, registered);
                        let (bye, from) = receive_register(server, &mut wire).unwrap();
                        assert!(bye.starts_with("BYE "));
                        assert_eq!(header(&bye, "Call-ID"), dialog);
                        assert_eq!(from, registered);
                        server.send_to(response(&bye, 200, false).as_bytes(), from).unwrap();
                        Some(())
                    });
                        let mut call = std::ptr::null_mut();
                        let target = b"18*52#";
                        let dial = abi::rptadv_peer_bytes_v1 {
                            data: target.as_ptr(),
                            length: target.len(),
                        };
                        let mut pending = std::ptr::null_mut();
                        let mut status = begin(
                            table.context,
                            [1u8; 16].as_ptr(),
                            [2u8; 16].as_ptr(),
                            7,
                            dial,
                            160,
                            &mut pending,
                        );
                        if status == 0 {
                            assert!(!pending.is_null());
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(35);
                            loop {
                                status =
                                    poll(table.context, pending, std::ptr::null_mut(), &mut call);
                                if status != 1 || std::time::Instant::now() >= deadline {
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                            cancel(table.context, pending);
                        }
                        let metadata = (!call.is_null()).then(|| {
                            let mut info: abi::rptadv_peer_info_v1 = std::mem::zeroed();
                            info.struct_size = size_of::<abi::rptadv_peer_info_v1>() as u32;
                            info.abi_version = 1;
                            let result = peer_info(table.context, call, &mut info);
                            peer_destroy(table.context, call);
                            (result, info.account_generation, info.qso_generation)
                        });
                        (status, metadata, peer.join())
                    }));
                },
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !host.private_stopped() {
            assert!(
                std::time::Instant::now() < deadline,
                "standalone phone owner did not join"
            );
            std::thread::yield_now();
        }
        eprintln!("standalone SIP account withdrawn and frontend joined");
        let (status, metadata, dialog) = outcome.unwrap();
        let dialog = dialog.unwrap();
        assert_eq!(
            status,
            0,
            "registered standalone SIP originate returned {status}; wire dialog {}",
            dialog.is_some()
        );
        assert!(
            dialog.is_some(),
            "registrar did not settle the same SIP dialog"
        );
        assert!(
            metadata.is_some_and(|(result, account, qso)| result == 0 && account == 7 && qso != 0),
            "standalone SIP call did not expose the expected ABI1 metadata"
        );
    }
}

#[cfg(test)]
#[path = "../../private-phone-host/tests/support/frontend_account.rs"]
mod test_phone_group;
