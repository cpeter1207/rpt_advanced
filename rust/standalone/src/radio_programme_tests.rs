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
