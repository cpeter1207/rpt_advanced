//! Core task integration; neutral descriptor/ownership cases live in peer-support.
use super::*;
use std::{
    mem::{align_of, offset_of, size_of},
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
