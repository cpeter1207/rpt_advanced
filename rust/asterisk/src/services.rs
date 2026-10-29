//! Public-Asterisk implementation of the portable product host-services table.

#[cfg(test)]
#[path = "services/echo_tests.rs"]
mod echo_tests;

use crate::{
    bindings as ffi,
    connection::Connection,
    link::{
        directory,
        peer_io::{Input, PeerIo},
    },
    radio::Radio,
};
use rptadv_peer_support::echolink::{
    abi as echo,
    host::{
        AccountVisitor, Accounts, Error as EchoError, HostPeer, Preparation, Registration,
        UpdateError, UpdateOutcome,
    },
};
use std::{
    ffi::{CString, c_char, c_void},
    mem::size_of,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{Arc, Mutex},
};

pub(crate) unsafe fn text<'a>(pointer: *const c_char, length: usize) -> Option<&'a str> {
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

pub(crate) fn boundary<T>(fallback: T, operation: impl FnOnce() -> T) -> T {
    catch_unwind(AssertUnwindSafe(operation)).unwrap_or(fallback)
}

unsafe extern "C" fn local_time(
    _context: *mut c_void,
    seconds: i64,
    result: *mut ffi::rptadv_local_time_v1,
) -> i32 {
    boundary(-1, || {
        let Some(result) = (unsafe { result.as_mut() }) else {
            return -1;
        };
        if result.struct_size < size_of::<ffi::rptadv_local_time_v1>() as u32 {
            return -1;
        }
        let seconds: ffi::time_t = seconds;
        let mut local = std::mem::MaybeUninit::<ffi::tm>::uninit();
        if unsafe { ffi::localtime_r(&seconds, local.as_mut_ptr()) }.is_null() {
            result.valid = 0;
            return 0;
        }
        let local = unsafe { local.assume_init() };
        let Ok(year) = u16::try_from(i64::from(local.tm_year) + 1900) else {
            result.valid = 0;
            return 0;
        };
        result.valid = 1;
        result.year = year;
        result.month = (local.tm_mon + 1) as u8;
        result.day = local.tm_mday as u8;
        result.weekday = local.tm_wday as u8;
        result.hour = local.tm_hour as u8;
        result.minute = local.tm_min as u8;
        result.second = local.tm_sec as u8;
        0
    })
}
unsafe extern "C" fn command_notice(
    _context: *mut c_void,
    local: *const c_char,
    length: usize,
    completed: u32,
) {
    boundary((), || {
        let Some(local) =
            (unsafe { text(local, length) }).and_then(|value| CString::new(value).ok())
        else {
            return;
        };
        unsafe {
            ffi::ast_log(
                ffi::__LOG_NOTICE as i32,
                c"rust/asterisk/services.rs".as_ptr(),
                0,
                c"command".as_ptr(),
                c"rpt_advanced: node %s link command %s\n".as_ptr(),
                local.as_ptr(),
                if completed != 0 {
                    c"completed"
                } else {
                    c"failed"
                }
                .as_ptr(),
            );
        }
    });
}
unsafe extern "C" fn scheduler_notice(_: *mut c_void, pointer: *const c_char, length: usize) {
    boundary((), || {
        let Some(text) =
            (unsafe { text(pointer, length) }).and_then(|value| CString::new(value).ok())
        else {
            return;
        };
        unsafe {
            ffi::ast_log(
                ffi::__LOG_WARNING as i32,
                c"rust/asterisk/services.rs".as_ptr(),
                0,
                c"scheduler".as_ptr(),
                c"rpt_advanced: %s\n".as_ptr(),
                text.as_ptr(),
            );
        }
    });
}
struct ReservedRadio {
    radio: Option<Radio>,
    name: CString,
}
unsafe extern "C" fn radio_open(
    _context: *mut c_void,
    name: *const c_char,
    length: usize,
    maximum: usize,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        let Some(identity) = (unsafe { text(name, length) }) else {
            return -1;
        };
        let channel = match identity.split_once('\0') {
            Some((node, channel)) if !node.is_empty() && !channel.is_empty() => channel,
            Some(_) => return -1,
            None => identity,
        };
        let Some(channel) = CString::new(channel).ok() else {
            return -1;
        };
        let radio =
            Connection::open(&channel).and_then(|connection| connection.into_radio(maximum));
        match radio {
            Ok(radio) => {
                *output = Box::into_raw(Box::new(ReservedRadio {
                    radio: Some(radio),
                    name: channel,
                }))
                .cast();
                0
            }
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn radio_activate(
    _context: *mut c_void,
    radio: *mut c_void,
    receive: ffi::rptadv_radio_receive_v2,
    receive_context: *mut c_void,
    transmit: ffi::rptadv_radio_transmit_v3,
    transmit_context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(reserved) = (unsafe { radio.cast::<ReservedRadio>().as_mut() }) else {
            return -1;
        };
        let Some(radio) = reserved.radio.as_mut() else {
            return -1;
        };
        let mut direct = ffi::urp_ast_direct_callbacks {
            struct_size: size_of::<ffi::urp_ast_direct_callbacks>() as u32,
            abi_version: ffi::URP_AST_DIRECT_CALLBACKS_ABI_VERSION,
            receive_context,
            receive,
            transmit_context,
            transmit,
            accepted_abi_version: 0,
        };
        if receive.is_some()
            && transmit.is_some()
            && unsafe { radio.attach_direct(&mut direct) }.is_ok()
            && radio.start(&reserved.name).is_ok()
        {
            return 0;
        }
        // A failed option/start may already have copied callbacks. Hangup must quiesce
        // those callbacks before returning failure to their owner.
        drop(reserved.radio.take());
        -1
    })
}
unsafe extern "C" fn radio_destroy(_context: *mut c_void, radio: *mut c_void) {
    boundary((), || {
        if !radio.is_null() {
            unsafe { drop(Box::from_raw(radio.cast::<ReservedRadio>())) };
        }
    });
}
/// Serialize one site option through the already-reserved channel; never open hardware here.
unsafe fn site_control(radio: *mut c_void, request: &mut ffi::UrpAstSiteControl) -> i32 {
    let Some(reserved) = (unsafe { radio.cast::<ReservedRadio>().as_mut() }) else {
        return -1;
    };
    reserved
        .radio
        .as_mut()
        .map_or(-1, |radio| radio.site_control(request))
}

fn site_request(operation: u32, bank: u32) -> ffi::UrpAstSiteControl {
    // SAFETY: this C record contains only integers and nested integer records.
    let mut request: ffi::UrpAstSiteControl = unsafe { std::mem::zeroed() };
    request.struct_size = size_of::<ffi::UrpAstSiteControl>() as u32;
    request.abi_version = ffi::URP_AST_SITE_OPTION_ABI_VERSION;
    request.operation = operation;
    request.bank = bank;
    request
}

unsafe extern "C" fn radio_site_bind(
    _: *mut c_void,
    radio: *mut c_void,
    bank: u32,
    binding: *const ffi::UrpAstSiteBinding,
) -> i32 {
    boundary(-1, || {
        let Some(binding) = (unsafe { binding.as_ref() }) else {
            return -1;
        };
        let mut request = site_request(ffi::URP_AST_SITE_BIND, bank);
        request.binding = *binding;
        unsafe { site_control(radio, &mut request) }
    })
}

unsafe extern "C" fn radio_site_publish(
    _: *mut c_void,
    radio: *mut c_void,
    bank: u32,
    output: *const ffi::UrpAstSiteRequest,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_ref() }) else {
            return -1;
        };
        let mut request = site_request(ffi::URP_AST_SITE_PUBLISH, bank);
        request.request = *output;
        unsafe { site_control(radio, &mut request) }
    })
}

unsafe extern "C" fn radio_site_snapshot(
    _: *mut c_void,
    radio: *mut c_void,
    bank: u32,
    output: *mut ffi::UrpAstSiteSnapshot,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        let mut request = site_request(ffi::URP_AST_SITE_SNAPSHOT, bank);
        request.snapshot = *output;
        let result = unsafe { site_control(radio, &mut request) };
        if result == 0 {
            *output = request.snapshot;
        }
        result
    })
}

unsafe extern "C" fn peer_dial(
    context: *mut c_void,
    destination: *const c_char,
    destination_length: usize,
    local: *const c_char,
    local_length: usize,
    maximum: usize,
    current: ffi::rptadv_current_v1,
    current_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        let values = unsafe {
            (
                text(destination, destination_length),
                text(local, local_length),
            )
        };
        let (Some(destination), Some(local), Some(current)) = (values.0, values.1, current) else {
            return -1;
        };
        if let Some(remote) = destination.strip_prefix("EchoLink/") {
            if unsafe { current(current_context) } == 0 {
                return -1;
            }
            let Some(context) = (unsafe { context.cast::<HostContext>().as_ref() }) else {
                return -1;
            };
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
        let (Ok(destination), Ok(local)) = (CString::new(destination), CString::new(local)) else {
            return -1;
        };
        match PeerIo::dial(&destination, &local, maximum, || unsafe {
            current(current_context) != 0
        }) {
            Ok(peer) => {
                let remote = destination
                    .to_str()
                    .ok()
                    .and_then(|value| value.rsplit_once('/'))
                    .map(|(_, node)| node)
                    .unwrap_or("");
                *output = into_raw(peer, local.to_str().unwrap_or(""), remote);
                0
            }
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn peer_rate(_context: *mut c_void, peer: *const c_void) -> u32 {
    boundary(0, || match unsafe { peer.cast::<PeerHandle>().as_ref() } {
        Some(HostPeer::Echo(_)) => 8000,
        Some(HostPeer::Iax(peer)) => peer.io.rate(),
        None => 0,
    })
}
unsafe extern "C" fn peer_bind_radio(
    _context: *mut c_void,
    peer: *mut c_void,
    radio: *mut c_void,
) -> i32 {
    boundary(-1, || {
        if matches!(
            unsafe { peer.cast::<PeerHandle>().as_ref() },
            Some(HostPeer::Echo(_))
        ) {
            return unsafe { radio.cast::<ReservedRadio>().as_ref() }
                .map_or(-1, |radio| if radio.radio.is_some() { 0 } else { -1 });
        }
        // SAFETY: the product lends uniquely owned peer/radio handles on control.
        let (Some(peer), Some(reserved)) = (unsafe {
            (
                iax_peer_mut(peer).map(|peer| &mut peer.io),
                radio.cast::<ReservedRadio>().as_ref(),
            )
        }) else {
            return -1;
        };
        reserved
            .radio
            .as_ref()
            .map_or(-1, |radio| radio.bind_peer(peer).map_or(-1, |()| 0))
    })
}
unsafe extern "C" fn peer_ready(_context: *mut c_void, peer: *mut c_void) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { iax_peer_mut(peer).map(|peer| &mut peer.io) }) else {
            return -1;
        };
        peer.ready().map_or(-1, i32::from)
    })
}
unsafe extern "C" fn peer_read(
    _context: *mut c_void,
    peer: *mut c_void,
    event: ffi::rptadv_peer_event_v1,
    event_context: *mut c_void,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(event)) = (
            unsafe { iax_peer_mut(peer).map(|peer| &mut peer.io) },
            event,
        ) else {
            return -1;
        };
        peer.read(|input| unsafe {
            match input {
                Input::Text(bytes) => event(event_context, 1, bytes.as_ptr().cast(), bytes.len()),
                Input::Digit(digit) => {
                    let digit = digit as u8;
                    event(event_context, 2, ptr::from_ref(&digit).cast(), 1);
                }
                Input::Audio(samples) => {
                    event(event_context, 3, samples.as_ptr().cast(), samples.len());
                }
            }
        })
        .map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_send_text(
    _context: *mut c_void,
    peer: *mut c_void,
    text_pointer: *const c_char,
    length: usize,
) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { iax_peer_mut(peer).map(|peer| &mut peer.io) }) else {
            return -1;
        };
        let Some(text) = (unsafe { text(text_pointer, length) }) else {
            return -1;
        };
        let Ok(text) = CString::new(text) else {
            return -1;
        };
        peer.send_text(&text).map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_send_digit(_context: *mut c_void, peer: *mut c_void, digit: u8) -> i32 {
    boundary(-1, || {
        unsafe { iax_peer_mut(peer).map(|peer| &mut peer.io) }
            .ok_or(())
            .and_then(|peer| peer.send_digit(digit as char).map_err(|_| ()))
            .map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_write(
    _context: *mut c_void,
    peer: *mut c_void,
    samples: *const f32,
    count: usize,
) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { iax_peer_mut(peer).map(|peer| &mut peer.io) }) else {
            return -1;
        };
        if samples.is_null() && count != 0 {
            return -1;
        }
        let samples = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(samples, count) }
        };
        peer.write(samples).map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_destroy(_context: *mut c_void, peer: *mut c_void) {
    boundary((), || {
        if !peer.is_null() {
            unsafe { drop(Box::from_raw(peer.cast::<PeerHandle>())) };
        }
    });
}

// Only Echo QSOs use provider serial/prepared callbacks here. Native Asterisk
// channels retain their existing PCM worker and never receive these capabilities.
unsafe fn echo_peer<'a>(
    peer: *const c_void,
) -> Option<&'a std::sync::Arc<rptadv_peer_support::echolink::host::Peer>> {
    match unsafe { peer.cast::<PeerHandle>().as_ref() }? {
        HostPeer::Echo(peer) => Some(peer),
        HostPeer::Iax(_) => None,
    }
}

unsafe extern "C" fn peer_step_attach(
    _: *mut c_void,
    peer: *mut c_void,
    step: ffi::rptadv_peer_step_v1,
    step_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    boundary(-1, || {
        let Some(output) = (unsafe { output.as_mut() }) else {
            return -1;
        };
        *output = ptr::null_mut();
        let Some(peer) = (unsafe { echo_peer(peer) }).cloned() else {
            return -1;
        };
        match unsafe { peer.attach(step, step_context) } {
            Ok(token) => {
                *output = Box::into_raw(Box::new(token)).cast();
                0
            }
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn peer_step_stop(_: *mut c_void, token: *mut c_void) {
    if let Some(token) = unsafe { token.cast::<Registration>().as_ref() } {
        token.stop();
    }
}
unsafe extern "C" fn peer_step_wake(_: *mut c_void, token: *mut c_void) {
    if let Some(token) = unsafe { token.cast::<Registration>().as_ref() } {
        token.wake();
    }
}
unsafe extern "C" fn peer_step_detach(_: *mut c_void, token: *mut c_void) {
    if !token.is_null() {
        unsafe { drop(Box::from_raw(token.cast::<Registration>())) };
    }
}
unsafe extern "C" fn peer_set_deadline(_: *mut c_void, peer: *mut c_void, after: u64) -> i32 {
    unsafe { echo_peer(peer) }.map_or(-1, |peer| {
        unsafe { peer.set_deadline(after) }.map_or(-1, |()| 0)
    })
}
unsafe extern "C" fn peer_prepare_audio(
    _: *mut c_void,
    peer: *mut c_void,
    pcm: *const f32,
    count: usize,
    output: *mut u8,
    capacity: usize,
    flush: u32,
    result: *mut ffi::rptadv_codec_encode_result_v1,
) -> i32 {
    boundary(-1, || {
        let Some(peer) = (unsafe { echo_peer(peer) }) else {
            return -1;
        };
        if result.is_null()
            || unsafe { ptr::addr_of!((*result).struct_size).read() }
                < size_of::<ffi::rptadv_codec_encode_result_v1>() as u32
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
        unsafe { peer.prepare_audio(pcm, output, flush, result.cast()) }
            .map_or(-1, |()| unsafe { (*result).status as i32 })
    })
}
unsafe extern "C" fn peer_reset_encoder(_: *mut c_void, peer: *mut c_void) -> i32 {
    unsafe { echo_peer(peer) }.map_or(-1, |peer| {
        unsafe { peer.reset_encoder() }.map_or(-1, |()| 0)
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
        let Some(peer) = (unsafe { echo_peer(peer) }) else {
            return -1;
        };
        if count > isize::MAX as usize || (bytes.is_null() && count != 0) {
            return -1;
        }
        let bytes = if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(bytes, count) }
        };
        match unsafe { peer.send_prepared(bytes, frames, timestamp) } {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(_) => -1,
        }
    })
}
unsafe extern "C" fn peer_elapsed_ms(_: *mut c_void, peer: *mut c_void) -> u32 {
    unsafe { echo_peer(peer) }
        .and_then(|peer| peer.elapsed_ms().ok())
        .map_or(0, |value| value.min(u64::from(u32::MAX)) as u32)
}

unsafe extern "C" fn peer_read_source(
    _: *mut c_void,
    peer: *mut c_void,
    completion: u32,
    sink: *const ffi::rptadv_peer_source_sink_v1,
    text: ffi::rptadv_peer_source_text_v1,
    text_context: *mut c_void,
    output: *mut ffi::rptadv_peer_source_result_v1,
) -> i32 {
    boundary(-1, || {
        if output.is_null()
            || unsafe { ptr::addr_of!((*output).struct_size).read() }
                < size_of::<ffi::rptadv_peer_source_result_v1>() as u32
            || unsafe { ptr::addr_of!((*output).abi_version).read() } != 1
        {
            return -1;
        }
        let Some(peer) = (unsafe { echo_peer(peer) }) else {
            return -1;
        };
        let output = unsafe { &mut *output };
        let sink = unsafe { sink.as_ref() };
        if sink.is_some_and(|sink| {
            sink.struct_size < size_of::<ffi::rptadv_peer_source_sink_v1>() as u32
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
            callback: ffi::rptadv_peer_source_text_v1,
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
                        ffi::rptadv_peer_bytes_v1 {
                            data: sender.data,
                            length: sender.length,
                        },
                        ffi::rptadv_peer_bytes_v1 {
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
                output.represented_node_number = result.represented_node_number;
                0
            }
            Err(_) => -1,
        }
    })
}

/// Stable account context; only control/lifecycle calls inspect this map.
struct HostContext {
    echo: Mutex<Option<Arc<Accounts>>>,
}
impl HostContext {
    fn echo_accounts(&self) -> Option<Arc<Accounts>> {
        self.echo
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

/// Owned immutable host table retained until all product/account callbacks stop.
pub(crate) struct HostServicesOwner {
    context: Box<HostContext>,
    descriptor: Box<ffi::rptadv_host_services_v9>,
}
// The table/context allocations remain stable, and accounts are thread-safe.
unsafe impl Send for HostServicesOwner {}
impl HostServicesOwner {
    /// Allocate the callback context before exposing its immutable table.
    pub(crate) fn new() -> Self {
        let mut context = Box::new(HostContext {
            echo: Mutex::new(None),
        });
        let mut descriptor = Box::new(SERVICES.0);
        descriptor.context = ptr::from_mut(context.as_mut()).cast();
        Self {
            context,
            descriptor,
        }
    }
    /// Borrow the immutable table through successful account/product stop.
    pub(crate) fn descriptor(&self) -> &ffi::rptadv_host_services_v9 {
        &self.descriptor
    }
    /// Prepare resolved accounts while their record spans are borrowed. Store
    /// stable owners before product startup; activation is a separate phase.
    /// # Safety
    /// Product inspection and the preparation's backend code remain loaded
    /// through successful product/account shutdown.
    pub(crate) unsafe fn prepare_echo(
        &mut self,
        product: &ffi::rptadv_product_descriptor_v1,
        configuration: &str,
        mut preparation: Preparation<'_>,
    ) -> Result<(), rptadv_peer_support::echolink::host::Error> {
        unsafe {
            visit_accounts(product, configuration, &mut |config, path| {
                preparation.prepare(config, path)
            })
        }?;
        *self
            .context
            .echo
            .get_mut()
            .unwrap_or_else(|error| error.into_inner()) = preparation.finish().map(Arc::new);
        Ok(())
    }

    /// Apply the shared account/product transaction without retaining a host
    /// context lock over a provider callback or product reload.
    /// # Safety
    /// Same lifetime/off-executor contract as preparation and account stop.
    pub(crate) unsafe fn reload_echo(
        &self,
        product: &ffi::rptadv_product_descriptor_v1,
        configuration: &str,
        preparation: Preparation<'_>,
        apply: impl FnOnce() -> bool,
        restore: impl FnOnce() -> bool,
    ) -> Result<UpdateOutcome, UpdateError> {
        let (accounts, outcome) = unsafe {
            preparation.reload(
                self.context.echo_accounts(),
                |visit| visit_accounts(product, configuration, visit),
                apply,
                restore,
            )
        }?;
        *self
            .context
            .echo
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = accounts;
        Ok(outcome)
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
}

unsafe fn visit_accounts(
    product: &ffi::rptadv_product_descriptor_v1,
    configuration: &str,
    mut visit: &mut AccountVisitor<'_>,
) -> Result<(), EchoError> {
    let inspect = product.inspect_configuration.ok_or(EchoError::Contract)?;
    let result = unsafe {
        inspect(
            configuration.as_ptr().cast(),
            configuration.len(),
            Some(prepare_account),
            ptr::from_mut(&mut visit).cast(),
            None,
            ptr::null_mut(),
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(EchoError::Contract)
    }
}

unsafe extern "C" fn prepare_account(
    context: *mut c_void,
    node: *const ffi::rptadv_node_host_configuration,
) -> i32 {
    boundary(-1, || {
        let Some(node) = (unsafe { node.as_ref() }) else {
            return -1;
        };
        if node.struct_size as usize != size_of::<ffi::rptadv_node_host_configuration>() {
            return -1;
        }
        let Some(config) = (unsafe { node.echolink.cast::<echo::AccountConfig>().as_ref() }) else {
            return -1;
        };
        let Some(path) = (unsafe {
            text(
                node.echolink_secrets_file.cast(),
                node.echolink_secrets_file_length,
            )
        }) else {
            return -1;
        };
        let visit = unsafe { &mut *context.cast::<&mut AccountVisitor<'_>>() };
        visit(
            config,
            (!path.is_empty()).then(|| std::path::Path::new(path)),
        )
        .map_or(-1, |()| 0)
    })
}

/// Transfer an already claimed incoming QSO using this host's actual enum layout.
/// # Safety
/// Product code remains callable through the account's admission-drain barrier.
pub(crate) unsafe fn incoming_echo(
    incoming: unsafe extern "C" fn(*const ffi::rptadv_incoming_peer_v1, *mut c_void) -> i32,
    peer: Arc<rptadv_peer_support::echolink::host::Peer>,
) {
    let Some(source) = peer.identity().source else {
        return;
    };
    let source = source.ip().to_string();
    let bytes = |text: &str| ffi::rptadv_peer_bytes_v1 {
        data: text.as_ptr(),
        length: text.len(),
    };
    let request = ffi::rptadv_incoming_peer_v1 {
        struct_size: size_of::<ffi::rptadv_incoming_peer_v1>() as u32,
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

struct Services(ffi::rptadv_host_services_v9);
// SAFETY: the table is immutable, its context is null, and object callbacks serialize each handle.
unsafe impl Sync for Services {}

static SERVICES: Services = Services(ffi::rptadv_host_services_v9 {
    struct_size: size_of::<ffi::rptadv_host_services_v9>() as u32,
    abi_version: 9,
    capability: *b"rptadv.hst9\0",
    context: ptr::null_mut(),
    local_time: Some(local_time),
    command_notice: Some(command_notice),
    reaper_acquire: Some(ffi::ast_replace_sigchld),
    reaper_release: Some(ffi::ast_unreplace_sigchld),
    directory_record: Some(directory::directory_record),
    directory_srv: Some(directory::directory_srv),
    directory_addresses: Some(directory::directory_addresses),
    directory_notice: Some(directory::directory_notice),
    radio_open: Some(radio_open),
    radio_activate: Some(radio_activate),
    radio_destroy: Some(radio_destroy),
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
    peer_write: Some(peer_write),
    peer_destroy: Some(peer_destroy),
    peer_step_attach: Some(peer_step_attach),
    peer_step_stop: Some(peer_step_stop),
    peer_step_detach: Some(peer_step_detach),
    peer_observe: None,
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

/// Return the immutable host-services table retained by the adapter DSO.
#[cfg(test)]
pub fn descriptor() -> &'static ffi::rptadv_host_services_v9 {
    &SERVICES.0
}

/// Transfer one raw peer owner to the portable product.
pub fn into_raw(peer: PeerIo, local: &str, remote: &str) -> *mut c_void {
    Box::into_raw(Box::new(PeerHandle::Iax(IaxHandle {
        io: peer,
        local: local.into(),
        remote: remote.into(),
    })))
    .cast()
}

type PeerHandle = HostPeer<IaxHandle>;
struct IaxHandle {
    io: PeerIo,
    local: String,
    remote: String,
}

// Native channel operations lend only their original uniquely owned IAX payload.
// Echo uses the product mix and must never be cast to an Asterisk channel owner.
unsafe fn iax_peer_mut<'a>(peer: *mut c_void) -> Option<&'a mut IaxHandle> {
    match unsafe { peer.cast::<PeerHandle>().as_mut() }? {
        HostPeer::Iax(peer) => Some(peer),
        HostPeer::Echo(_) => None,
    }
}

unsafe extern "C" fn peer_info(
    _: *mut c_void,
    peer: *const c_void,
    output: *mut ffi::rptadv_peer_info_v1,
) -> i32 {
    boundary(-1, || {
        let (Some(peer), Some(output)) = (unsafe { peer.cast::<PeerHandle>().as_ref() }, unsafe {
            output.as_mut()
        }) else {
            return -1;
        };
        if output.struct_size < size_of::<ffi::rptadv_peer_info_v1>() as u32
            || output.abi_version != 1
        {
            return -1;
        }
        let (protocol, capabilities, account_generation, qso_generation, remote, local, callsign) =
            match peer {
                HostPeer::Iax(peer) => (1, 12, 0, 0, peer.remote.as_str(), peer.local.as_str(), ""),
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
        let bytes = |text: &str| ffi::rptadv_peer_bytes_v1 {
            data: if text.is_empty() {
                ptr::null()
            } else {
                text.as_ptr()
            },
            length: text.len(),
        };
        *output = ffi::rptadv_peer_info_v1 {
            struct_size: size_of::<ffi::rptadv_peer_info_v1>() as u32,
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

/// Release a peer retained by the adapter after product admission declined ownership.
///
/// # Safety
/// `peer` must be the unique pointer returned by [`into_raw`] and not yet transferred.
pub unsafe fn destroy_raw(peer: *mut c_void) {
    unsafe { peer_destroy(ptr::null_mut(), peer) };
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
