//! Deterministic ASL PCM edge for the actual Product account transaction fixture.

use crate::abi;
use std::{collections::BTreeMap, ffi::c_void, ptr, sync::Mutex};

type Identity = abi::rptadv_asl_identity_v1;
static ACTIVE: Mutex<BTreeMap<usize, Vec<Identity>>> = Mutex::new(BTreeMap::new());

struct Selection {
    previous: Vec<Identity>,
    next: Vec<Identity>,
}

pub(crate) static PROVIDER: abi::rptadv_asl_provider_v2 = abi::rptadv_asl_provider_v2 {
    struct_size: size_of::<abi::rptadv_asl_provider_v2>() as u32,
    abi_version: 2,
    media_kind: 2,
    reserved: 0,
    prepare: Some(prepare),
    step: Some(step),
    observe: Some(observe),
    originate: Some(originate),
};

unsafe extern "C" fn prepare(
    context: *mut c_void,
    basis: u64,
    _: u64,
    input: *const abi::rptadv_asl_account_v2,
    count: usize,
    _: *const abi::rptadv_asl_admission_v1,
    result: *mut *mut c_void,
) -> i32 {
    // SAFETY: Product retains the complete row slice and unique output for prepare.
    let Some(result) = (unsafe { result.as_mut() }) else {
        return -1;
    };
    *result = ptr::null_mut();
    let rows = if count == 0 {
        &[]
    } else {
        if input.is_null() {
            return -1;
        }
        unsafe { std::slice::from_raw_parts(input, count) }
    };
    let next = rows.iter().map(|row| row.id).collect();
    let previous = if basis == 0 {
        Vec::new()
    } else {
        ACTIVE
            .lock()
            .unwrap()
            .get(&(context as usize))
            .cloned()
            .unwrap_or_default()
    };
    *result = Box::into_raw(Box::new(Selection { previous, next })).cast();
    0
}

unsafe extern "C" fn step(context: *mut c_void, token: *mut c_void, phase: u32) -> i32 {
    if phase == 4 {
        // SAFETY: the final acknowledgment consumes the unique fixture selection.
        drop(unsafe { Box::from_raw(token.cast::<Selection>()) });
        return 0;
    }
    // SAFETY: Product retains this original token for each reversible phase.
    let selection = unsafe { &*token.cast::<Selection>() };
    let rows = match phase {
        1 => return 0,
        2 => &selection.next,
        3 => &selection.previous,
        _ => return -1,
    };
    let mut active = ACTIVE.lock().unwrap();
    if rows.is_empty() {
        active.remove(&(context as usize));
    } else {
        active.insert(context as usize, rows.clone());
    }
    0
}

unsafe extern "C" fn observe(
    context: *mut c_void,
    identity: *const Identity,
    state: *mut u32,
) -> i32 {
    // SAFETY: Product lends one complete identity and unique scalar output.
    let (Some(identity), Some(state)) = (unsafe { identity.as_ref() }, unsafe { state.as_mut() })
    else {
        return -1;
    };
    let current = ACTIVE
        .lock()
        .unwrap()
        .get(&(context as usize))
        .is_some_and(|rows| {
            rows.iter().any(|row| {
                row.station_uuid == identity.station_uuid
                    && row.account_uuid == identity.account_uuid
                    && row.generation == identity.generation
                    && row.wire == identity.wire
                    && row.udp_port == identity.udp_port
                    && identity.reserved == 0
            })
        });
    *state = 3 * u32::from(current);
    if current { 0 } else { -1 }
}

unsafe extern "C" fn originate(
    context: *mut c_void,
    identity: *const Identity,
    remote: abi::rptadv_peer_bytes_v1,
    _: abi::rptadv_peer_bytes_v1,
    maximum: usize,
    current: abi::rptadv_current_v1,
    current_context: *mut c_void,
    output: *mut *mut c_void,
) -> i32 {
    let mut seen = 0;
    // SAFETY: the checked typed client lends its current identity and exact output.
    if unsafe { observe(context, identity, &mut seen) } != 0 {
        return -1;
    }
    let local = unsafe { &*identity }.wire.to_string();
    // SAFETY: the original deterministic peer callback borrows the spans synchronously.
    unsafe {
        super::peer_dial(
            context,
            remote.data.cast(),
            remote.length,
            local.as_ptr().cast(),
            local.len(),
            maximum,
            current,
            current_context,
            output,
        )
    }
}
