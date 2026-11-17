use super::*;
use crate::resolve_radio_nodes;
use std::{cell::Cell, mem::size_of_val, ptr};

#[repr(C)]
struct Request {
    control: crate::abi::rptadv_radio_control_v1,
    retirement: crate::abi::UrpAstProgrammeRetirement,
}

thread_local! {
    static STATUS: Cell<i32> = const { Cell::new(0) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn retire(handle: *mut c_void, generation: u64, request: u64) -> i32 {
    assert_eq!(handle, std::ptr::NonNull::<u8>::dangling().as_ptr().cast());
    assert_eq!((generation, request), (7, 11));
    CALLS.set(CALLS.get() + 1);
    STATUS.get()
}

fn request() -> Request {
    let mut value: Request = unsafe { std::mem::zeroed() };
    value.control.struct_size = size_of_val(&value) as u32;
    value.control.abi_version = 1;
    value.control.operation = 8;
    value.retirement.struct_size = size_of_val(&value.retirement) as u32;
    value.retirement.abi_version = 1;
    value.retirement.expected_radio_generation = 7;
    value.retirement.request_id = 11;
    value.retirement.result = -7;
    value
}

#[test]
fn programme_retirement_uses_existing_native_owner_and_preserves_actual_sdk_result() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut api = crate::radio_activation::tests::remote_fixture_api();
    api.native_programme_retire = Some(retire);
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: Some(crate::radio_activation::tests::fixture_active_radio(api)),
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    for code in [0, 1, -9] {
        STATUS.set(code);
        CALLS.set(0);
        let mut value = request();
        assert_eq!(
            unsafe {
                radio_control(
                    ptr::null_mut(),
                    ptr::from_mut(&mut handle).cast(),
                    &mut value.control,
                    None,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(
            value.retirement.accepted_abi_version, 1,
            "only actual dispatch acknowledges the option"
        );
        assert_eq!(
            value.retirement.result, code,
            "pending and failure are not completion"
        );
        assert_eq!(CALLS.get(), 1);
    }
}

#[test]
fn programme_retirement_rejects_partial_or_unsupported_native_requests() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: None,
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    let invoke = |handle: &mut RadioHandle, value: &mut crate::abi::rptadv_radio_control_v1| unsafe {
        radio_control(
            ptr::null_mut(),
            ptr::from_mut(handle).cast(),
            value,
            None,
            ptr::null_mut(),
        )
    };
    for defect in 0..6 {
        let mut value = request();
        match defect {
            0 => value.control.struct_size -= 1,
            1 => value.control.abi_version = 2,
            2 => value.retirement.struct_size -= 1,
            3 => value.retirement.abi_version = 2,
            4 => value.retirement.expected_radio_generation = 0,
            _ => value.retirement.request_id = 0,
        }
        assert_eq!(invoke(&mut handle, &mut value.control), -1);
    }
    // The object really ends after the base control prefix.
    let mut prefix = request().control;
    prefix.struct_size = size_of_val(&prefix) as u32;
    assert_eq!(invoke(&mut handle, &mut prefix), -1);
    let mut value = request();
    assert_eq!(invoke(&mut handle, &mut value.control), -7);
    handle.active = Some(crate::radio_activation::tests::fixture_active_radio(
        crate::radio_activation::tests::remote_fixture_api(),
    ));
    assert_eq!(invoke(&mut handle, &mut value.control), -7);
    assert_eq!(
        (
            value.retirement.accepted_abi_version,
            value.retirement.result
        ),
        (0, -7)
    );
}

#[repr(C)]
struct FenceRequest {
    control: crate::abi::rptadv_radio_control_v1,
    binding: crate::abi::UrpProgrammeFenceBinding,
    tail: u64,
}

fn fence_request() -> FenceRequest {
    // SAFETY: this ABI record contains only scalars and nullable pointers.
    let mut value: FenceRequest = unsafe { std::mem::zeroed() };
    value.control.struct_size = size_of_val(&value) as u32;
    value.control.abi_version = 1;
    value.control.operation = 9;
    value.binding.struct_size = size_of_val(&value.binding) as u32;
    value.binding.abi_version = 1;
    value.tail = 0xabcdef;
    value
}

#[test]
fn programme_fence_staging_is_not_sdk_retention_acknowledgment() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: None,
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    let mut request = fence_request();
    let status = unsafe {
        radio_control(
            ptr::null_mut(),
            ptr::from_mut(&mut handle).cast(),
            &mut request.control,
            None,
            ptr::null_mut(),
        )
    };
    assert_eq!(status, 1, "staging has a distinct non-ACK result");
    assert_eq!(request.binding.accepted_abi_version, 0);
    assert_eq!(request.tail, 0xabcdef);
}

thread_local! {
    static FENCE_FAULT: Cell<u8> = const { Cell::new(0) };
    static FENCE_CALLS: Cell<usize> = const { Cell::new(0) };
}

unsafe extern "C" fn bind_fence(
    _: *mut c_void,
    binding: *mut crate::abi::UrpProgrammeFenceBinding,
) -> i32 {
    if unsafe { (*binding).abi_version } == 2 {
        let binding = unsafe { &mut *binding.cast::<crate::abi::UrpProgrammeFenceBindingV2>() };
        assert_eq!(binding.base.struct_size, 40);
        assert_eq!(binding.base.accepted_abi_version, 0);
        FENCE_CALLS.set(FENCE_CALLS.get() + 1);
        binding.base.accepted_abi_version = 2;
        match FENCE_FAULT.get() {
            1 => return -9,
            2 => binding.base.accepted_abi_version = 0,
            3 => binding.base.accepted_abi_version = 1,
            4 => binding.base.struct_size -= 1,
            5 => binding.base.abi_version = 1,
            6 => binding.base.context = ptr::null_mut(),
            7 => binding.base.reserved = 1,
            8 => return 1,
            9 => binding.observed_permit = None,
            10 => binding.base.permit = Some(legacy_permit),
            11 => return -7, // Old SDK rejects V2 but still accepts V1 below.
            _ => {}
        }
        return 0;
    }
    let binding = unsafe { &mut *binding };
    assert_eq!(binding.accepted_abi_version, 0);
    FENCE_CALLS.set(FENCE_CALLS.get() + 1);
    binding.accepted_abi_version = 1;
    match FENCE_FAULT.get() {
        1 => return -9,
        2 => binding.accepted_abi_version = 0,
        3 => binding.accepted_abi_version = 2,
        4 => binding.struct_size -= 1,
        5 => binding.abi_version = 2,
        6 => binding.context = ptr::dangling_mut::<u8>().cast(),
        7 => binding.reserved = 1,
        8 => return 1,
        _ => {}
    }
    0
}

#[test]
fn programme_fence_requires_confirmed_stop_and_exact_sdk_ack() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut api = crate::radio_activation::tests::remote_fixture_api();
    api.native_programme_fence_bind = Some(bind_fence);
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: Some(crate::radio_activation::tests::fixture_active_radio(api)),
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    let invoke = |handle: &mut RadioHandle, value: &mut FenceRequest| unsafe {
        radio_control(
            ptr::null_mut(),
            ptr::from_mut(handle).cast(),
            &mut value.control,
            None,
            ptr::null_mut(),
        )
    };
    FENCE_CALLS.set(0);
    assert_eq!(invoke(&mut handle, &mut fence_request()), -1);
    assert_eq!(FENCE_CALLS.get(), 0);
    assert_eq!(
        unsafe { radio_quiesce(ptr::null_mut(), ptr::from_mut(&mut handle).cast()) },
        0
    );
    for fault in 0..9 {
        FENCE_FAULT.set(fault);
        let mut request = fence_request();
        assert_eq!(
            invoke(&mut handle, &mut request),
            if fault == 0 {
                0
            } else if fault == 1 {
                -9
            } else {
                -7
            }
        );
        assert_eq!(request.binding.accepted_abi_version, u32::from(fault == 0));
        assert_eq!(
            request.binding.struct_size as usize,
            size_of_val(&request.binding)
        );
        assert_eq!(request.binding.abi_version, 1);
        assert!(request.binding.context.is_null());
        assert_eq!(request.binding.reserved, 0);
        assert_eq!(request.tail, 0xabcdef);
    }
    FENCE_FAULT.set(0);
}

#[test]
fn programme_fence_validates_complete_nested_extent_before_staging() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: None,
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    for fault in 0..7 {
        let mut request = fence_request();
        match fault {
            0 => request.control.struct_size = size_of_val(&request.control) as u32,
            1 => request.control.abi_version = 2,
            2 => request.binding.struct_size -= 1,
            3 => request.binding.struct_size = u32::MAX,
            4 => request.binding.abi_version = 2,
            5 => request.binding.reserved = 1,
            _ => request.binding.accepted_abi_version = 1,
        }
        assert_eq!(
            unsafe {
                radio_control(
                    ptr::null_mut(),
                    ptr::from_mut(&mut handle).cast(),
                    &mut request.control,
                    None,
                    ptr::null_mut(),
                )
            },
            -1
        );
    }
}

#[test]
fn programme_fence_staged_request_is_copied_and_larger_tails_are_untouched() {
    unsafe extern "C" fn permit(_: *mut c_void, _: u64, _: u32) -> u32 {
        0
    }
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: None,
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    let mut context = Box::new(7_u64);
    {
        let mut value = fence_request();
        value.binding.struct_size += 8;
        value.binding.context = ptr::from_mut(&mut *context).cast();
        value.binding.permit = Some(permit);
        assert_eq!(
            unsafe {
                radio_control(
                    ptr::null_mut(),
                    ptr::from_mut(&mut handle).cast(),
                    &mut value.control,
                    None,
                    ptr::null_mut(),
                )
            },
            1
        );
        assert_eq!(value.binding.accepted_abi_version, 0);
        assert_eq!(value.tail, 0xabcdef);
        value.binding.context = ptr::null_mut();
    }
    let retained = handle.fence.unwrap();
    let retained = retained.base();
    assert_eq!(retained.context, ptr::from_mut(&mut *context).cast());
    assert_eq!(retained.struct_size as usize, size_of_val(retained));
    assert_eq!(retained.accepted_abi_version, 0);
    assert_eq!(
        unsafe { retained.permit.unwrap()(retained.context, 73, 128) },
        0
    );
    drop(handle);
    drop(context);
}

#[repr(C)]
struct ObservedRequest {
    control: crate::abi::rptadv_radio_control_v1,
    binding: crate::abi::UrpProgrammeFenceBindingV2,
    tail: u64,
}
unsafe extern "C" fn observed_permit(
    context: *mut c_void,
    _: *const crate::abi::UrpProgrammeObservationV1,
) -> u32 {
    unsafe { *context.cast::<u32>() }
}
unsafe extern "C" fn legacy_permit(_: *mut c_void, _: u64, _: u32) -> u32 {
    0
}
fn observed_request(context: *mut c_void) -> ObservedRequest {
    let mut value: ObservedRequest = unsafe { std::mem::zeroed() };
    value.control.struct_size = size_of_val(&value) as u32;
    value.control.abi_version = 1;
    value.control.operation = 9;
    value.binding.base.struct_size = 40;
    value.binding.base.abi_version = 2;
    value.binding.base.context = context;
    value.binding.observed_permit = Some(observed_permit);
    value.tail = 0x123456789abcdef;
    value
}

#[test]
fn stage_d_observed_binding_contract_staging_and_extent() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: None,
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    let mut context = Box::new(1_u32);
    for fault in 0..10 {
        let mut value = observed_request(ptr::from_mut(&mut *context).cast());
        match fault {
            1 => value.control.struct_size -= 9,
            2 => value.binding.base.struct_size = 39,
            3 => value.binding.base.struct_size = 49,
            4 => value.binding.base.abi_version = 3,
            5 => value.binding.base.permit = Some(legacy_permit),
            6 => value.binding.base.reserved = 1,
            7 => value.binding.base.accepted_abi_version = 2,
            8 => value.binding.base.struct_size = 48,
            9 => value.binding.observed_permit = None,
            _ => {}
        }
        let result = unsafe {
            radio_control(
                ptr::null_mut(),
                ptr::from_mut(&mut handle).cast(),
                ptr::from_mut(&mut value).cast(),
                None,
                ptr::null_mut(),
            )
        };
        assert_eq!(
            result,
            if matches!(fault, 0 | 8 | 9) { 1 } else { -1 },
            "fault {fault}"
        );
        if matches!(fault, 0 | 8) {
            value.binding.observed_permit = None;
            value.binding.base.context = ptr::null_mut();
            let Some(crate::radio_activation::ProgrammeFenceBinding::Observed(retained)) =
                handle.fence
            else {
                panic!("V2 storage must retain its discriminator and complete callback");
            };
            assert_eq!(retained.base.struct_size, 40);
            assert_eq!(retained.base.accepted_abi_version, 0);
            assert_eq!(
                unsafe { retained.observed_permit.unwrap()(retained.base.context, ptr::null()) },
                1
            );
        }
        assert_eq!(value.tail, 0x123456789abcdef);
        assert_eq!(
            value.binding.base.accepted_abi_version,
            if fault == 7 { 2 } else { 0 }
        );
    }
}

#[test]
fn stage_d_observed_binding_contract_stopped_sdk_ack() {
    let radios = resolve_radio_nodes("[1000]\n[radio 1000]\n").unwrap();
    let mut api = crate::radio_activation::tests::remote_fixture_api();
    api.native_programme_fence_bind = Some(bind_fence);
    let mut handle = RadioHandle {
        reservation: reserve_radio(&radios, "1000", 960).unwrap(),
        active: Some(crate::radio_activation::tests::fixture_active_radio(api)),
        pending: None,
        metadata: None,
        feedback: None,
        fence: None,
    };
    let mut context = Box::new(1_u32);
    let invoke = |handle: &mut RadioHandle, value: &mut ObservedRequest| unsafe {
        radio_control(
            ptr::null_mut(),
            ptr::from_mut(handle).cast(),
            ptr::from_mut(value).cast(),
            None,
            ptr::null_mut(),
        )
    };
    FENCE_CALLS.set(0);
    assert_eq!(
        invoke(
            &mut handle,
            &mut observed_request(ptr::from_mut(&mut *context).cast())
        ),
        -1
    );
    assert_eq!(FENCE_CALLS.get(), 0);
    assert_eq!(
        unsafe { radio_quiesce(ptr::null_mut(), ptr::from_mut(&mut handle).cast()) },
        0
    );
    for fault in 0..12 {
        FENCE_FAULT.set(fault);
        FENCE_CALLS.set(0);
        let mut value = observed_request(ptr::from_mut(&mut *context).cast());
        value.binding.base.struct_size = 48;
        assert_eq!(
            invoke(&mut handle, &mut value),
            match fault {
                0 => 0,
                1 => -9,
                _ => -7,
            },
            "fault {fault}"
        );
        assert_eq!(FENCE_CALLS.get(), 1, "never retry a V2 request as V1");
        assert_eq!(
            value.binding.base.accepted_abi_version,
            if fault == 0 { 2 } else { 0 }
        );
        assert_eq!(value.binding.base.struct_size, 48);
        assert_eq!(
            value.binding.base.context,
            ptr::from_mut(&mut *context).cast()
        );
        assert_eq!(
            value.binding.observed_permit.map(|f| f as usize),
            Some(observed_permit as usize)
        );
        assert_eq!(value.tail, 0x123456789abcdef);
    }
    let mut legacy = fence_request();
    FENCE_CALLS.set(0);
    assert_eq!(
        unsafe {
            radio_control(
                ptr::null_mut(),
                ptr::from_mut(&mut handle).cast(),
                ptr::from_mut(&mut legacy).cast(),
                None,
                ptr::null_mut(),
            )
        },
        0
    );
    assert_eq!(
        legacy.binding.accepted_abi_version, 1,
        "same old SDK still supports V1"
    );
    assert_eq!(FENCE_CALLS.get(), 1);
    FENCE_FAULT.set(0);
}
