use super::*;

#[test]
fn physically_short_host_prefix_omits_feedback_without_borrowing_a_larger_table() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    let prefix = std::mem::offset_of!(abi::rptadv_host_services_v10, radio_feedback_bind);
    assert_eq!(prefix, 400);
    table.struct_size = prefix as u32;
    let mut old = [0_u64; 50];
    // SAFETY: the aligned destination has exactly the old complete table size.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::from_ref(&table).cast::<u8>(),
            old.as_mut_ptr().cast(),
            prefix,
        )
    };
    let services = unsafe { HostServices::open(old.as_ptr().cast()) }.unwrap();
    assert!(services.0.radio_feedback_bind.is_none());
    table.struct_size = (prefix + 4) as u32;
    assert!(matches!(
        unsafe { HostServices::open(&table) },
        Err(Error::Admission)
    ));
}

#[test]
fn stopped_feedback_binding_preserves_the_borrowed_port_and_reports_capability() {
    unsafe extern "C" fn publish(_: *mut c_void, _: *const abi::UrpRadioFeedback) {}
    unsafe extern "C" fn bind(
        _: *mut c_void,
        radio: *mut c_void,
        port: *const abi::UrpRadioFeedbackPort,
    ) -> i32 {
        assert!(!radio.is_null());
        // SAFETY: the unique radio owner lends this complete port synchronously.
        let port = unsafe { &*port };
        assert_eq!(port.struct_size as usize, size_of_val(port));
        assert_eq!(port.abi_version, 1);
        assert!(port.publish.is_some());
        if port.context.is_null() { 0 } else { -7 }
    }
    let port = abi::UrpRadioFeedbackPort {
        struct_size: size_of::<abi::UrpRadioFeedbackPort>() as u32,
        abi_version: 1,
        context: ptr::null_mut(),
        publish: Some(publish),
    };
    let ordinary = host_services(|table| table.radio_feedback_bind = None);
    let mut radio = ordinary.radio("1000", "usb", 8).unwrap();
    assert_eq!(unsafe { radio.bind_feedback(&port) }, Ok(false));
    drop(radio);
    let capable = host_services(|table| table.radio_feedback_bind = Some(bind));
    let mut radio = capable.radio("1000", "usb", 8).unwrap();
    assert_eq!(unsafe { radio.bind_feedback(&port) }, Ok(true));
    let invalid = abi::UrpRadioFeedbackPort {
        context: ptr::dangling_mut::<u8>().cast(),
        ..port
    };
    assert_eq!(
        unsafe { radio.bind_feedback(&invalid) },
        Err(Error::Operation)
    );
}

#[test]
fn timed_egress_tail_preserves_physically_short_408_byte_host() {
    let mut table = unsafe { crate::fixture::host_descriptor().read() };
    let prefix = std::mem::offset_of!(abi::rptadv_host_services_v10, peer_send_prepared_timed);
    assert_eq!(prefix, 408);
    table.struct_size = prefix as u32;
    let mut old = [0_u64; 51];
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::from_ref(&table).cast::<u8>(),
            old.as_mut_ptr().cast(),
            prefix,
        );
    }
    let services = unsafe { HostServices::open(old.as_ptr().cast()) }.unwrap();
    assert!(services.0.peer_send_prepared_timed.is_none());
    for length in 409..416 {
        table.struct_size = length;
        assert!(matches!(
            unsafe { HostServices::open(&table) },
            Err(Error::Admission)
        ));
    }
}
