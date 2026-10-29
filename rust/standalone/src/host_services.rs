//! Asterisk-free host callbacks for the portable product runtime.

#[cfg(test)]
#[path = "host_services/echo_tests.rs"]
mod echo_tests;

use crate::{
    abi, directory,
    iax::{IaxClient, IaxError, IaxEvent, IaxPeer},
    radio_host::{
        RadioHostContext, peer_bind_radio, radio_activate, radio_destroy, radio_open,
        radio_site_bind, radio_site_publish, radio_site_snapshot,
    },
    secrets::SecretsFile,
};
use rptadv_peer_support::echolink::{
    abi as echo,
    host::{Accounts, HostPeer, Preparation, Registration},
};
use std::{
    ffi::{c_char, c_void},
    mem::size_of,
    net::SocketAddr,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{Arc, Mutex},
};

/// Context retained while the product uses the standalone host table.
pub struct StandaloneHostContext {
    radio: RadioHostContext,
    pub(crate) resolver: Option<directory::Resolver>,
    secrets: SecretsFile,
    ingress: Option<PeerIngress>,
    echo: Mutex<Option<Arc<Accounts>>>,
}

/// An executor cannot publish peers until its jitter provider is available.
struct PeerIngress {
    executor: crate::iax::executor::Executor,
    jitter: &'static abi::rptadv_jitter_descriptor_v1,
    client: IaxClient,
}

/// Owns a complete host-services table and the context it points to.
pub struct HostServicesOwner {
    context: Box<StandaloneHostContext>,
    descriptor: Box<abi::rptadv_host_services_v9>,
}

impl HostServicesOwner {
    /// Create the immutable host table around already-resolved node and provider owners.
    pub fn new(
        radios: Vec<crate::ResolvedRadioNode>,
        providers: Option<&'static crate::providers::ProviderSet>,
        secrets: SecretsFile,
    ) -> Result<Self, crate::providers::ProviderError> {
        let resolver = providers
            .map(|providers| directory::Resolver::new(providers.resolver_descriptor()?))
            .transpose()?;
        let ingress = providers
            .map(|providers| {
                Ok::<_, crate::providers::ProviderError>(PeerIngress {
                    executor: providers.iax_executor()?,
                    jitter: providers.jitter_descriptor()?,
                    client: providers.iax_client()?,
                })
            })
            .transpose()?;
        let mut context = Box::new(StandaloneHostContext {
            radio: RadioHostContext::new(radios, providers),
            resolver,
            secrets,
            ingress,
            echo: Mutex::new(None),
        });
        let descriptor = Box::new(abi::rptadv_host_services_v9 {
            struct_size: size_of::<abi::rptadv_host_services_v9>() as u32,
            abi_version: 9,
            capability: *b"rptadv.hst9\0",
            context: ptr::from_mut(context.as_mut()).cast(),
            local_time: Some(local_time),
            command_notice: Some(command_notice),
            reaper_acquire: Some(noop),
            reaper_release: Some(noop),
            directory_record: Some(directory::directory_record),
            directory_srv: Some(directory::directory_srv),
            directory_addresses: Some(directory::directory_addresses),
            directory_notice: Some(directory::directory_notice),
            radio_open: Some(open_radio),
            radio_activate: Some(activate_radio),
            radio_destroy: Some(destroy_radio),
            radio_site_bind: Some(radio_site_bind),
            radio_site_publish: Some(radio_site_publish),
            radio_site_snapshot: Some(radio_site_snapshot),
            peer_dial: Some(peer_dial),
            peer_bind_radio: Some(peer_bind_radio),
            peer_rate: Some(peer_rate),
            peer_ready: Some(peer_ready),
            peer_read: Some(peer_read),
            peer_send_text: Some(peer_send_text),
            peer_send_digit: Some(peer_send_digit),
            peer_write: None,
            peer_destroy: Some(peer_destroy),
            peer_step_attach: Some(peer_step_attach),
            peer_step_stop: Some(peer_step_stop),
            peer_step_detach: Some(peer_step_detach),
            peer_observe: Some(peer_observe),
            peer_prepare_audio: Some(peer_prepare_audio),
            peer_reset_encoder: Some(peer_reset_encoder),
            peer_send_prepared: Some(peer_send_prepared),
            peer_elapsed_ms: Some(peer_elapsed_ms),
            peer_step_wake: Some(peer_step_wake),
            peer_set_deadline: Some(peer_set_deadline),
            scheduler_notice: Some(scheduler_notice),
            peer_info: Some(peer_info),
            peer_read_source: Some(peer_read_source),
        });
        Ok(Self {
            context,
            descriptor,
        })
    }

    /// Borrow the descriptor; this owner and its provider libraries must remain live through stop.
    pub fn descriptor(&self) -> &abi::rptadv_host_services_v9 {
        let _keep_context_alive = &self.context;
        self.descriptor.as_ref()
    }

    /// Prepare resolved accounts while their record spans are borrowed. Store
    /// stable owners before product startup; activation is a separate phase.
    /// # Safety
    /// Product inspection and the preparation's backend code remain loaded
    /// through successful product/account shutdown.
    pub(crate) unsafe fn prepare_echo(
        &mut self,
        product: &abi::rptadv_product_descriptor_v1,
        configuration: &str,
        mut preparation: Preparation<'_>,
    ) -> Result<(), rptadv_peer_support::echolink::host::Error> {
        use rptadv_peer_support::echolink::host::Error;
        let inspect = product.inspect_configuration.ok_or(Error::Contract)?;
        let result = unsafe {
            inspect(
                configuration.as_ptr().cast(),
                configuration.len(),
                Some(prepare_account),
                ptr::from_mut(&mut preparation).cast(),
                None,
                ptr::null_mut(),
            )
        };
        if result != 0 {
            return Err(Error::Contract);
        }
        *self
            .context
            .echo
            .get_mut()
            .unwrap_or_else(|error| error.into_inner()) = preparation.finish().map(Arc::new);
        Ok(())
    }

    /// Start accounts only after their captured product target is callable.
    /// # Safety
    /// Call off executors; retain product/backend code through stop and drain.
    pub(crate) unsafe fn activate_echo(
        &self,
    ) -> Result<(), rptadv_peer_support::echolink::host::Error> {
        if let Some(accounts) = self.context.echo_accounts() {
            unsafe { accounts.activate() }?;
        }
        Ok(())
    }

    /// Gate producers and drain accepted admissions before stopping the product.
    pub(crate) fn stop_echo(&self) -> Result<(), rptadv_peer_support::echolink::host::Error> {
        if let Some(accounts) = self.context.echo_accounts() {
            accounts.stop()?;
        }
        Ok(())
    }

    /// Put one peer handle into the representation used by the host-services callbacks.
    pub fn peer_handle(&self, peer: IaxPeer, local: &str, remote: &str) -> *mut c_void {
        Box::into_raw(Box::new(PeerHandle::Iax(IaxHandle::new(
            peer, local, remote,
        ))))
        .cast()
    }

    /// Destroy an unconsumed peer handle with the host's normal ownership rule.
    pub fn destroy_peer(&self, peer: *mut c_void) {
        if !peer.is_null() {
            unsafe { drop(Box::from_raw(peer.cast::<PeerHandle>())) };
        }
    }

    /// Start listener routing on the same executor as established peer handlers.
    pub(crate) fn enable_listener(
        &self,
        listener: &mut crate::iax::IaxServer,
    ) -> Result<(), IaxError> {
        if let Some(ingress) = &self.context.ingress {
            listener.enable_async(&ingress.executor)?;
        }
        Ok(())
    }

    /// Stage radio settings while keeping the active set available for rollback.
    pub fn stage_radios(&self, radios: Vec<crate::ResolvedRadioNode>) -> bool {
        self.context.radio.stage_radios(radios)
    }

    /// Make successfully opened candidate radios active after product reload succeeds.
    pub fn commit_radios(&self) {
        self.context.radio.commit_radios();
    }

    /// Discard candidate radio settings after reload failure.
    pub fn discard_staged_radios(&self) {
        self.context.radio.discard_staged_radios();
    }
}

impl StandaloneHostContext {
    fn echo_accounts(&self) -> Option<Arc<Accounts>> {
        self.echo
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

unsafe extern "C" fn prepare_account(
    context: *mut c_void,
    node: *const abi::rptadv_node_host_configuration,
) -> i32 {
    boundary(-1, || {
        let Some(node) = (unsafe { node.as_ref() }) else {
            return -1;
        };
        if node.struct_size as usize != size_of::<abi::rptadv_node_host_configuration>() {
            return -1;
        }
        let Some(config) = (unsafe { node.echolink.cast::<echo::AccountConfig>().as_ref() }) else {
            return -1;
        };
        let Some(path) = (unsafe {
            input(
                node.echolink_secrets_file.cast(),
                node.echolink_secrets_file_length,
            )
        }) else {
            return -1;
        };
        let preparation = unsafe { &mut *context.cast::<Preparation<'_>>() };
        unsafe {
            preparation.prepare(
                config,
                (!path.is_empty()).then(|| std::path::Path::new(path)),
            )
        }
        .map_or(-1, |()| 0)
    })
}

/// Transfer an already claimed incoming QSO using this host's actual enum layout.
/// # Safety
/// Product code remains callable through the account's admission-drain barrier.
pub(crate) unsafe fn incoming_echo(
    incoming: unsafe extern "C" fn(*const abi::rptadv_incoming_peer_v1, *mut c_void) -> i32,
    peer: Arc<rptadv_peer_support::echolink::host::Peer>,
) {
    let Some(source) = peer.identity().source else {
        return;
    };
    let source = source.ip().to_string();
    let bytes = |text: &str| abi::rptadv_peer_bytes_v1 {
        data: text.as_ptr(),
        length: text.len(),
    };
    let request = abi::rptadv_incoming_peer_v1 {
        struct_size: size_of::<abi::rptadv_incoming_peer_v1>() as u32,
        abi_version: 1,
        protocol: 2,
        local: bytes(&peer.identity().local),
        remote: bytes(peer.remote()),
        source: bytes(&source),
    };
    let handle = Box::into_raw(Box::new(PeerHandle::Echo(peer))).cast();
    if unsafe { incoming(&request, handle) } > 0 {
        unsafe { drop(Box::from_raw(handle.cast::<PeerHandle>())) };
    }
}

type PeerHandle = HostPeer<IaxHandle>;
// Native tokens belong to the existing IAX executor; Echo tokens retain their
// shared provider/QSO owner. Never pass one protocol's token to another runtime.
enum PeerRegistration {
    Iax(*mut c_void),
    Echo(Registration),
}
struct IaxHandle(IaxPeer, Vec<f32>, String, String);
impl IaxHandle {
    fn new(peer: IaxPeer, local: &str, remote: &str) -> Self {
        Self(peer, vec![0.0; 4096], local.into(), remote.into())
    }
}

// Only the host's unique serial owner lends mutable native state. Echo handles
// are never cast to IAX state, including legacy-only operations called in error.
unsafe fn iax_peer_mut<'a>(peer: *mut c_void) -> Option<&'a mut IaxHandle> {
    match unsafe { peer.cast::<PeerHandle>().as_mut() }? {
        HostPeer::Iax(peer) => Some(peer),
        HostPeer::Echo(_) => None,
    }
}
unsafe fn iax_peer_ref<'a>(peer: *const c_void) -> Option<&'a IaxHandle> {
    match unsafe { peer.cast::<PeerHandle>().as_ref() }? {
        HostPeer::Iax(peer) => Some(peer),
        HostPeer::Echo(_) => None,
    }
}

unsafe extern "C" fn peer_info(
    context: *mut c_void,
    peer: *const c_void,
    output: *mut abi::rptadv_peer_info_v1,
) -> i32 {
    boundary(-1, || {
        let (Some(context), Some(peer), Some(output)) = (
            unsafe { context.cast::<StandaloneHostContext>().as_ref() },
            unsafe { peer.cast::<PeerHandle>().as_ref() },
            unsafe { output.as_mut() },
        ) else {
            return -1;
        };
        if output.struct_size < size_of::<abi::rptadv_peer_info_v1>() as u32
            || output.abi_version != 1
        {
            return -1;
        }
        let (protocol, capabilities, account_generation, qso_generation, remote, local, callsign) =
            match peer {
                HostPeer::Iax(peer) => (
                    1,
                    if context.ingress.is_some() { 15 } else { 12 },
                    0,
                    0,
                    peer.3.as_str(),
                    peer.2.as_str(),
                    "",
                ),
                HostPeer::Echo(peer) => {
                    let identity = peer.identity();
                    (
                        2,
                        identity.host_capabilities(),
                        identity.account_generation,
                        identity.qso_generation,
                        peer.remote(),
                        identity.local.as_str(),
                        identity.callsign.as_str(),
                    )
                }
            };
        let bytes = |text: &str| abi::rptadv_peer_bytes_v1 {
            data: if text.is_empty() {
                ptr::null()
            } else {
                text.as_ptr()
            },
            length: text.len(),
        };
        *output = abi::rptadv_peer_info_v1 {
            struct_size: size_of::<abi::rptadv_peer_info_v1>() as u32,
            abi_version: 1,
            protocol,
            capabilities,
            account_generation,
            qso_generation,
            remote: bytes(remote),
            callsign: bytes(callsign),
            local: bytes(local),
        };
        0
    })
}

unsafe extern "C" fn peer_step_attach(
    context: *mut c_void,
    peer: *mut c_void,
    step: abi::rptadv_peer_step_v1,
    step_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        if let Some(HostPeer::Echo(peer)) = unsafe { peer.cast::<PeerHandle>().as_ref() } {
            // Copy retention before attach, which may immediately reenter this handle.
            let peer = std::sync::Arc::clone(peer);
            return match unsafe { peer.attach(step, step_context) } {
                Ok(token) => {
                    *output = Box::into_raw(Box::new(PeerRegistration::Echo(token))).cast();
                    0
                }
                Err(_) => -1,
            };
        }
        let (Some(context), Some(step)) = (
            unsafe { context.cast::<StandaloneHostContext>().as_ref() },
            step,
        ) else {
            return -1;
        };
        let Some(ingress) = &context.ingress else {
            return -1;
        };
        // End the wrapper borrow before attach: its callback may immediately read it.
        let raw_peer = {
            let Some(peer) = (unsafe { iax_peer_ref(peer) }) else {
                return -1;
            };
            peer.0.raw_handle()
        };
        if unsafe {
            ingress
                .executor
                .configure_jitter(raw_peer, ptr::from_ref(ingress.jitter).cast())
        }
        .is_err()
        {
            return -1;
        }
        match unsafe { ingress.executor.attach(raw_peer, step, step_context) } {
            Ok(token) => {
                *output = Box::into_raw(Box::new(PeerRegistration::Iax(token))).cast();
                0
            }
            Err(_) => -1,
        }
    })
}

unsafe extern "C" fn peer_step_stop(context: *mut c_void, token: *mut c_void) {
    match unsafe { token.cast::<PeerRegistration>().as_ref() } {
        Some(PeerRegistration::Echo(token)) => token.stop(),
        Some(PeerRegistration::Iax(token)) => {
            if let Some(ingress) = unsafe { context.cast::<StandaloneHostContext>().as_ref() }
                .and_then(|context| context.ingress.as_ref())
            {
                unsafe { ingress.executor.stop(*token) };
            }
        }
        None => {}
    }
}

unsafe extern "C" fn peer_observe(
    context: *mut c_void,
    peer: *mut c_void,
    output: *mut abi::rptadv_peer_observation_v1,
) -> i32 {
    boundary(-1, || {
        let executor = unsafe { context.cast::<StandaloneHostContext>().as_ref() }
            .and_then(|context| context.ingress.as_ref().map(|ingress| &ingress.executor));
        let peer = unsafe { iax_peer_ref(peer) };
        match (executor, peer) {
            (Some(executor), Some(peer)) => unsafe {
                executor.observe(peer.0.raw_handle(), output.cast())
            },
            _ => -1,
        }
    })
}

unsafe extern "C" fn peer_step_detach(context: *mut c_void, token: *mut c_void) {
    if token.is_null() {
        return;
    }
    let token = unsafe { Box::from_raw(token.cast::<PeerRegistration>()) };
    match *token {
        PeerRegistration::Echo(token) => drop(token),
        PeerRegistration::Iax(token) => {
            if let Some(ingress) = unsafe { context.cast::<StandaloneHostContext>().as_ref() }
                .and_then(|context| context.ingress.as_ref())
            {
                unsafe { ingress.executor.detach(token) };
            }
        }
    }
}

pub(crate) fn boundary<T>(fallback: T, operation: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(fallback)
}

pub(crate) unsafe fn input<'a>(pointer: *const c_char, length: usize) -> Option<&'a str> {
    if length > isize::MAX as usize || (pointer.is_null() && length != 0) {
        return None;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(pointer.cast(), length) }
    };
    std::str::from_utf8(bytes).ok()
}

unsafe extern "C" fn noop() {}
unsafe extern "C" fn scheduler_notice(_: *mut c_void, text: *const c_char, length: usize) {
    boundary((), || {
        if let Some(text) = unsafe { input(text, length) } {
            eprintln!("rpt-advanced: {text}");
        }
    });
}

unsafe extern "C" fn command_notice(
    _: *mut c_void,
    local: *const c_char,
    length: usize,
    completed: u32,
) {
    boundary((), || {
        if let Some(local) = unsafe { input(local, length) } {
            eprintln!(
                "node {local}: link command {}",
                if completed != 0 {
                    "completed"
                } else {
                    "rejected"
                }
            );
        }
    });
}

unsafe extern "C" fn local_time(
    _: *mut c_void,
    unix_seconds: i64,
    result: *mut abi::rptadv_local_time_v1,
) -> i32 {
    boundary(-1, || {
        let Some(result) = (unsafe { result.as_mut() }) else {
            return -1;
        };
        if result.struct_size < size_of::<abi::rptadv_local_time_v1>() as u32 {
            return -1;
        }
        let timestamp = unix_seconds as libc::time_t;
        let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
        if unsafe { libc::localtime_r(&timestamp, local.as_mut_ptr()) }.is_null() {
            result.valid = 0;
            return 0;
        }
        let local = unsafe { local.assume_init() };
        result.year = (local.tm_year + 1900) as u16;
        result.month = (local.tm_mon + 1) as u8;
        result.day = local.tm_mday as u8;
        result.weekday = local.tm_wday as u8;
        result.hour = local.tm_hour as u8;
        result.minute = local.tm_min as u8;
        result.second = local.tm_sec as u8;
        result.valid = 1;
        0
    })
}

unsafe extern "C" fn open_radio(
    context: *mut c_void,
    name: *const c_char,
    name_length: usize,
    maximum_frames: usize,
    radio: *mut *mut c_void,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    // Access only the radio field; other host callbacks may concurrently read secrets.
    let radio_context =
        unsafe { ptr::addr_of_mut!((*context.cast::<StandaloneHostContext>()).radio) }.cast();
    unsafe { radio_open(radio_context, name, name_length, maximum_frames, radio) }
}

unsafe extern "C" fn activate_radio(
    context: *mut c_void,
    radio: *mut c_void,
    receive: abi::rptadv_radio_receive_v2,
    receive_context: *mut c_void,
    transmit: abi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    if context.is_null() {
        return -1;
    }
    let radio_context =
        unsafe { ptr::addr_of_mut!((*context.cast::<StandaloneHostContext>()).radio) }.cast();
    unsafe {
        radio_activate(
            radio_context,
            radio,
            receive,
            receive_context,
            transmit,
            transmit_context,
        )
    }
}

unsafe extern "C" fn destroy_radio(context: *mut c_void, radio: *mut c_void) {
    if context.is_null() {
        return;
    }
    let radio_context =
        unsafe { ptr::addr_of_mut!((*context.cast::<StandaloneHostContext>()).radio) }.cast();
    unsafe { radio_destroy(radio_context, radio) };
}

unsafe extern "C" fn peer_dial(
    context: *mut c_void,
    destination: *const c_char,
    destination_length: usize,
    local: *const c_char,
    local_length: usize,
    _: usize,
    current: abi::rptadv_current_v1,
    current_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(context) = (unsafe { context.cast::<StandaloneHostContext>().as_ref() }) else {
            return -1;
        };
        let (Some(destination), Some(local), Some(current), Some(output)) = (
            unsafe { input(destination, destination_length) },
            unsafe { input(local, local_length) },
            current,
            unsafe { output.as_mut() },
        ) else {
            return -1;
        };
        *output = ptr::null_mut();
        if unsafe { current(current_context) } == 0 {
            return -1;
        }
        if let Some(remote) = destination.strip_prefix("EchoLink/") {
            let peer = remote
                .parse::<u32>()
                .ok()
                .and_then(|node| context.echo_accounts()?.account(local)?.dial(node).ok());
            let Some(peer) = peer else {
                return -1;
            };
            if unsafe { current(current_context) } == 0 {
                return -1;
            }
            *output = Box::into_raw(Box::new(PeerHandle::Echo(peer))).cast();
            return 0;
        }
        let Some((address, remote)) = parse_destination(destination, |endpoint| {
            context
                .resolver
                .as_ref()?
                .addresses(endpoint)
                .ok()?
                .into_iter()
                .next()
        }) else {
            eprintln!("rpt-advanced: IAX2 destination validation or address resolution failed");
            return -1;
        };
        let Some(ingress) = &context.ingress else {
            return -1;
        };
        let peer = match ingress.client.clone().dial_configured(
            address,
            local,
            &remote,
            &context.secrets,
            5000,
        ) {
            Ok(peer) => peer,
            Err(error) => {
                eprintln!("rpt-advanced: IAX2 outbound dial failed: {error}");
                return -1;
            }
        };
        if unsafe { current(current_context) } == 0 {
            drop(peer);
            return -1;
        }
        *output = Box::into_raw(Box::new(PeerHandle::Iax(IaxHandle::new(
            peer, local, &remote,
        ))))
        .cast();
        0
    })
}

#[cfg(test)]
fn dial_peer(
    library: &std::path::Path,
    address: SocketAddr,
    local: &str,
    remote: &str,
    secrets: &SecretsFile,
    timeout_ms: u32,
) -> Result<IaxPeer, IaxError> {
    IaxClient::load(library)?.dial_configured(address, local, remote, secrets, timeout_ms)
}

fn parse_destination(
    value: &str,
    resolve: impl FnOnce(&str) -> Option<SocketAddr>,
) -> Option<(SocketAddr, String)> {
    let (address, remote) = value.strip_prefix("radio@")?.rsplit_once('/')?;
    if remote.is_empty() || remote.len() > 63 || !remote.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let address: SocketAddr = if let Ok(address) = address.parse() {
        address
    } else if let Ok(ip) = address
        .strip_prefix('[')
        .and_then(|address| address.strip_suffix(']'))
        .unwrap_or(address)
        .parse::<std::net::IpAddr>()
    {
        SocketAddr::new(ip, 4569)
    } else if address.contains(':') {
        resolve(address)?
    } else {
        resolve(&format!("{address}:4569"))?
    };
    (address.port() != 0).then(|| (address, remote.to_owned()))
}

unsafe extern "C" fn peer_rate(_: *mut c_void, peer: *const c_void) -> u32 {
    match unsafe { peer.cast::<PeerHandle>().as_ref() } {
        Some(HostPeer::Echo(_)) => 8000,
        Some(HostPeer::Iax(peer)) => peer.0.sample_rate_hz(),
        None => 0,
    }
}

unsafe extern "C" fn peer_ready(_: *mut c_void, peer: *mut c_void) -> i32 {
    if unsafe { iax_peer_ref(peer) }.is_none() {
        return -1;
    }
    1
}

unsafe extern "C" fn peer_read(
    _: *mut c_void,
    peer: *mut c_void,
    event: abi::rptadv_peer_event_v1,
    event_context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(event)) = (unsafe { iax_peer_mut(peer) }, event) else {
            return -1;
        };
        // Full legal datagram text remains deliverable; local synthesis has its own limit.
        let mut text = [0_u8; 65507];
        let mut input = peer.0.poll(&mut peer.1, &mut text);
        if let Ok(IaxEvent::NeedCapacity(required)) = input {
            if peer.1.try_reserve_exact(required - peer.1.len()).is_err() {
                return -1;
            }
            peer.1.resize(required, 0.0);
            // Native decode/PLC was retained, not consumed; retry once on this owner.
            input = peer.0.poll(&mut peer.1, &mut text);
        }
        match input {
            Ok(IaxEvent::None) => 0,
            Ok(IaxEvent::Audio(length)) => unsafe {
                event(event_context, 3, peer.1.as_ptr().cast(), length);
                0
            },
            Ok(IaxEvent::Gap(length)) => unsafe {
                event(event_context, 6, ptr::null(), length);
                0
            },
            Ok(IaxEvent::Text(length)) => unsafe {
                event(event_context, 1, text.as_ptr().cast(), length);
                0
            },
            Ok(IaxEvent::Digit(digit)) => unsafe {
                event(event_context, 2, ptr::from_ref(&digit).cast(), 1);
                0
            },
            Ok(IaxEvent::RadioKey) => unsafe {
                event(event_context, 4, ptr::null(), 0);
                0
            },
            Ok(IaxEvent::RadioUnkey) => unsafe {
                event(event_context, 5, ptr::null(), 0);
                0
            },
            Ok(IaxEvent::Hangup | IaxEvent::NeedCapacity(_)) | Err(_) => -1,
        }
    })
}

unsafe extern "C" fn peer_send_text(
    _: *mut c_void,
    peer: *mut c_void,
    text: *const c_char,
    length: usize,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(text)) = (unsafe { iax_peer_mut(peer) }, unsafe {
            input(text, length)
        }) else {
            return -1;
        };
        if peer.0.send_text(text.as_bytes()).is_ok() {
            0
        } else {
            -1
        }
    })
}

unsafe extern "C" fn peer_send_digit(_: *mut c_void, peer: *mut c_void, digit: u8) -> i32 {
    unsafe { iax_peer_mut(peer) }.map_or(-1, |peer| {
        if peer.0.send_digit(digit).is_ok() {
            0
        } else {
            -1
        }
    })
}

unsafe extern "C" fn peer_destroy(_: *mut c_void, peer: *mut c_void) {
    if !peer.is_null() {
        unsafe { drop(Box::from_raw(peer.cast::<PeerHandle>())) };
    }
}

// Prepared media stays in the product owner across socket congestion; only IAX
// owns reliable sequence numbers and committed retransmission bytes.
unsafe extern "C" fn peer_prepare_audio(
    _: *mut c_void,
    peer: *mut c_void,
    pcm: *const f32,
    count: usize,
    output: *mut u8,
    capacity: usize,
    flush: u32,
    result: *mut abi::rptadv_codec_encode_result_v1,
) -> i32 {
    boundary(-1, || {
        if result.is_null()
            || unsafe { ptr::addr_of!((*result).struct_size).read() }
                < size_of::<abi::rptadv_codec_encode_result_v1>() as u32
            || count > isize::MAX as usize / size_of::<f32>()
            || capacity > isize::MAX as usize
            || (count != 0 && pcm.is_null())
            || (capacity != 0 && output.is_null())
        {
            return -1;
        }
        let pcm = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(pcm, count) }
        };
        let output = if capacity == 0 {
            &mut []
        } else {
            unsafe { std::slice::from_raw_parts_mut(output, capacity) }
        };
        match unsafe { peer.cast::<PeerHandle>().as_ref() } {
            Some(HostPeer::Echo(peer)) => {
                unsafe { peer.prepare_audio(pcm, output, flush, result.cast()) }
                    .map_or(-1, |()| unsafe { (*result).status as i32 })
            }
            Some(HostPeer::Iax(_)) => {
                unsafe { iax_peer_mut(peer) }
                    .unwrap()
                    .0
                    .prepare_audio(pcm, output, flush, unsafe { &mut *result })
            }
            None => -1,
        }
    })
}
unsafe extern "C" fn peer_reset_encoder(_: *mut c_void, peer: *mut c_void) -> i32 {
    boundary(-1, || {
        if let Some(HostPeer::Echo(peer)) = unsafe { peer.cast::<PeerHandle>().as_ref() } {
            return unsafe { peer.reset_encoder() }.map_or(-1, |()| 0);
        }
        unsafe { iax_peer_mut(peer) }.map_or(-1, |peer| peer.0.reset_encoder())
    })
}
unsafe extern "C" fn peer_send_prepared(
    _: *mut c_void,
    peer: *mut c_void,
    bytes: *const u8,
    count: usize,
    frames: u32,
    timestamp: u32,
) -> i32 {
    boundary(-1, || {
        if count > isize::MAX as usize || (bytes.is_null() && count != 0) {
            return -1;
        }
        let bytes = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(bytes, count) }
        };
        let result = match unsafe { peer.cast::<PeerHandle>().as_ref() } {
            Some(HostPeer::Echo(peer)) => {
                unsafe { peer.send_prepared(bytes, frames, timestamp) }.map_err(|_| ())
            }
            Some(HostPeer::Iax(_)) => unsafe { iax_peer_mut(peer) }
                .unwrap()
                .0
                .send_prepared(bytes, frames, timestamp)
                .map_err(|_| ()),
            None => return -1,
        };
        match result {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn peer_elapsed_ms(_: *mut c_void, peer: *mut c_void) -> u32 {
    if let Some(HostPeer::Echo(peer)) = unsafe { peer.cast::<PeerHandle>().as_ref() } {
        return peer
            .elapsed_ms()
            .map_or(0, |value| value.min(u64::from(u32::MAX)) as u32);
    }
    unsafe { iax_peer_ref(peer) }.map_or(0, |peer| peer.0.elapsed_ms())
}
unsafe extern "C" fn peer_step_wake(context: *mut c_void, registration: *mut c_void) {
    match unsafe { registration.cast::<PeerRegistration>().as_ref() } {
        Some(PeerRegistration::Echo(token)) => token.wake(),
        Some(PeerRegistration::Iax(token)) => {
            if let Some(ingress) = unsafe { context.cast::<StandaloneHostContext>().as_ref() }
                .and_then(|context| context.ingress.as_ref())
            {
                unsafe { ingress.executor.wake(*token) };
            }
        }
        None => {}
    }
}
unsafe extern "C" fn peer_set_deadline(context: *mut c_void, peer: *mut c_void, after: u64) -> i32 {
    if let Some(HostPeer::Echo(peer)) = unsafe { peer.cast::<PeerHandle>().as_ref() } {
        return unsafe { peer.set_deadline(after) }.map_or(-1, |()| 0);
    }
    let (Some(context), Some(peer)) = (
        unsafe { context.cast::<StandaloneHostContext>().as_ref() },
        unsafe { iax_peer_ref(peer) },
    ) else {
        return -1;
    };
    context.ingress.as_ref().map_or(-1, |ingress| unsafe {
        ingress.executor.set_deadline(peer.0.raw_handle(), after)
    })
}

unsafe extern "C" fn peer_read_source(
    _: *mut c_void,
    peer: *mut c_void,
    completion: u32,
    sink: *const abi::rptadv_peer_source_sink_v1,
    text: abi::rptadv_peer_source_text_v1,
    text_context: *mut c_void,
    output: *mut abi::rptadv_peer_source_result_v1,
) -> i32 {
    boundary(-1, || {
        let (Some(HostPeer::Echo(peer)), Some(output)) =
            (unsafe { peer.cast::<PeerHandle>().as_ref() }, unsafe {
                output.as_mut()
            })
        else {
            return -1;
        };
        if output.struct_size < size_of::<abi::rptadv_peer_source_result_v1>() as u32
            || output.abi_version != 1
        {
            return -1;
        }
        let sink = unsafe { sink.as_ref() };
        if sink.is_some_and(|sink| {
            sink.struct_size < size_of::<abi::rptadv_peer_source_sink_v1>() as u32
                || sink.abi_version != 1
        }) {
            return -1;
        }
        let sink = sink.map(|sink| echo::PcmSink {
            struct_size: size_of::<echo::PcmSink>() as u32,
            abi_version: 1,
            context: sink.context,
            audio: sink.audio,
            gap: sink.gap,
        });
        struct Text {
            callback: abi::rptadv_peer_source_text_v1,
            context: *mut c_void,
        }
        unsafe extern "C" fn forward(
            context: *mut c_void,
            sender: echo::ByteSpan,
            body: echo::ByteSpan,
        ) {
            let text = unsafe { &*context.cast::<Text>() };
            if let Some(callback) = text.callback {
                unsafe {
                    callback(
                        text.context,
                        abi::rptadv_peer_bytes_v1 {
                            data: sender.data,
                            length: sender.length,
                        },
                        abi::rptadv_peer_bytes_v1 {
                            data: body.data,
                            length: body.length,
                        },
                    )
                };
            }
        }
        let mut text = Text {
            callback: text,
            context: text_context,
        };
        match unsafe {
            peer.read(
                completion,
                sink.as_ref(),
                Some(forward),
                ptr::from_mut(&mut text).cast(),
            )
        } {
            Ok(result) => {
                output.progress = result.progress;
                output.activity = result.activity;
                output.finish_pending = result.finish_pending;
                output.state = result.state;
                0
            }
            Err(_) => -1,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{HostServicesOwner, boundary, dial_peer, input, peer_dial};
    use crate::{iax::IaxError, secrets::SecretsFile};
    use std::{
        ffi::c_void,
        mem::size_of,
        net::{SocketAddr, UdpSocket},
        path::Path,
        ptr,
        sync::atomic::{AtomicUsize, Ordering},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    unsafe extern "C" fn current_no(_: *mut c_void) -> u32 {
        0
    }

    unsafe extern "C" fn current_yes(_: *mut c_void) -> u32 {
        1
    }

    unsafe extern "C" fn current_yes_then_no(context: *mut c_void) -> u32 {
        let Some(calls) = (unsafe { context.cast::<AtomicUsize>().as_ref() }) else {
            return 0;
        };
        u32::from(calls.fetch_add(1, Ordering::AcqRel) == 0)
    }

    unsafe extern "C" fn record_peer_event(
        context: *mut c_void,
        kind: u32,
        _: *const c_void,
        _: usize,
    ) {
        unsafe { context.cast::<Vec<u32>>().as_mut() }
            .unwrap()
            .push(kind);
    }

    unsafe extern "C" fn record_peer_text(
        context: *mut c_void,
        kind: u32,
        data: *const c_void,
        length: usize,
    ) {
        if kind == 1 {
            // SAFETY: peer_read provides a readable payload for this synchronous callback.
            let text = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
            unsafe { context.cast::<Vec<Vec<u8>>>().as_mut() }
                .unwrap()
                .push(text.to_vec());
        }
    }

    unsafe extern "C" fn allow_test_peer(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn reject_test_peer(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
    ) -> i32 {
        1
    }

    unsafe extern "C" fn capture_test_peer(
        context: *mut c_void,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        _: *const u8,
        _: usize,
        peer: *mut c_void,
    ) -> i32 {
        let Some(context) = (unsafe { context.cast::<AtomicUsize>().as_ref() }) else {
            return -1;
        };
        if peer.is_null() {
            return -1;
        }
        context.store(peer as usize, Ordering::Release);
        0
    }

    fn parse_destination(value: &str) -> Option<(SocketAddr, String)> {
        super::parse_destination(value, |endpoint| match endpoint {
            "localhost:4569" => Some("127.0.0.1:4569".parse().unwrap()),
            "localhost:4571" => Some("[::1]:4571".parse().unwrap()),
            _ => None,
        })
    }

    #[test]
    fn destination_resolution_uses_provider_only_for_names() {
        for target in [
            "radio@127.0.0.1/1",
            "radio@[::ffff:127.0.0.1]:4569/1",
            "radio@[::1]/1",
        ] {
            assert!(
                super::parse_destination(target, |_| panic!("numeric address reached DNS"))
                    .is_some()
            );
        }
        let expected: SocketAddr = "[::ffff:192.0.2.1]:4569".parse().unwrap();
        assert_eq!(
            super::parse_destination("radio@test.invalid/1", |endpoint| {
                assert_eq!(endpoint, "test.invalid:4569");
                Some(expected)
            }),
            Some((expected, "1".into()))
        );
        assert!(
            super::parse_destination("radio@test.invalid:4571/1", |endpoint| {
                assert_eq!(endpoint, "test.invalid:4571");
                None
            })
            .is_none()
        );
    }

    #[test]
    fn scheduler_notice_writes_valid_borrowed_text_to_stderr() {
        const CHILD: &str = "RPTADV_SCHEDULER_NOTICE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let notice = super::scheduler_notice;
            let message = "scheduler queue 50% — Δ;ignored";
            unsafe {
                notice(ptr::null_mut(), message.as_ptr().cast(), 26);
                for (text, length) in [
                    (ptr::null(), 1),
                    (c"\xff".as_ptr(), 1),
                    (c"x".as_ptr(), usize::MAX),
                ] {
                    notice(ptr::null_mut(), text, length);
                }
            }
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "host_services::tests::scheduler_notice_writes_valid_borrowed_text_to_stderr",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "rpt-advanced: scheduler queue 50% — Δ\n"
        );
    }

    #[test]
    fn host_services_table_retains_its_owned_context() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let api = owner.descriptor();
        assert_eq!(
            api.struct_size,
            size_of::<crate::abi::rptadv_host_services_v9>() as u32
        );
        assert_eq!(api.abi_version, 9);
        assert_eq!(api.capability, *b"rptadv.hst9\0");
        assert!(!api.context.is_null());
        assert!(api.local_time.is_some());
        assert!(api.peer_dial.is_some());
        assert!(api.radio_activate.is_some());
        let _context: *mut c_void = api.context;
    }

    #[test]
    fn descriptor_address_survives_owner_move() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let before = ptr::from_ref(owner.descriptor());
        let owner = Box::new(owner);
        let after = ptr::from_ref(owner.descriptor());

        assert_eq!(before, after);
    }

    #[test]
    fn async_callbacks_reject_absent_context_and_disabled_backend() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let api = owner.descriptor();
        assert!(api.peer_step_attach.is_some());
        assert!(api.peer_step_stop.is_some());
        assert!(api.peer_step_detach.is_some());
        assert!(api.peer_observe.is_some());
        assert!(
            api.peer_prepare_audio.is_some()
                && api.peer_send_prepared.is_some()
                && api.peer_elapsed_ms.is_some()
                && api.peer_step_wake.is_some()
                && api.peer_set_deadline.is_some()
        );
        unsafe {
            super::peer_step_wake(ptr::null_mut(), ptr::null_mut());
            super::peer_step_wake(api.context, ptr::null_mut());
        }
        assert_eq!(
            unsafe { super::peer_set_deadline(ptr::null_mut(), ptr::null_mut(), u64::MAX) },
            -1
        );
        let peer = owner.peer_handle(
            crate::iax::tests::peer_for_host_callback(0, false, false),
            "1000",
            "2000",
        );
        assert_eq!(
            unsafe { super::peer_set_deadline(api.context, peer, 0) },
            -1
        );
        owner.destroy_peer(peer);
        unsafe extern "C" fn step(_: *mut c_void) -> i32 {
            0
        }
        let mut token = std::ptr::dangling_mut();
        for context in [ptr::null_mut(), api.context] {
            for callback in [None, Some(step as unsafe extern "C" fn(*mut c_void) -> i32)] {
                assert_eq!(
                    unsafe {
                        super::peer_step_attach(
                            context,
                            ptr::null_mut(),
                            callback,
                            ptr::null_mut(),
                            &mut token,
                        )
                    },
                    -1
                );
                assert!(token.is_null());
            }
            unsafe {
                super::peer_step_stop(context, ptr::null_mut());
                super::peer_step_detach(context, ptr::null_mut());
            }
            assert_eq!(
                unsafe { super::peer_observe(context, ptr::null_mut(), ptr::null_mut()) },
                -1
            );
        }
        assert_eq!(
            unsafe {
                super::peer_step_attach(
                    api.context,
                    ptr::null_mut(),
                    Some(step),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            -1
        );
    }

    #[test]
    fn host_owner_rejects_a_manifest_without_its_resolver() {
        let providers = Box::leak(Box::new(crate::providers::tests::empty_provider_set()));
        assert_eq!(
            HostServicesOwner::new(Vec::new(), Some(providers), SecretsFile::parse("").unwrap())
                .err(),
            Some(crate::providers::ProviderError::MissingDescriptor(
                "librptadv_resolver_adapter.so.1"
            ))
        );
    }

    #[test]
    fn callback_input_and_panic_boundary_return_safe_fallbacks() {
        let invalid_utf8 = [0xff_u8];
        assert_eq!(unsafe { input(ptr::null(), 0) }, Some(""));
        assert_eq!(unsafe { input(ptr::null(), 1) }, None);
        assert_eq!(unsafe { input(invalid_utf8.as_ptr().cast(), 1) }, None);
        assert_eq!(unsafe { input(ptr::null(), usize::MAX) }, None);
        assert_eq!(boundary(-1, || panic!("test callback panic")), -1);
    }

    #[test]
    fn local_time_validates_output_and_returns_local_calendar_fields() {
        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), 0, ptr::null_mut()) },
            -1
        );
        let mut short = crate::abi::rptadv_local_time_v1 {
            struct_size: 0,
            valid: 7,
            year: 0,
            month: 0,
            day: 0,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), 0, &mut short) },
            -1
        );
        assert_eq!(short.valid, 7);

        let mut result = crate::abi::rptadv_local_time_v1 {
            struct_size: size_of::<crate::abi::rptadv_local_time_v1>() as u32,
            valid: 0,
            year: 0,
            month: 0,
            day: 0,
            weekday: 0,
            hour: 0,
            minute: 0,
            second: 0,
        };
        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), 0, &mut result) },
            0
        );
        assert_eq!(result.valid, 1);
        assert!((1..=12).contains(&result.month));
        assert!((1..=31).contains(&result.day));

        assert_eq!(
            unsafe { super::local_time(ptr::null_mut(), i64::MAX, &mut result) },
            0
        );
        assert_eq!(result.valid, 0);
    }

    #[test]
    fn destination_parser_checks_scheme_node_and_address() {
        let (address, node) = parse_destination("radio@localhost:4571/506316")
            .expect("explicit hostname port must resolve");
        assert!(address.ip().is_loopback());
        assert_eq!(address.port(), 4571);
        assert_eq!(node, "506316");
        assert_eq!(
            parse_destination("radio@127.0.0.1:4569/506315"),
            Some(("127.0.0.1:4569".parse().unwrap(), "506315".into()))
        );
        assert!(parse_destination("radio@127.0.0.1/506315").is_some());
        assert_eq!(
            parse_destination("radio@localhost/506315")
                .expect("localhost must resolve through the system resolver")
                .0
                .port(),
            4569
        );
        for (target, expected) in [
            ("radio@[::1]:4571/506315", "[::1]:4571"),
            ("radio@[::1]/506315", "[::1]:4569"),
            ("radio@::1/506315", "[::1]:4569"),
        ] {
            assert_eq!(
                parse_destination(target).unwrap().0,
                expected.parse::<SocketAddr>().unwrap()
            );
        }
        for invalid in [
            "",
            "127.0.0.1/506315",
            "radio@127.0.0.1",
            "radio@127.0.0.1/",
            "radio@127.0.0.1/not-a-node",
            "radio@127.0.0.1/1111111111111111111111111111111111111111111111111111111111111111",
            "radio@localhost:65536/506315",
            "radio@localhost:invalid/506315",
            "radio@127.0.0.1:0/506315",
        ] {
            assert_eq!(parse_destination(invalid), None, "accepted {invalid:?}");
        }
    }

    #[test]
    fn outbound_dial_reports_library_and_network_failures() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let secrets = SecretsFile::parse("").unwrap();
        assert!(matches!(
            dial_peer(
                Path::new("/no/such/librptadviax2.so.2"),
                address,
                "524950",
                "506315",
                &secrets,
                50,
            ),
            Err(IaxError::Load)
        ));
        assert!(matches!(
            dial_peer(
                Path::new("librptadviax2.so.2"),
                address,
                "524950",
                "506315",
                &secrets,
                50,
            ),
            Err(IaxError::Timeout | IaxError::Network)
        ));
    }

    #[test]
    fn outbound_dial_callback_rejects_bad_inputs_and_stale_calls() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let descriptor = owner.descriptor();
        let local = c"524950";
        let destination = c"not-a-radio-destination";
        let mut output = ptr::null_mut();
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    ptr::null(),
                    1,
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_no),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        let valid = c"radio@127.0.0.1:4569/506315";
        assert_eq!(
            unsafe {
                peer_dial(
                    descriptor.context,
                    valid.as_ptr(),
                    valid.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            },
            -1
        );
        assert!(output.is_null());
    }

    #[test]
    fn outbound_dial_callback_returns_failure_when_remote_rejects() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let library = Path::new("librptadviax2.so.2");
        let mut server = crate::iax::IaxServer::bind(
            &crate::iax::IaxClient::load(library).unwrap(),
            address,
            &["506315".to_owned()],
        )
        .unwrap();
        let providers = Box::leak(Box::new(crate::providers::ProviderSet::load().unwrap()));
        let owner =
            HostServicesOwner::new(Vec::new(), Some(providers), SecretsFile::parse("").unwrap())
                .unwrap();
        let context = owner.descriptor().context as usize;
        let destination = format!("radio@{address}/506315");
        let (sender, receiver) = std::sync::mpsc::channel();
        let dial = thread::spawn(move || {
            let destination = std::ffi::CString::new(destination).unwrap();
            let local = c"524950";
            let mut output = ptr::null_mut();
            let status = unsafe {
                super::peer_dial(
                    context as *mut c_void,
                    destination.as_ptr(),
                    destination.as_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut output,
                )
            };
            sender.send((status, output as usize)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(6);
        while !dial.is_finished() && Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(
                        now,
                        Some(reject_test_peer),
                        Some(capture_test_peer),
                        ptr::null_mut(),
                    )
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        let (status, peer) = receiver.try_recv().unwrap();
        assert_eq!(status, -1);
        assert_eq!(peer, 0);
    }

    #[test]
    fn host_callbacks_reject_missing_context_handles_and_outputs() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let api = owner.descriptor();
        let destination = c"radio@127.0.0.1/506315";
        let local = c"524950";
        let mut radio = ptr::null_mut();
        assert_eq!(
            unsafe { super::open_radio(ptr::null_mut(), c"x".as_ptr(), 1, 960, &mut radio) },
            -1
        );
        assert_eq!(
            unsafe { super::open_radio(api.context, c"x".as_ptr(), 1, 960, &mut radio) },
            -1
        );
        assert_eq!(radio, ptr::null_mut());
        assert_eq!(
            unsafe {
                super::activate_radio(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::activate_radio(
                    api.context,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                    None,
                    ptr::null_mut(),
                )
            },
            -1
        );
        unsafe { super::destroy_radio(ptr::null_mut(), ptr::null_mut()) };
        unsafe { super::destroy_radio(api.context, ptr::null_mut()) };

        let mut peer = ptr::null_mut();
        assert_eq!(
            unsafe {
                super::peer_dial(
                    ptr::null_mut(),
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_no),
                    ptr::null_mut(),
                    &mut peer,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::peer_dial(
                    api.context,
                    destination.as_ptr(),
                    destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_no),
                    ptr::null_mut(),
                    &mut peer,
                )
            },
            -1
        );
        assert_eq!(peer, ptr::null_mut());

        let invalid_destination = c"not-an-iax-destination";
        assert_eq!(
            unsafe {
                super::peer_dial(
                    api.context,
                    invalid_destination.as_ptr(),
                    invalid_destination.to_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut peer,
                )
            },
            -1
        );

        assert_eq!(unsafe { super::peer_rate(ptr::null_mut(), ptr::null()) }, 0);
        assert_eq!(
            unsafe { super::peer_ready(ptr::null_mut(), ptr::null_mut()) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_read(ptr::null_mut(), ptr::null_mut(), None, ptr::null_mut()) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), ptr::null_mut(), c"x".as_ptr(), 1) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_digit(ptr::null_mut(), ptr::null_mut(), b'1') },
            -1
        );
        unsafe { super::peer_destroy(ptr::null_mut(), ptr::null_mut()) };
        owner.destroy_peer(ptr::null_mut());
    }

    #[test]
    fn host_callback_notice_and_radio_staging_preserve_safe_state() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        unsafe { super::noop() };
        unsafe { super::command_notice(ptr::null_mut(), c"524950".as_ptr(), 6, 1) };
        unsafe { super::command_notice(ptr::null_mut(), c"524950".as_ptr(), 6, 0) };
        unsafe { super::command_notice(ptr::null_mut(), ptr::null(), 1, 1) };

        assert!(owner.stage_radios(Vec::new()));
        assert!(!owner.stage_radios(Vec::new()));
        owner.commit_radios();
        assert!(owner.stage_radios(Vec::new()));
        owner.discard_staged_radios();
        assert!(owner.stage_radios(Vec::new()));
        owner.discard_staged_radios();
    }

    #[test]
    fn prepared_media_rejects_invalid_buffers_without_touching_output() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let peer = owner.peer_handle(
            crate::iax::tests::peer_for_host_callback(0, false, false),
            "1000",
            "2000",
        );
        let pcm = [0.0_f32];
        let mut output = [0xa5];
        let mut result: crate::abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<crate::abi::rptadv_codec_encode_result_v1>() as u32;
        let output_ptr = output.as_mut_ptr();
        let oversized = isize::MAX as usize / size_of::<f32>() + 1;
        for (handle, samples, count, encoded, capacity) in [
            (ptr::null_mut(), pcm.as_ptr(), 1, output_ptr, 1),
            (peer, pcm.as_ptr(), oversized, output_ptr, oversized),
            (peer, pcm.as_ptr(), 1, output_ptr, usize::MAX),
            (peer, ptr::null(), 1, output_ptr, 1),
            (peer, pcm.as_ptr(), 1, ptr::null_mut(), 1),
        ] {
            assert_eq!(
                unsafe {
                    super::peer_prepare_audio(
                        ptr::null_mut(),
                        handle,
                        samples,
                        count,
                        encoded,
                        capacity,
                        0,
                        &mut result,
                    )
                },
                -1
            );
            assert_eq!(output, [0xa5]);
        }
        result.struct_size = 0;
        for invalid in [ptr::null_mut(), ptr::from_mut(&mut result)] {
            assert_eq!(
                unsafe {
                    super::peer_prepare_audio(
                        ptr::null_mut(),
                        peer,
                        pcm.as_ptr(),
                        1,
                        output_ptr,
                        1,
                        0,
                        invalid,
                    )
                },
                -1
            );
            assert_eq!(output, [0xa5]);
        }
        result.struct_size = size_of::<crate::abi::rptadv_codec_encode_result_v1>() as u32;
        assert_eq!(
            unsafe {
                super::peer_prepare_audio(
                    ptr::null_mut(),
                    peer,
                    ptr::null(),
                    0,
                    ptr::null_mut(),
                    0,
                    1,
                    &mut result,
                )
            },
            0
        );
        for (handle, bytes, count, expected) in [
            (ptr::null_mut(), output.as_ptr(), 1, -1),
            (peer, output.as_ptr(), isize::MAX as usize + 1, -1),
            (peer, ptr::null(), 0, 0),
        ] {
            assert_eq!(
                unsafe {
                    super::peer_send_prepared(
                        ptr::null_mut(),
                        handle,
                        bytes,
                        count,
                        count as u32,
                        1234,
                    )
                },
                expected
            );
        }
        owner.destroy_peer(peer);
    }

    #[test]
    fn host_codec_read_retries_native_plc_capacity_and_accepts_large_text() {
        unsafe extern "C" fn count(
            context: *mut c_void,
            kind: u32,
            _: *const c_void,
            length: usize,
        ) {
            unsafe {
                *context.cast::<(u32, usize)>() = (kind, length);
            }
        }
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        for (input, expected, length) in [
            (1, 3, 6000),
            (2, 1, 4096),
            (1, 0, isize::MAX as usize / size_of::<f32>()),
        ] {
            let peer = owner.peer_handle(
                crate::iax::tests::peer_for_capacity(input, length),
                "1000",
                "2000",
            );
            let mut observed = (0, 0);
            assert_eq!(
                unsafe {
                    super::peer_read(
                        ptr::null_mut(),
                        peer,
                        Some(count),
                        ptr::from_mut(&mut observed).cast(),
                    )
                },
                if expected == 0 { -1 } else { 0 }
            );
            assert_eq!(observed, (expected, if expected == 0 { 0 } else { length }));
            owner.destroy_peer(peer);
        }
    }

    #[test]
    fn peer_callbacks_forward_events_and_report_send_results() {
        let owner =
            HostServicesOwner::new(Vec::new(), None, SecretsFile::parse("").unwrap()).unwrap();
        let events = [
            (0, 0, None),
            (1, 0, Some(3)),
            (2, 0, Some(1)),
            (4, 0, Some(2)),
            (5, 0, Some(4)),
            (6, 0, Some(5)),
            (7, 0, Some(6)),
            (3, -1, None),
        ];
        for (event, result, forwarded) in events {
            let peer = crate::iax::tests::peer_for_host_callback(event, false, false);
            let handle = owner.peer_handle(peer, "1000", "2000");
            let mut observed = Vec::new();
            assert_eq!(
                unsafe {
                    super::peer_read(
                        ptr::null_mut(),
                        handle,
                        Some(record_peer_event),
                        ptr::from_mut(&mut observed).cast(),
                    )
                },
                result
            );
            assert_eq!(observed.first().copied(), forwarded);
            owner.destroy_peer(handle);
        }

        let peer = crate::iax::tests::peer_for_host_callback(0, false, false);
        let handle = owner.peer_handle(peer, "1000", "2000");
        assert_eq!(unsafe { super::peer_rate(ptr::null_mut(), handle) }, 8000);
        assert_eq!(unsafe { super::peer_ready(ptr::null_mut(), handle) }, 1);
        let mut encoded = [0; 160];
        let mut result: crate::abi::rptadv_codec_encode_result_v1 = unsafe { std::mem::zeroed() };
        result.struct_size = size_of::<crate::abi::rptadv_codec_encode_result_v1>() as u32;
        assert_eq!(
            unsafe {
                super::peer_prepare_audio(
                    ptr::null_mut(),
                    handle,
                    [0.0; 160].as_ptr(),
                    160,
                    encoded.as_mut_ptr(),
                    160,
                    0,
                    &mut result,
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                super::peer_prepare_audio(
                    ptr::null_mut(),
                    handle,
                    ptr::null(),
                    0,
                    ptr::null_mut(),
                    0,
                    1,
                    &mut result,
                )
            },
            0
        );
        assert_eq!(
            unsafe {
                super::peer_prepare_audio(
                    ptr::null_mut(),
                    handle,
                    ptr::null(),
                    1,
                    ptr::null_mut(),
                    0,
                    0,
                    &mut result,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::peer_send_prepared(ptr::null_mut(), handle, encoded.as_ptr(), 160, 160, 1234)
            },
            0
        );
        assert_eq!(
            unsafe { super::peer_send_prepared(ptr::null_mut(), handle, ptr::null(), 1, 1, 1234) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_elapsed_ms(ptr::null_mut(), handle) },
            1234
        );
        assert_eq!(
            unsafe { super::peer_reset_encoder(ptr::null_mut(), handle) },
            0
        );
        assert_eq!(
            unsafe { super::peer_reset_encoder(ptr::null_mut(), ptr::null_mut()) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), handle, c"status".as_ptr(), 6) },
            0
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), handle, ptr::null(), 1) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_digit(ptr::null_mut(), handle, b'7') },
            0
        );
        assert!(owner.descriptor().peer_write.is_none());
        owner.destroy_peer(handle);

        let peer = crate::iax::tests::peer_for_host_callback(0, true, true);
        let handle = owner.peer_handle(peer, "1000", "2000");
        assert_eq!(
            unsafe {
                super::peer_send_prepared(ptr::null_mut(), handle, encoded.as_ptr(), 160, 160, 1234)
            },
            1,
            "congestion must remain retryable across both ABIs"
        );
        assert_eq!(
            unsafe {
                super::peer_prepare_audio(
                    ptr::null_mut(),
                    handle,
                    [0.0; 160].as_ptr(),
                    160,
                    encoded.as_mut_ptr(),
                    160,
                    0,
                    &mut result,
                )
            },
            -1
        );
        assert_eq!(
            unsafe {
                super::peer_read(
                    ptr::null_mut(),
                    handle,
                    Some(record_peer_event),
                    ptr::null_mut(),
                )
            },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_text(ptr::null_mut(), handle, c"status".as_ptr(), 6) },
            -1
        );
        assert_eq!(
            unsafe { super::peer_send_digit(ptr::null_mut(), handle, b'7') },
            -1
        );
        unsafe { super::peer_destroy(ptr::null_mut(), handle) };
    }

    #[test]
    fn asynchronous_host_delivers_jittered_audio_and_quiesces_after_detach() {
        host_dial_connects(None);
    }

    #[test]
    fn asynchronous_codec_units_use_real_jitter_and_provider_tables() {
        for profile in [4, 8, 11] {
            host_dial_connects(Some(profile));
        }
    }

    fn host_dial_connects(profile: Option<u32>) {
        unsafe extern "C" fn ordered_addresses(
            _: *mut c_void,
            endpoint: *const u8,
            length: usize,
            sink: crate::abi::rptadv_resolver_address_sink_v1,
            context: *mut c_void,
        ) -> i32 {
            let endpoint = unsafe { input(endpoint.cast(), length) }.unwrap();
            let port = endpoint.strip_prefix("selected.invalid:").unwrap();
            for address in [format!("127.0.0.1:{port}"), format!("[::1]:{port}")] {
                unsafe { sink.unwrap()(context, address.as_ptr(), address.len()) };
            }
            0
        }
        let library = Path::new("librptadviax2.so.2");
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let client = profile.map_or_else(
            || crate::iax::IaxClient::load(library).unwrap(),
            |profile| crate::iax::tests::selected_client(library, profile),
        );
        let mut server =
            crate::iax::IaxServer::bind(&client, address, &["506315".to_owned()]).unwrap();
        let accepted = AtomicUsize::new(0);
        let providers = Box::leak(Box::new(crate::providers::ProviderSet::load().unwrap()));
        let mut host = HostServicesOwner::new(
            Vec::new(),
            Some(&*providers),
            SecretsFile::parse("").unwrap(),
        )
        .unwrap();
        let ingress = host.context.ingress.take();
        host.enable_listener(&mut server).unwrap();
        host.context.ingress = ingress;
        host.enable_listener(&mut server).unwrap();
        let mut resolver = *providers.resolver_descriptor().unwrap();
        resolver.resolve_addresses = Some(ordered_addresses);
        host.context.resolver =
            Some(crate::directory::Resolver::new(Box::leak(Box::new(resolver))).unwrap());
        let host_context = host.descriptor().context as usize;
        let destination = format!("radio@selected.invalid:{}/506315", address.port());
        let (sender, receiver) = std::sync::mpsc::channel();
        let dial = thread::spawn(move || {
            let destination = std::ffi::CString::new(destination).unwrap();
            let local = c"524950";
            let mut peer = ptr::null_mut();
            let status = unsafe {
                super::peer_dial(
                    host_context as *mut c_void,
                    destination.as_ptr(),
                    destination.as_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes),
                    ptr::null_mut(),
                    &mut peer,
                )
            };
            sender.send((status, peer as usize)).unwrap();
        });

        let deadline = Instant::now() + Duration::from_secs(6);
        while !dial.is_finished() && Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(
                        now,
                        Some(allow_test_peer),
                        Some(capture_test_peer),
                        ptr::from_ref(&accepted).cast_mut().cast(),
                    )
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        let (status, peer) = receiver.try_recv().unwrap();
        assert_eq!(status, 0);
        assert_ne!(peer, 0);
        assert_ne!(accepted.load(Ordering::Acquire), 0);
        let accepted_peer = accepted.swap(0, Ordering::AcqRel) as *mut c_void;
        let server_client = crate::iax::IaxClient::load(library).unwrap();
        let mut inbound = unsafe { server_client.adopt_inbound(accepted_peer) }.unwrap();

        // The provider's packet bound is stricter than the host's safe-slice bound.
        let oversized = [0xff; 65508];
        assert_eq!(
            unsafe {
                super::peer_send_prepared(
                    ptr::null_mut(),
                    peer as *mut c_void,
                    oversized.as_ptr(),
                    oversized.len(),
                    oversized.len() as u32,
                    0,
                )
            },
            -1,
            "a provider rejection must not become success or retryable congestion"
        );

        // 51018 sent a valid 1291-byte topology that used to terminate this call.
        let topology = format!("L {}T1506316\0", "T506316,".repeat(160));
        let mut observed = Vec::<Vec<u8>>::new();
        for message in [topology.as_bytes(), b"!NEWKEY1!"] {
            inbound.send_text(message).unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while observed.is_empty() && Instant::now() < deadline {
                assert_eq!(
                    unsafe {
                        super::peer_read(
                            ptr::null_mut(),
                            peer as *mut c_void,
                            Some(record_peer_text),
                            ptr::from_mut(&mut observed).cast(),
                        )
                    },
                    0,
                    "valid topology must not end the call",
                );
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(observed, [message]);
            observed.clear();
        }
        verify_async_audio(&host, peer as *mut c_void, &mut inbound);
        unsafe { super::peer_destroy(ptr::null_mut(), peer as *mut c_void) };
        drop(inbound);

        let canceled_calls = AtomicUsize::new(0);
        let canceled_context = ptr::from_ref(&canceled_calls) as usize;
        let destination = format!("radio@{address}/506315");
        let (sender, receiver) = std::sync::mpsc::channel();
        let dial = thread::spawn(move || {
            let destination = std::ffi::CString::new(destination).unwrap();
            let local = c"524951";
            let mut peer = ptr::null_mut();
            let status = unsafe {
                super::peer_dial(
                    host_context as *mut c_void,
                    destination.as_ptr(),
                    destination.as_bytes().len(),
                    local.as_ptr(),
                    local.to_bytes().len(),
                    0,
                    Some(current_yes_then_no),
                    canceled_context as *mut c_void,
                    &mut peer,
                )
            };
            sender.send((status, peer as usize)).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(6);
        while !dial.is_finished() && Instant::now() < deadline {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as u32;
            unsafe {
                server
                    .poll(
                        now,
                        Some(allow_test_peer),
                        Some(capture_test_peer),
                        ptr::from_ref(&accepted).cast_mut().cast(),
                    )
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(1));
        }
        dial.join().unwrap();
        let (status, peer) = receiver.try_recv().unwrap();
        assert_eq!(status, -1);
        assert_eq!(peer, 0);
        let accepted_peer = accepted.swap(0, Ordering::AcqRel) as *mut c_void;
        let server_client = crate::iax::IaxClient::load(library).unwrap();
        let inbound = unsafe { server_client.adopt_inbound(accepted_peer) }.unwrap();
        verify_attach_rejection(&mut host, inbound);
    }

    fn verify_attach_rejection(host: &mut HostServicesOwner, inbound: crate::iax::IaxPeer) {
        unsafe extern "C" fn unused_step(_: *mut c_void) -> i32 {
            panic!("rejected attachment must not publish a callback");
        }
        let context = host.descriptor().context;
        let handle = host.peer_handle(inbound, "1000", "2000");
        let mut token = 1_usize as *mut c_void;
        assert_eq!(
            unsafe {
                super::peer_step_attach(
                    context,
                    ptr::null_mut(),
                    Some(unused_step),
                    ptr::null_mut(),
                    &mut token,
                )
            },
            -1
        );
        assert!(token.is_null());

        let jitter = host.context.ingress.as_ref().unwrap().jitter;
        let mut invalid = *jitter;
        invalid.create = None;
        host.context.ingress.as_mut().unwrap().jitter = Box::leak(Box::new(invalid));
        assert_eq!(
            unsafe {
                super::peer_step_attach(
                    context,
                    handle,
                    Some(unused_step),
                    ptr::null_mut(),
                    &mut token,
                )
            },
            -1
        );
        assert!(token.is_null());

        let ingress = host.context.ingress.as_mut().unwrap();
        ingress.jitter = jitter;
        ingress.executor.reject_attachment_for_test();
        assert_eq!(
            unsafe {
                super::peer_step_attach(
                    context,
                    handle,
                    Some(unused_step),
                    ptr::null_mut(),
                    &mut token,
                )
            },
            -1
        );
        assert!(token.is_null());
        host.destroy_peer(handle);
    }

    struct AsyncRead {
        host: *mut c_void,
        peer: *mut c_void,
        samples: AtomicUsize,
        observations: AtomicUsize,
        failures: AtomicUsize,
    }

    unsafe extern "C" fn receive_samples(
        context: *mut c_void,
        kind: u32,
        _: *const c_void,
        count: usize,
    ) {
        let state = unsafe { &*context.cast::<AsyncRead>() };
        if kind == 3 {
            state.samples.fetch_add(count, Ordering::Release);
        }
    }

    unsafe extern "C" fn read_step(context: *mut c_void) -> i32 {
        let state = unsafe { &*context.cast::<AsyncRead>() };
        let status =
            unsafe { super::peer_read(state.host, state.peer, Some(receive_samples), context) };
        if status != 0 {
            state.failures.fetch_add(1, Ordering::Relaxed);
        }
        let mut observation: crate::abi::rptadv_peer_observation_v1 = unsafe { std::mem::zeroed() };
        observation.struct_size = size_of::<crate::abi::rptadv_peer_observation_v1>() as u32;
        observation.abi_version = 1;
        if unsafe { super::peer_observe(state.host, state.peer, &mut observation) } != 0 {
            return -1;
        }
        let after = if state.observations.load(Ordering::Acquire) == 0 {
            0
        } else {
            u64::MAX
        };
        if unsafe { super::peer_set_deadline(state.host, state.peer, after) } != 0 {
            return -1;
        }
        state.observations.fetch_add(1, Ordering::Release);
        status
    }

    fn verify_async_audio(
        host: &HostServicesOwner,
        peer: *mut c_void,
        inbound: &mut crate::iax::IaxPeer,
    ) {
        let state = Box::into_raw(Box::new(AsyncRead {
            host: host.descriptor().context,
            peer,
            samples: AtomicUsize::new(0),
            observations: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
        }));
        let mut token = ptr::null_mut();
        assert_eq!(
            unsafe {
                super::peer_step_attach(
                    host.descriptor().context,
                    peer,
                    Some(read_step),
                    state.cast(),
                    &mut token,
                )
            },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { (*state).observations.load(Ordering::Acquire) } < 2
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        let scheduled = unsafe { (*state).observations.load(Ordering::Acquire) };
        unsafe { super::peer_step_wake(host.descriptor().context, token) };
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { (*state).observations.load(Ordering::Acquire) } == scheduled
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        let awakened = unsafe { (*state).observations.load(Ordering::Acquire) } > scheduled;
        crate::iax::tests::send_pcm(inbound, &[0.25; 480]);
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { (*state).samples.load(Ordering::Acquire) } < 480 && Instant::now() < deadline
        {
            thread::yield_now();
        }
        unsafe {
            super::peer_step_stop(host.descriptor().context, token);
            super::peer_step_detach(host.descriptor().context, token);
        }
        let state = unsafe { Box::from_raw(state) };
        assert!(
            scheduled >= 2,
            "the owner deadline must schedule an idle peer"
        );
        assert!(awakened, "an explicit wake must schedule the parked peer");
        assert!(
            state.samples.load(Ordering::Acquire) >= 480,
            "received {} frames, {} failed polls over {} callbacks at {} Hz",
            state.samples.load(Ordering::Acquire),
            state.failures.load(Ordering::Acquire),
            state.observations.load(Ordering::Acquire),
            inbound.sample_rate_hz()
        );
        assert!(state.observations.load(Ordering::Acquire) > 0);
    }
}
