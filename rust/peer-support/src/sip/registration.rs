//! Thread-confined SIP registration and incoming-call contexts and retained neutral descriptor leases.
//! The composition owns the servicing thread and must join it before dropping its provider.
use libloading::Library;
use std::{cell::Cell, ffi::c_void, fmt, mem::size_of, path::Path, ptr::NonNull, sync::Arc};

/// Redacted provider failure; no native reason, endpoint or credential is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The selected shared object could not be opened.
    Load,
    /// The descriptor or returned record violates the neutral contract.
    Contract,
    /// The provider rejected an operation with its numeric ABI status.
    Status(i32),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Load => f.write_str("cannot load SIP registration provider"),
            Self::Contract => f.write_str("invalid SIP registration provider contract"),
            Self::Status(code) => write!(f, "SIP registration provider returned {code}"),
        }
    }
}
impl std::error::Error for Error {}

#[repr(C)]
#[derive(Clone, Copy)]
struct Span {
    data: *const u8,
    length: usize,
}
impl From<&str> for Span {
    fn from(s: &str) -> Self {
        Self {
            data: s.as_ptr(),
            length: s.len(),
        }
    }
}
#[repr(C)]
struct Config {
    struct_size: u32,
    expires: u32,
    generation: u64,
    bind: Span,
    next_hop: Span,
    registrar: Span,
    aor: Span,
    username: Span,
    password: Span,
}

/// Borrowed preparation input; create copies every retained value without sending REGISTER.
pub struct Settings<'a> {
    /// Nonzero immutable account generation.
    pub generation: u64,
    /// Requested registration lifetime, in seconds.
    pub expires: u16,
    /// Numeric local SocketAddr; port zero selects an ephemeral port.
    pub bind: &'a str,
    /// Prepared numeric remote SocketAddr.
    pub next_hop: &'a str,
    /// SIP registrar authority, distinct from its resolved transport endpoint.
    pub registrar: &'a str,
    /// SIP address of record, with any username URI escaping already applied.
    pub aor: &'a str,
    /// Literal Digest username.
    pub username: &'a str,
    /// Literal private credential, never formatted in diagnostics.
    pub password: &'a str,
}

/// Authoritative numeric snapshot. State values follow the versioned provider header.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    /// Complete writable record size.
    pub struct_size: u32,
    /// Prepared=0, registering=1, registered=2, removing=3, removed=4,
    /// failed=5, stopping=6, stopped=7, shutdown-failed=8.
    pub state: u32,
    /// Immutable account generation.
    pub generation: u64,
    /// Monotonic provider observation sequence.
    pub sequence: u64,
    /// Numeric SIP response, never untrusted reason text.
    pub sip_status: u32,
    /// Last accepted matching Contact expiry, in seconds.
    pub accepted_expires: u32,
    /// Saturating successful initial/refresh response count.
    pub registrations: u32,
    /// Saturating count of replaced events in the provider's bounded queue.
    pub events_dropped: u32,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            state: 0,
            generation: 0,
            sequence: 0,
            sip_status: 0,
            accepted_expires: 0,
            registrations: 0,
            events_dropped: 0,
        }
    }
}

/// Current call ownership, retained independently of registration observations.
#[repr(C)]
#[derive(Clone)]
pub struct Call {
    struct_size: u32,
    /// Invited=1, answered=2, established=3, closing=4, terminated=5.
    pub state: u32,
    /// Immutable account generation.
    pub generation: u64,
    /// Nonzero token, never reused within this account generation.
    pub call: u64,
    /// Numeric SIP status; no untrusted reason/header text.
    pub sip_status: u32,
    offer_length: u32,
    offer: [u8; 4096],
    source_family: u16,
    source_port: u16,
    source: [u8; 16],
    /// Incoming=0, outgoing=1; direction is not mailbox/control authority.
    pub direction: u32,
}
impl Default for Call {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            state: 0,
            generation: 0,
            call: 0,
            sip_status: 0,
            offer_length: 0,
            offer: [0; 4096],
            source_family: 0,
            source_port: 0,
            source: [0; 16],
            direction: 0,
        }
    }
}
impl Call {
    /// Remote offer/answer, empty before an outgoing answer; never mailbox authority.
    pub fn offer(&self) -> &[u8] {
        &self.offer[..self.offer_length as usize]
    }

    /// Actual source socket address copied from the native transport, never Via or Caller-ID.
    pub fn source(&self) -> Result<std::net::SocketAddr, Error> {
        let ip = match self.source_family {
            4 => std::net::IpAddr::V4(std::net::Ipv4Addr::from(
                <[u8; 4]>::try_from(&self.source[..4]).expect("fixed address extent"),
            )),
            6 => std::net::IpAddr::V6(std::net::Ipv6Addr::from(self.source)),
            _ => return Err(Error::Contract),
        };
        if self.source_port == 0 {
            return Err(Error::Contract);
        }
        Ok(std::net::SocketAddr::new(ip, self.source_port))
    }

    fn validate(&mut self, generation: u64, supports_outgoing: bool) -> Result<(), Error> {
        if !supports_outgoing {
            // This field was padding in the released incoming-only record.
            self.direction = 0;
        }
        let absent_answer = self.direction == 1
            && matches!(self.state, 1 | 4 | 5)
            && self.offer_length == 0
            && self.source_family == 0
            && self.source_port == 0
            && self.source == [0; 16];
        if self.struct_size != size_of::<Self>() as u32
            || self.generation != generation
            || self.call == 0
            || !(1..=5).contains(&self.state)
            || self.direction > 1
            || self.offer_length as usize > self.offer.len()
            || (!absent_answer && (self.source().is_err() || self.offer_length == 0))
        {
            return Err(Error::Contract);
        }
        Ok(())
    }
}

type CallOriginate = unsafe extern "C" fn(*mut c_void, u64, Span, Span, *mut u64) -> i32;
type CallRead = unsafe extern "C" fn(*mut c_void, u64, *mut Call) -> i32;
type CallResponse = unsafe extern "C" fn(*mut c_void, u64, u64, u32, Span) -> i32;
type CallAction = unsafe extern "C" fn(*mut c_void, u64, u64) -> i32;
type Action = unsafe extern "C" fn(*mut c_void, u64) -> i32;
type Read = unsafe extern "C" fn(*mut c_void, u64, *mut Status) -> i32;
#[repr(C)]
#[derive(Clone, Copy)]
struct Descriptor {
    struct_size: u32,
    abi_version: u32,
    capability: [u8; 16],
    create: Option<unsafe extern "C" fn(*const Config, *mut *mut c_void) -> i32>,
    service: Option<Action>,
    next_event: Option<Read>,
    status: Option<Read>,
    register_account: Option<Action>,
    unregister_account: Option<Action>,
    shutdown: Option<Action>,
    destroy: Option<Action>,
    next_call: Option<CallRead>,
    respond_call: Option<CallResponse>,
    hangup_call: Option<CallAction>,
    release_call: Option<CallAction>,
    originate_call: Option<CallOriginate>,
    acknowledge_call: Option<CallAction>,
}
impl Descriptor {
    unsafe fn read(pointer: *const Self) -> Result<Self, Error> {
        if pointer.is_null() {
            return Err(Error::Contract);
        }
        let size = unsafe { std::ptr::addr_of!((*pointer).struct_size).read_unaligned() } as usize;
        let prefix = std::mem::offset_of!(Self, originate_call);
        if size < prefix || (size > prefix && size < size_of::<Self>()) {
            return Err(Error::Contract);
        }
        // Zero initializes nullable optional slots without reading past the released prefix.
        let mut api = std::mem::MaybeUninit::<Self>::zeroed();
        unsafe {
            std::ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                api.as_mut_ptr().cast::<u8>(),
                size.min(size_of::<Self>()),
            );
        }
        let api = unsafe { api.assume_init() };
        if api.abi_version != 1
            || api.capability != *b"rptadv.sip\0\0\0\0\0\0"
            || api.originate_call.is_some() != api.acknowledge_call.is_some()
            || ![
                api.create.is_some(),
                api.service.is_some(),
                api.next_event.is_some(),
                api.status.is_some(),
                api.register_account.is_some(),
                api.unregister_account.is_some(),
                api.shutdown.is_some(),
                api.destroy.is_some(),
                api.next_call.is_some(),
                api.respond_call.is_some(),
                api.hangup_call.is_some(),
                api.release_call.is_some(),
            ]
            .into_iter()
            .all(|present| present)
        {
            return Err(Error::Contract);
        }
        Ok(api)
    }
}

/// Immutable operations and their code lease, shareable before any context is created.
pub struct Provider {
    api: Descriptor,
    _library: Library,
}
impl Provider {
    /// Load the complete neutral ABI without creating a socket, context or thread.
    /// # Safety
    /// `path` identifies trusted native code. Keep this lease until all servicing
    /// threads are joined, including after the last successful context destruction.
    pub unsafe fn load(path: &Path) -> Result<Arc<Self>, Error> {
        let library = unsafe { Library::new(path) }.map_err(|_| Error::Load)?;
        let get = unsafe {
            library.get::<unsafe extern "C" fn() -> *const Descriptor>(
                b"rptadv_sip_adapter_descriptor_v1\0",
            )
        }
        .map_err(|_| Error::Contract)?;
        let api = unsafe { Descriptor::read(get()) }?;
        Ok(Arc::new(Self {
            api,
            _library: library,
        }))
    }
    /// Prepare one account on its sole future servicing thread; no REGISTER is sent.
    pub fn prepare(self: &Arc<Self>, settings: &Settings<'_>) -> Result<Account, Error> {
        let config = Config {
            struct_size: size_of::<Config>() as u32,
            expires: u32::from(settings.expires),
            generation: settings.generation,
            bind: settings.bind.into(),
            next_hop: settings.next_hop.into(),
            registrar: settings.registrar.into(),
            aor: settings.aor.into(),
            username: settings.username.into(),
            password: settings.password.into(),
        };
        let mut context = std::ptr::null_mut();
        let code = unsafe { self.api.create.unwrap()(&config, &mut context) };
        if code != 0 {
            if !context.is_null() {
                std::mem::forget(Arc::clone(self));
            }
            return Err(Error::Status(code));
        }
        Ok(Account {
            context: Cell::new(Some(NonNull::new(context).ok_or(Error::Contract)?)),
            provider: Arc::clone(self),
            generation: settings.generation,
        })
    }
}

/// One non-Send/non-Sync account. Every operation runs on its creating thread.
/// Explicit shutdown/service/destroy is required; a nonquiescent drop retains its code.
pub struct Account {
    context: Cell<Option<NonNull<c_void>>>,
    provider: Arc<Provider>,
    generation: u64,
}
impl Account {
    fn action(&self, action: Action) -> Result<(), Error> {
        let context = self.context.get().ok_or(Error::Contract)?;
        let code = unsafe { action(context.as_ptr(), self.generation) };
        if code == 0 {
            Ok(())
        } else {
            Err(Error::Status(code))
        }
    }
    /// Execute a bounded nonblocking native turn.
    pub fn service(&self) -> Result<(), Error> {
        self.action(self.provider.api.service.unwrap())
    }
    /// Send the first REGISTER only after committed product publication.
    pub fn activate(&self) -> Result<(), Error> {
        self.action(self.provider.api.register_account.unwrap())
    }
    /// Request removal of the current registration.
    pub fn unregister(&self) -> Result<(), Error> {
        self.action(self.provider.api.unregister_account.unwrap())
    }
    /// Begin asynchronous shutdown; continue servicing until state 7.
    pub fn shutdown(&self) -> Result<(), Error> {
        self.action(self.provider.api.shutdown.unwrap())
    }
    /// Return a checked, credential-free snapshot.
    pub fn status(&self) -> Result<Status, Error> {
        let mut status = Status::default();
        let context = self.context.get().ok_or(Error::Contract)?;
        let code = unsafe {
            self.provider.api.status.unwrap()(context.as_ptr(), self.generation, &mut status)
        };
        if code != 0 {
            return Err(Error::Status(code));
        }
        if status.struct_size != size_of::<Status>() as u32
            || status.generation != self.generation
            || status.state > 8
        {
            return Err(Error::Contract);
        }
        Ok(status)
    }
    /// Poll one coalesced authoritative call observation. Terminal ownership
    /// persists until release_call even after this observation is consumed.
    pub fn next_call(&self) -> Result<Option<Call>, Error> {
        let context = self.context.get().ok_or(Error::Contract)?;
        let mut call = Call::default();
        let code = unsafe {
            self.provider.api.next_call.unwrap()(context.as_ptr(), self.generation, &mut call)
        };
        match code {
            0 => {
                call.validate(self.generation, self.provider.api.originate_call.is_some())?;
                Ok(Some(call))
            }
            1 => Ok(None),
            _ => Err(Error::Status(code)),
        }
    }
    /// Explicitly accept with bounded SDP or reject with 400..699 and empty SDP.
    pub fn respond_call(&self, call: u64, status: u32, answer: &[u8]) -> Result<(), Error> {
        let context = self.context.get().ok_or(Error::Contract)?;
        let code = unsafe {
            self.provider.api.respond_call.unwrap()(
                context.as_ptr(),
                self.generation,
                call,
                status,
                Span {
                    data: answer.as_ptr(),
                    length: answer.len(),
                },
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(Error::Status(code))
        }
    }
    /// Originate one exact call using the optional outgoing tail and retained local offer.
    pub fn originate_call(&self, destination: &str, offer: &[u8]) -> Result<u64, Error> {
        let action = self.provider.api.originate_call.ok_or(Error::Contract)?;
        let context = self.context.get().ok_or(Error::Contract)?;
        let mut call = 0;
        let code = unsafe {
            action(
                context.as_ptr(),
                self.generation,
                destination.into(),
                Span {
                    data: offer.as_ptr(),
                    length: offer.len(),
                },
                &mut call,
            )
        };
        if code != 0 {
            return Err(Error::Status(code));
        }
        if call == 0 {
            return Err(Error::Contract);
        }
        Ok(call)
    }
    /// ACK a final answer only after the consumer has validated its media contract.
    pub fn acknowledge_call(&self, call: u64) -> Result<(), Error> {
        self.call_action(
            self.provider.api.acknowledge_call.ok_or(Error::Contract)?,
            call,
        )
    }
    fn call_action(&self, action: CallAction, call: u64) -> Result<(), Error> {
        let context = self.context.get().ok_or(Error::Contract)?;
        let code = unsafe { action(context.as_ptr(), self.generation, call) };
        if code == 0 {
            Ok(())
        } else {
            Err(Error::Status(code))
        }
    }
    /// End one exact call, without releasing its terminal observation.
    pub fn hangup_call(&self, call: u64) -> Result<(), Error> {
        self.call_action(self.provider.api.hangup_call.unwrap(), call)
    }
    /// Release an exact terminal token; pending or stale tokens remain errors.
    pub fn release_call(&self, call: u64) -> Result<(), Error> {
        self.call_action(self.provider.api.release_call.unwrap(), call)
    }
    /// Destroy only a fully stopped context; pending/failed destruction retains ownership.
    pub fn destroy(&self) -> Result<(), Error> {
        self.action(self.provider.api.destroy.unwrap())?;
        self.context.set(None);
        Ok(())
    }
}
impl Drop for Account {
    fn drop(&mut self) {
        if self.context.get().is_some() && self.destroy().is_err() {
            // Native callbacks/storage remain live. Never dlclose underneath them.
            std::mem::forget(Arc::clone(&self.provider));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn create(_: *const Config, _: *mut *mut c_void) -> i32 {
        -1
    }
    unsafe extern "C" fn action(_: *mut c_void, _: u64) -> i32 {
        0
    }
    unsafe extern "C" fn read(_: *mut c_void, _: u64, _: *mut Status) -> i32 {
        0
    }
    unsafe extern "C" fn call_read(_: *mut c_void, _: u64, _: *mut Call) -> i32 {
        1
    }
    unsafe extern "C" fn call_response(_: *mut c_void, _: u64, _: u64, _: u32, _: Span) -> i32 {
        0
    }
    unsafe extern "C" fn call_action(_: *mut c_void, _: u64, _: u64) -> i32 {
        0
    }
    unsafe extern "C" fn originate(_: *mut c_void, _: u64, _: Span, _: Span, _: *mut u64) -> i32 {
        0
    }
    fn descriptor() -> Descriptor {
        Descriptor {
            struct_size: size_of::<Descriptor>() as u32,
            abi_version: 1,
            capability: *b"rptadv.sip\0\0\0\0\0\0",
            create: Some(create),
            service: Some(action),
            next_event: Some(read),
            status: Some(read),
            register_account: Some(action),
            unregister_account: Some(action),
            shutdown: Some(action),
            destroy: Some(action),
            next_call: Some(call_read),
            respond_call: Some(call_response),
            hangup_call: Some(call_action),
            release_call: Some(call_action),
            originate_call: Some(originate),
            acknowledge_call: Some(call_action),
        }
    }
    #[test]
    fn call_consumers_reject_registration_only_descriptor() {
        let mut table = descriptor();
        table.struct_size = (24 + size_of::<[usize; 8]>()) as u32;
        assert!(
            unsafe { Descriptor::read(&table) }.is_err(),
            "incoming call consumer requires the complete call tail"
        );
    }
    #[test]
    fn descriptor_requires_complete_identity_and_lifecycle_before_use() {
        let mut table = descriptor();
        assert!(unsafe { Descriptor::read(&table) }.is_ok());
        table.struct_size -= 1;
        assert!(unsafe { Descriptor::read(&table) }.is_err());
        table = descriptor();
        table.abi_version = 2;
        assert!(unsafe { Descriptor::read(&table) }.is_err());
        table = descriptor();
        table.capability[0] = 0;
        assert!(unsafe { Descriptor::read(&table) }.is_err());
        for absent in 0..14 {
            table = descriptor();
            match absent {
                0 => table.create = None,
                1 => table.service = None,
                2 => table.next_event = None,
                3 => table.status = None,
                4 => table.register_account = None,
                5 => table.unregister_account = None,
                6 => table.shutdown = None,
                7 => table.destroy = None,
                8 => table.next_call = None,
                9 => table.respond_call = None,
                10 => table.hangup_call = None,
                11 => table.release_call = None,
                12 => table.originate_call = None,
                _ => table.acknowledge_call = None,
            }
            assert!(unsafe { Descriptor::read(&table) }.is_err());
        }
        assert!(unsafe { Descriptor::read(std::ptr::null()) }.is_err());
        let prefix = 4_u32;
        assert!(unsafe { Descriptor::read(std::ptr::from_ref(&prefix).cast()) }.is_err());
        assert_eq!(size_of::<Status>(), 40);
        assert_eq!(size_of::<Config>(), 16 + 12 * size_of::<usize>());
        assert_eq!(size_of::<Descriptor>(), 24 + size_of::<[usize; 14]>());
    }
    #[test]
    fn outgoing_offer_is_absent_until_a_validated_answer_arrives() {
        let mut call = Call {
            generation: 7,
            call: 1,
            state: 1,
            direction: 1,
            ..Call::default()
        };
        assert!(
            call.validate(7, true).is_ok(),
            "outgoing setup has no remote SDP yet"
        );
        for state in [4, 5] {
            call.state = state;
            assert!(
                call.validate(7, true).is_ok(),
                "failed outgoing setup still owns its token"
            );
        }
        for state in [2, 3] {
            call.state = state;
            assert_eq!(call.validate(7, true), Err(Error::Contract));
        }
        call.offer_length = 1;
        call.source_family = 4;
        call.source_port = 5060;
        assert!(call.validate(7, true).is_ok());
        call.direction = 2;
        assert_eq!(call.validate(7, true), Err(Error::Contract));
        call.direction = 0;
        assert!(call.validate(7, true).is_ok());
        call.offer_length = 0;
        assert_eq!(
            call.validate(7, true),
            Err(Error::Contract),
            "incoming admission requires an offer"
        );
    }
    #[test]
    fn optional_outgoing_tail_preserves_incoming_prefix_without_reading_old_padding() {
        let mut table = descriptor();
        let prefix = std::mem::offset_of!(Descriptor, originate_call) as u32;
        table.struct_size = prefix;
        let incoming =
            unsafe { Descriptor::read(&table) }.expect("released incoming prefix remains usable");
        assert!(incoming.originate_call.is_none());
        assert!(incoming.acknowledge_call.is_none());
        for size in prefix + 1..size_of::<Descriptor>() as u32 {
            table.struct_size = size;
            assert!(
                unsafe { Descriptor::read(&table) }.is_err(),
                "partial outgoing group rejected"
            );
        }
        table = descriptor();
        table.originate_call = None;
        table.acknowledge_call = None;
        assert!(unsafe { Descriptor::read(&table) }.is_ok());
        let mut call = Call {
            generation: 7,
            call: 1,
            state: 1,
            direction: 0xdeadbeef,
            source_family: 4,
            source_port: 5060,
            offer_length: 1,
            ..Call::default()
        };
        assert!(call.validate(7, false).is_ok());
        assert_eq!(
            call.direction, 0,
            "old ABI padding cannot become outgoing authority"
        );
    }
}
