//! Bounded SIP/UDP registration through an independently replaceable native stack.
//! The embedding host owns the sole servicing thread, activation and library lease.
#![deny(warnings, missing_docs)]

use std::{
    cell::UnsafeCell,
    collections::VecDeque,
    ffi::{CString, c_char, c_void},
    mem::size_of,
    net::SocketAddr,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
};

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code,
    missing_docs
)]
mod abi {
    include!(concat!(env!("OUT_DIR"), "/abi.rs"));
}
use abi::{rptadv_sip_config_v1 as Config, rptadv_sip_status_v1 as Status};

unsafe extern "C" {
    fn sip_native_create(
        bind: *const c_char,
        sink: unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const c_char, usize),
        context: *mut c_void,
    ) -> *mut c_void;
    fn sip_native_owner(context: *mut c_void) -> i32;
    fn sip_native_step(context: *mut c_void);
    fn sip_native_register(
        context: *mut c_void,
        registrar: *const c_char,
        aor: *const c_char,
        hop: *const c_char,
        expires: *const c_char,
    ) -> i32;
    fn sip_native_authenticate(context: *mut c_void, credentials: *const c_char);
    fn sip_native_unregister(context: *mut c_void);
    fn sip_native_cancel(context: *mut c_void);
    fn sip_native_shutdown(context: *mut c_void);
    fn sip_native_destroy(context: *mut c_void);
}

/// Wipe the adapter-owned formatted credential when its context retires.
struct Secret(Vec<u8>);
impl Drop for Secret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // SAFETY: each byte is valid and uniquely borrowed; volatile prevents elision.
            unsafe { ptr::write_volatile(byte, 0) };
        }
    }
}

/// Callback-owned observations; no callback borrows the surrounding account.
struct Observations {
    status: Status,
    events: VecDeque<Status>,
    authenticate: bool,
    challenges: u32,
    realm: [u8; 256],
    realm_length: usize,
}
impl Observations {
    fn publish(&mut self, state: u32, code: u32, expiry: u32) {
        self.status.state = state;
        self.status.sip_status = code;
        self.status.accepted_expires = expiry;
        self.status.sequence = self.status.sequence.saturating_add(1);
        if self.events.len() == 8 {
            self.events.pop_front();
            self.status.events_dropped = self.status.events_dropped.saturating_add(1);
        }
        self.events.push_back(self.status);
    }
    fn observe(&mut self, kind: u32, code: u32, expiry: u32) {
        if code < 200 {
            return;
        }
        if kind == 3 {
            self.publish(if code == 200 { 7 } else { 8 }, code, 0);
            return;
        }
        if kind == 4 {
            if matches!(self.status.state, 1 | 2) && code >= 300 {
                self.authenticate = false;
                self.publish(5, code, 0);
            }
            return;
        }
        if kind == 5 {
            if matches!(self.status.state, 1..=3) && code >= 300 {
                self.authenticate = false;
                self.publish(5, code, 0);
            }
            return;
        }
        if !matches!(self.status.state, 1..=3) {
            return;
        }
        if kind == 1 && self.status.state == 3 {
            // NUA may need credentials to finish an in-flight refresh before it
            // services queued removal. Preserve that progress without undoing removal.
            if matches!(code, 401 | 407) {
                self.authenticate = self.challenges == 0;
                if self.authenticate {
                    self.challenges = 1;
                } else {
                    // NUA can wait indefinitely for credentials; cancel through FAILED.
                    // This does not claim the registrar removed its remote binding.
                    self.publish(5, code, 0);
                }
            } else {
                self.challenges = 0;
                self.authenticate = false;
            }
            return;
        }
        if kind == 2 && self.status.state != 3 {
            return;
        }
        if matches!(code, 401 | 407) && self.challenges == 0 {
            self.challenges = 1;
            self.authenticate = true;
            if self.status.state == 2 {
                // Observe actual reauthentication, not an unobservable refresh send.
                self.publish(1, code, self.status.accepted_expires);
            }
            return;
        }
        self.authenticate = false;
        if (200..300).contains(&code) && kind == 2 {
            self.publish(4, code, 0);
        } else if (200..300).contains(&code) && (1..=86400).contains(&expiry) {
            self.challenges = 0;
            self.status.registrations = self.status.registrations.saturating_add(1);
            self.publish(2, code, expiry);
        } else {
            self.publish(5, code, 0);
        }
    }
}

/// Stable account allocation, retained until the native shutdown callback succeeds.
struct Account {
    native: *mut c_void,
    observed: Box<UnsafeCell<Observations>>,
    registrar: CString,
    aor: CString,
    hop: CString,
    activation_pending: bool,
    expires: CString,
    auth: Secret,
}

impl Account {
    /// Borrow callback observations only between native calls on the sole owner.
    fn observed(&self) -> &Observations {
        // SAFETY: all entries are serialized; callers end this borrow before native service.
        unsafe { &*self.observed.get() }
    }

    /// Mutate through the same interior-mutable allocation used by native callbacks.
    fn observed_mut(&mut self) -> &mut Observations {
        // SAFETY: sole-owner access never overlaps the callback's temporary mutable borrow.
        unsafe { &mut *self.observed.get() }
    }
}

/// Catch only the ABI boundary; invalid input and operational failures remain explicit.
fn boundary(f: impl FnOnce() -> Result<i32, i32>) -> i32 {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or(Err(-1))
        .unwrap_or_else(|code| code)
}

/// Borrow a bounded UTF-8 field; NUL cannot cross the native C-string boundary.
unsafe fn field<'a>(span: abi::rptadv_sip_span_v1, max: usize) -> Result<&'a str, i32> {
    if span.data.is_null() || span.length == 0 || span.length > max {
        return Err(-1);
    }
    // SAFETY: caller supplies a readable extent for the duration of create.
    let bytes = unsafe { std::slice::from_raw_parts(span.data, span.length) };
    if bytes.contains(&0) {
        return Err(-1);
    }
    std::str::from_utf8(bytes).map_err(|_| -1)
}

/// Narrow SIP/UDP URI grammar: no URI passwords, header parameters or routing overrides.
fn sip_uri(text: &str, aor: bool) -> Result<CString, i32> {
    let rest = text.strip_prefix("sip:").ok_or(-1)?;
    let authority = if aor {
        let (user, host) = rest.split_once('@').ok_or(-1)?;
        if user.is_empty() {
            return Err(-1);
        }
        let mut bytes = user.as_bytes();
        while let Some((&byte, rest)) = bytes.split_first() {
            bytes = rest;
            if byte == b'%' {
                if bytes.len() < 2 || !bytes[..2].iter().all(u8::is_ascii_hexdigit) {
                    return Err(-1);
                }
                bytes = &bytes[2..];
            } else if !byte.is_ascii_alphanumeric() && !b"-_.+~".contains(&byte) {
                return Err(-1);
            }
        }
        host
    } else {
        rest
    };
    if authority.is_empty()
        || !authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b))
    {
        return Err(-1);
    }
    CString::new(text).map_err(|_| -1)
}

/// Construct a pointer-stable account only after all neutral inputs validate.
unsafe fn prepare(config: &Config) -> Result<Box<Account>, i32> {
    if config.struct_size != size_of::<Config>() as u32
        || config.generation == 0
        || !(1..=65535).contains(&config.expires)
    {
        return Err(-1);
    }
    // SAFETY: each config extent belongs to the caller for this synchronous call.
    let bind: SocketAddr = unsafe { field(config.bind, 64)? }.parse().map_err(|_| -1)?;
    let hop: SocketAddr = unsafe { field(config.next_hop, 64)? }
        .parse()
        .map_err(|_| -1)?;
    if hop.port() == 0
        || bind.is_ipv4() != hop.is_ipv4()
        || hop.ip().is_unspecified()
        || hop.ip().is_multicast()
    {
        return Err(-1);
    }
    let registrar = sip_uri(unsafe { field(config.registrar, 264)? }, false)?;
    let aor = sip_uri(unsafe { field(config.aor, 1030)? }, true)?;
    let user = unsafe { field(config.username, 255)? };
    let password = unsafe { field(config.password, 4096)? };
    // Sofia parses a colon-delimited C string and emits the Digest username
    // inside quotes without escaping. Password bytes are hashed, never emitted.
    if password.contains(':')
        || user.contains([':', '"', '\\'])
        || user.chars().any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(-1);
    }
    let auth = Secret(format!(":{user}:{password}\0").into_bytes());
    let observed = Box::new(UnsafeCell::new(Observations {
        status: Status {
            struct_size: size_of::<Status>() as u32,
            state: 0,
            generation: config.generation,
            sequence: 0,
            sip_status: 0,
            accepted_expires: 0,
            registrations: 0,
            events_dropped: 0,
        },
        events: VecDeque::with_capacity(8),
        authenticate: false,
        challenges: 0,
        realm: [0; 256],
        realm_length: 0,
    }));
    let mut account = Box::new(Account {
        native: ptr::null_mut(),
        observed,
        registrar,
        aor,
        hop: CString::new(format!("sip:{hop};transport=udp")).map_err(|_| -1)?,
        activation_pending: false,
        expires: CString::new(config.expires.to_string()).map_err(|_| -1)?,
        auth,
    });
    let binding = bind.to_string();
    let binding = binding
        .strip_suffix(":0")
        .map_or(binding.clone(), |address| format!("{address}:*"));
    let bind = CString::new(format!("sip:{binding};transport=udp")).map_err(|_| -1)?;
    // SAFETY: callback allocation is stable until successful shutdown and destruction.
    account.native =
        unsafe { sip_native_create(bind.as_ptr(), observe, account.observed.get().cast()) };
    if account.native.is_null() {
        return Err(-4);
    }
    Ok(account)
}

/// Copy numeric data and at most one native-validated quoted realm, never raw messages.
unsafe extern "C" fn observe(
    context: *mut c_void,
    kind: u32,
    code: u32,
    expiry: u32,
    realm: *const c_char,
    realm_length: usize,
) {
    // SAFETY: stable interior-mutable callback storage is accessed only by its owner.
    let observed = unsafe { &mut *context.cast::<Observations>() };
    observed.observe(kind, code, expiry);
    if matches!(kind, 1 | 2) && matches!(code, 401 | 407) && observed.authenticate {
        if realm.is_null() || realm_length == 0 || realm_length > observed.realm.len() {
            observed.authenticate = false;
            observed.publish(5, code, 0);
        } else {
            // SAFETY: the private native shim lends a checked extent until this callback returns.
            let bytes = unsafe { std::slice::from_raw_parts(realm.cast::<u8>(), realm_length) };
            observed.realm[..realm_length].copy_from_slice(bytes);
            observed.realm_length = realm_length;
        }
    }
}

/// Validate owner and generation before exposing any mutable account state.
unsafe fn checked(context: *mut c_void, generation: u64) -> Result<*mut Account, i32> {
    if context.is_null() {
        return Err(-1);
    }
    let pointer = context.cast::<Account>();
    // SAFETY: caller holds the live opaque context and serializes operations.
    let account = unsafe { &*pointer };
    if account.observed().status.generation != generation {
        return Err(-2);
    }
    if unsafe { sip_native_owner(account.native) } == 0 {
        return Err(-3);
    }
    Ok(pointer)
}

unsafe extern "C" fn create(config: *const Config, output: *mut *mut c_void) -> i32 {
    boundary(|| {
        if output.is_null() {
            return Err(-1);
        }
        // SAFETY: output points to caller-owned writable pointer storage.
        unsafe { output.write(ptr::null_mut()) };
        if config.is_null() || unsafe { (*config).struct_size } != size_of::<Config>() as u32 {
            return Err(-1);
        }
        // SAFETY: caller lends a complete config and its extents through this call.
        let account = unsafe { prepare(&*config)? };
        unsafe { output.write(Box::into_raw(account).cast()) };
        Ok(0)
    })
}

unsafe extern "C" fn service(context: *mut c_void, generation: u64) -> i32 {
    boundary(|| {
        let pointer = unsafe { checked(context, generation)? };
        // No Rust borrow of callback storage is live across any native call.
        let native = unsafe { (*pointer).native };
        let stopped = matches!(unsafe { (*pointer).observed().status.state }, 7 | 8);
        if !stopped {
            unsafe { sip_native_step(native) };
        }
        if unsafe { (*pointer).activation_pending } {
            let result = {
                let account = unsafe { &*pointer };
                unsafe {
                    sip_native_register(
                        native,
                        account.registrar.as_ptr(),
                        account.aor.as_ptr(),
                        account.hop.as_ptr(),
                        account.expires.as_ptr(),
                    )
                }
            };
            if result != 1 {
                let account = unsafe { &mut *pointer };
                account.activation_pending = false;
                if result != 0 {
                    account.observed_mut().publish(5, 0, 0);
                }
            }
        }
        let (credentials, cancel) = {
            let account = unsafe { &mut *pointer };
            let observed = account.observed_mut();
            let credentials = if observed.authenticate {
                observed.authenticate = false;
                let mut value = Secret(b"Digest:".to_vec());
                value
                    .0
                    .extend_from_slice(&observed.realm[..observed.realm_length]);
                value.0.extend_from_slice(&account.auth.0);
                Some(value)
            } else {
                None
            };
            (credentials, account.observed().status.state == 5)
        };
        if let Some(credentials) = credentials {
            unsafe { sip_native_authenticate(native, credentials.0.as_ptr().cast()) };
        }
        if cancel {
            unsafe { sip_native_cancel(native) };
        }
        Ok(0)
    })
}

unsafe fn read(
    context: *mut c_void,
    generation: u64,
    output: *mut Status,
    pop: bool,
) -> Result<i32, i32> {
    let account = unsafe { &mut *checked(context, generation)? };
    if output.is_null() || unsafe { (*output).struct_size } != size_of::<Status>() as u32 {
        return Err(-1);
    }
    let snapshot = if pop {
        let Some(event) = account.observed_mut().events.pop_front() else {
            return Ok(1);
        };
        event
    } else {
        account.observed().status
    };
    unsafe { output.write(snapshot) };
    Ok(0)
}
unsafe extern "C" fn status(context: *mut c_void, generation: u64, output: *mut Status) -> i32 {
    boundary(|| unsafe { read(context, generation, output, false) })
}
unsafe extern "C" fn next_event(context: *mut c_void, generation: u64, output: *mut Status) -> i32 {
    boundary(|| unsafe { read(context, generation, output, true) })
}
unsafe extern "C" fn register(context: *mut c_void, generation: u64) -> i32 {
    boundary(|| {
        let account = unsafe { &mut *checked(context, generation)? };
        if account.observed().status.state != 0 {
            return Err(-3);
        }
        account.observed_mut().publish(1, 0, 0);
        account.activation_pending = true;
        Ok(0)
    })
}
unsafe extern "C" fn unregister(context: *mut c_void, generation: u64) -> i32 {
    boundary(|| {
        let native = {
            let account = unsafe { &mut *checked(context, generation)? };
            if account.observed().status.state != 2 {
                return Err(-3);
            }
            account.observed_mut().publish(3, 0, 0);
            account.native
        };
        unsafe { sip_native_unregister(native) };
        Ok(0)
    })
}
unsafe extern "C" fn shutdown(context: *mut c_void, generation: u64) -> i32 {
    boundary(|| {
        let native = {
            let account = unsafe { &mut *checked(context, generation)? };
            if account.observed().status.state >= 6 {
                return Ok(0);
            }
            account.observed_mut().authenticate = false;
            account.activation_pending = false;
            account.observed_mut().publish(6, 0, 0);
            account.native
        };
        unsafe { sip_native_shutdown(native) };
        Ok(0)
    })
}

unsafe extern "C" fn destroy(context: *mut c_void, generation: u64) -> i32 {
    boundary(|| {
        let pointer = unsafe { checked(context, generation)? };
        if unsafe { (*pointer).observed().status.state } != 7 {
            return Ok(1);
        }
        // Successful final shutdown proves no future native callbacks can occur.
        unsafe { sip_native_destroy((*pointer).native) };
        unsafe { drop(Box::from_raw(pointer)) };
        Ok(0)
    })
}

/// Borrow the complete registration-only descriptor while retaining the DSO.
#[unsafe(no_mangle)]
pub extern "C" fn rptadv_sip_adapter_descriptor_v1() -> *const c_void {
    static DESCRIPTOR: abi::rptadv_sip_descriptor_v1 = abi::rptadv_sip_descriptor_v1 {
        struct_size: size_of::<abi::rptadv_sip_descriptor_v1>() as u32,
        abi_version: 1,
        capability: *b"rptadv.sip\0\0\0\0\0\0",
        create: Some(create),
        service: Some(service),
        next_event: Some(next_event),
        status: Some(status),
        register_account: Some(register),
        unregister_account: Some(unregister),
        shutdown: Some(shutdown),
        destroy: Some(destroy),
    };
    ptr::from_ref(&DESCRIPTOR).cast()
}

#[cfg(test)]
mod tests;
