//! Core task integration; neutral descriptor/ownership cases live in peer-support.
use super::*;
use std::{
    mem::{align_of, offset_of, size_of},
    ptr,
    sync::mpsc,
    time::Duration,
};

#[test]
fn standalone_provider_runs_through_the_product_control_client() {
    assert_eq!(size_of::<Descriptor>(), size_of::<control::Descriptor>());
    assert_eq!(align_of::<Descriptor>(), align_of::<control::Descriptor>());
    macro_rules! fields {
        ($left:ty, $right:ty; $($field:ident),+) => { $(assert_eq!(offset_of!($left, $field), offset_of!($right, $field));)+ };
    }
    fields!(Descriptor, control::Descriptor; abi_version, struct_size, capability, open, submit, stop_and_drain, close);
    type GeneratedTask = crate::abi::rptadv_control_task_v1;
    assert_eq!(size_of::<GeneratedTask>(), size_of::<control::Task>());
    assert_eq!(align_of::<GeneratedTask>(), align_of::<control::Task>());
    fields!(GeneratedTask, control::Task; context, run, release);
    let provider = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
    let client = unsafe { ControlClient::open(provider.cast(), "product-test", 2) }.unwrap();
    let (send, receive) = mpsc::channel();
    assert!(
        client
            .submit(ControlTask::lifecycle(move || send.send(()).unwrap()))
            .is_ok()
    );
    receive
        .recv_timeout(Duration::from_secs(2))
        .expect("product task executes on standalone control provider");
    client.stop_and_drain().unwrap();
}

#[test]
fn rejected_control_work_retains_its_task_and_distinguishes_full_from_backend_failure() {
    unsafe extern "C" fn reject(_: *mut std::ffi::c_void, _: control::Task) -> i32 {
        -1
    }
    for failed_backend in [false, true] {
        let api = rptadv_control_standalone_adapter::rptadv_control_standalone_descriptor_v1();
        let mut descriptor = unsafe { *api.cast::<control::Descriptor>() };
        if failed_backend {
            descriptor.submit = Some(reject);
        }
        let client = unsafe {
            ControlClient::open(ptr::from_ref(&descriptor).cast(), "product-rejection", 1)
        }
        .unwrap();
        let (entered, ready) = mpsc::channel();
        let (release, held) = mpsc::channel();
        if !failed_backend {
            assert!(
                client
                    .submit(ControlTask::lifecycle(move || {
                        entered.send(()).unwrap();
                        held.recv_timeout(Duration::from_secs(2)).unwrap();
                    }))
                    .is_ok()
            );
            ready.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let (send, received) = mpsc::channel();
        let rejected = client
            .submit(ControlTask::lifecycle(move || send.send(()).unwrap()))
            .err()
            .unwrap();
        assert_eq!(
            rejected.reason,
            if failed_backend {
                RejectionReason::Backend
            } else {
                RejectionReason::Full
            }
        );
        assert!(received.try_recv().is_err());
        rejected.task.run();
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        if !failed_backend {
            release.send(()).unwrap();
        }
        client.stop_and_drain().unwrap();
        let rejected = client
            .submit(ControlTask::lifecycle(|| panic!("stopped work ran")))
            .err()
            .unwrap();
        assert_eq!(rejected.reason, RejectionReason::Stopped);
    }
}
