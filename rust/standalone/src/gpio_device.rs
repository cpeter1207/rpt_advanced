//! Direct ownership of one CM119 HID/GPIO device.

use crate::{ResolvedRadioNode, abi, providers::ResolvedCm119Device};
use std::{ffi::CString, ptr::NonNull};

/// A CM119 GPIO adapter operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpioError {
    /// The adapter is missing a required operation.
    IncompleteAdapter,
    /// The selected USB interface path cannot be represented by the C ABI.
    InvalidIdentity,
    /// The adapter rejected the CM119 configuration.
    InvalidConfiguration,
    /// The adapter rejected a control operation.
    Adapter(i32),
    /// The adapter returned a malformed input snapshot.
    InvalidSnapshot,
    /// The GPIO service thread could not be started.
    ThreadStart,
}

impl std::fmt::Display for GpioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IncompleteAdapter => formatter.write_str("CM119 GPIO adapter is incomplete"),
            Self::InvalidIdentity => formatter.write_str("invalid CM119 USB identity"),
            Self::InvalidConfiguration => {
                formatter.write_str("CM119 GPIO adapter rejected configuration")
            }
            Self::Adapter(code) => write!(formatter, "CM119 GPIO operation failed ({code})"),
            Self::InvalidSnapshot => formatter.write_str("CM119 GPIO input snapshot is invalid"),
            Self::ThreadStart => formatter.write_str("cannot start CM119 GPIO service thread"),
        }
    }
}

impl std::error::Error for GpioError {}

/// One uniquely owned CM119 GPIO device; service and reads stay off audio callbacks.
pub struct Cm119GpioDevice {
    publish: unsafe extern "C" fn(
        *mut abi::rptadv_gpio_device,
        *const abi::rptadv_gpio_output_action,
    ) -> i32,
    service: unsafe extern "C" fn(*mut abi::rptadv_gpio_device) -> i32,
    get_inputs: unsafe extern "C" fn(
        *const abi::rptadv_gpio_device,
        *mut abi::rptadv_gpio_input_snapshot,
    ) -> i32,
    close: unsafe extern "C" fn(*mut abi::rptadv_gpio_device),
    handle: NonNull<abi::rptadv_gpio_device>,
    output_enable_mask: u8,
    initial_output_mask: u8,
}

// SAFETY: the device handle is uniquely owned. Moving it to the single GPIO service
// thread does not change its identity; callers must not access it concurrently.
unsafe impl Send for Cm119GpioDevice {}

impl Cm119GpioDevice {
    /// Open the GPIO interface belonging to the same resolved USB device as PortAudio.
    pub fn open(
        api: &abi::rptadv_gpio_adapter_descriptor,
        radio: &ResolvedRadioNode,
        device: &ResolvedCm119Device,
    ) -> Result<Self, GpioError> {
        let (Some(open), Some(publish), Some(service), Some(get_inputs), Some(close)) = (
            api.device_open,
            api.device_publish_outputs,
            api.device_service,
            api.device_get_inputs,
            api.device_close,
        ) else {
            return Err(GpioError::IncompleteAdapter);
        };
        let path = CString::new(device.usb_interface_path.as_bytes())
            .map_err(|_| GpioError::InvalidIdentity)?;
        let hardware = radio.cm119_hardware_request();
        let config = abi::rptadv_gpio_device_config {
            struct_size: std::mem::size_of::<abi::rptadv_gpio_device_config>() as u32,
            abi_version: 1,
            usb_port_path: path.as_ptr(),
            vendor_id: device.gpio_vendor_id,
            product_id: device.gpio_product_id,
            profile: match hardware.profile {
                rpt_advanced_core::config::Cm119Profile::DudeUsb => {
                    abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_DUDEUSB
                }
                rpt_advanced_core::config::Cm119Profile::SphUsb => {
                    abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_SPHUSB
                }
                rpt_advanced_core::config::Cm119Profile::Nhrc => {
                    abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC
                }
                rpt_advanced_core::config::Cm119Profile::Custom => {
                    abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_CUSTOM
                }
            },
            ptt_inverted: u32::from(hardware.ptt_inverted),
            gpio_output_enable_mask: u32::from(hardware.output_enable_mask),
            gpio_output_initial_mask: u32::from(hardware.initial_output_mask),
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: the path/config live through this synchronous device_open call.
        let result = unsafe { open(&config, &mut handle) };
        if result != 0 {
            if !handle.is_null() {
                // SAFETY: a returned handle remains owned by the adapter on failure.
                unsafe { close(handle) };
            }
            return Err(GpioError::Adapter(result));
        }
        Ok(Self {
            publish,
            service,
            get_inputs,
            close,
            handle: NonNull::new(handle).ok_or(GpioError::InvalidConfiguration)?,
            output_enable_mask: hardware.output_enable_mask,
            initial_output_mask: hardware.initial_output_mask,
        })
    }

    /// Publish desired PTT/GPIO levels to the adapter's lock-free output mailbox.
    pub fn publish_outputs(
        &self,
        ptt_asserted: bool,
        gpio_output_mask: u8,
    ) -> Result<(), GpioError> {
        let action = abi::rptadv_gpio_output_action {
            struct_size: std::mem::size_of::<abi::rptadv_gpio_output_action>() as u32,
            abi_version: 1,
            ptt_asserted: u32::from(ptt_asserted),
            gpio_output_mask: u32::from(gpio_output_mask & self.output_enable_mask),
        };
        // SAFETY: the action is copied by the adapter's bounded output mailbox.
        let result = unsafe { (self.publish)(self.handle.as_ptr(), &action) };
        (result == 0)
            .then_some(())
            .ok_or(GpioError::Adapter(result))
    }

    /// Perform one bounded HID service step; call from a non-audio owner.
    pub fn service(&self) -> Result<(), GpioError> {
        // SAFETY: the uniquely owned handle is serviced by its one control owner.
        let result = unsafe { (self.service)(self.handle.as_ptr()) };
        (result == 0)
            .then_some(())
            .ok_or(GpioError::Adapter(result))
    }

    /// Apply one PTT snapshot while preserving configured initial levels on other outputs.
    pub fn publish_ptt(&self, ptt_asserted: bool) -> Result<(), GpioError> {
        self.publish_outputs(ptt_asserted, self.initial_output_mask)
    }

    /// Read a lock-free copy of the latest adapter-polled COR and GPIO inputs.
    pub fn inputs(&self) -> Result<abi::rptadv_gpio_input_snapshot, GpioError> {
        let mut snapshot = abi::rptadv_gpio_input_snapshot {
            struct_size: std::mem::size_of::<abi::rptadv_gpio_input_snapshot>() as u32,
            abi_version: 1,
            online: 0,
            cor_active: 0,
            ctcss_active: 0,
            gpio_input_mask: 0,
            hid_report: [0; 4],
        };
        // SAFETY: the adapter writes a complete bounded snapshot to caller-owned storage.
        let result = unsafe { (self.get_inputs)(self.handle.as_ptr(), &mut snapshot) };
        if result != 0 {
            return Err(GpioError::Adapter(result));
        }
        if snapshot.struct_size < std::mem::size_of_val(&snapshot) as u32
            || snapshot.abi_version != 1
            || snapshot.online > 1
            || snapshot.cor_active > 1
            || snapshot.ctcss_active > 1
        {
            return Err(GpioError::InvalidSnapshot);
        }
        Ok(snapshot)
    }
}

/// One off-callback owner that polls CM119 receive signals and applies PTT.
pub struct GpioService {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

type GpioServiceJob = Box<dyn FnOnce() + Send + 'static>;

impl GpioService {
    /// Move a unique device to a bounded polling thread before starting PortAudio.
    pub fn start(
        device: Cm119GpioDevice,
        state: std::sync::Arc<crate::audio_callbacks::StandaloneRadioState>,
    ) -> Result<Self, GpioError> {
        Self::start_with_spawn(device, state, |job| {
            std::thread::Builder::new()
                .name("rptadv-gpio".into())
                .spawn(job)
        })
    }

    fn start_with_spawn(
        device: Cm119GpioDevice,
        state: std::sync::Arc<crate::audio_callbacks::StandaloneRadioState>,
        spawn: impl FnOnce(GpioServiceJob) -> std::io::Result<std::thread::JoinHandle<()>>,
    ) -> Result<Self, GpioError> {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = spawn(Box::new(move || {
            let mut applied_ptt = None;
            while !thread_stop.load(Ordering::Acquire) {
                let inputs = device.service().and_then(|()| device.inputs());
                let inputs_available = matches!(&inputs, Ok(snapshot) if snapshot.online != 0);
                match inputs {
                    Ok(inputs) if inputs.online != 0 => state.set_receive_inputs(
                        inputs.cor_active != 0,
                        inputs.ctcss_active != 0,
                        false,
                    ),
                    _ => state.set_receive_inputs(false, false, false),
                }

                // Loss of the GPIO service path must release PTT rather than hold a carrier.
                let requested_ptt = inputs_available && state.logical_ptt();
                if applied_ptt != Some(requested_ptt) {
                    if device.publish_ptt(requested_ptt).is_ok() {
                        applied_ptt = Some(requested_ptt);
                        state.set_physical_ptt(requested_ptt);
                    } else {
                        applied_ptt = None;
                        state.set_physical_ptt(false);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            let _ = device.publish_ptt(false);
            state.set_physical_ptt(false);
            state.set_receive_inputs(false, false, false);
        }))
        .map_err(|_| GpioError::ThreadStart)?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for GpioService {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Cm119GpioDevice {
    fn drop(&mut self) {
        // SAFETY: `open` requires close; all GPIO operations have stopped before drop.
        unsafe { (self.close)(self.handle.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::{Cm119GpioDevice, GpioError};
    use crate::{ResolvedRadioNode, abi, providers::ResolvedCm119Device};
    use std::sync::{
        Mutex,
        atomic::{AtomicI32, AtomicUsize, Ordering},
    };

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static LAST_PTT: AtomicUsize = AtomicUsize::new(0);
    static LAST_PROFILE: AtomicUsize = AtomicUsize::new(0);
    static OPEN_RESULT: AtomicI32 = AtomicI32::new(0);
    static OPEN_PARTIAL_HANDLE: AtomicUsize = AtomicUsize::new(0);
    static OPEN_NULL_HANDLE: AtomicUsize = AtomicUsize::new(0);
    static PUBLISH_RESULT: AtomicI32 = AtomicI32::new(0);
    static SERVICE_RESULT: AtomicI32 = AtomicI32::new(0);
    static INPUT_RESULT: AtomicI32 = AtomicI32::new(0);
    static SNAPSHOT_MODE: AtomicUsize = AtomicUsize::new(0);
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn opens_publishes_services_samples_and_closes_the_selected_device() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        reset_adapter_results();
        let api = fake_api();
        let radio = radio();
        let device = device();
        let gpio = Cm119GpioDevice::open(&api, &radio, &device).unwrap();

        gpio.publish_outputs(true, 0x05).unwrap();
        gpio.service().unwrap();
        let inputs = gpio.inputs().unwrap();
        drop(gpio);

        assert_eq!(inputs.online, 1);
        assert_eq!(inputs.cor_active, 1);
        assert_eq!(inputs.ctcss_active, 0);
        assert_eq!(inputs.gpio_input_mask, 0x0a);
        assert_eq!(
            LAST_PROFILE.load(Ordering::SeqCst),
            abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC as usize
        );
        assert_eq!(LAST_PTT.load(Ordering::SeqCst), 1);
        assert_eq!(CALLS.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn gpio_errors_have_stable_user_facing_messages() {
        assert_eq!(
            GpioError::IncompleteAdapter.to_string(),
            "CM119 GPIO adapter is incomplete"
        );
        assert_eq!(
            GpioError::InvalidIdentity.to_string(),
            "invalid CM119 USB identity"
        );
        assert_eq!(
            GpioError::InvalidConfiguration.to_string(),
            "CM119 GPIO adapter rejected configuration"
        );
        assert_eq!(
            GpioError::Adapter(-4).to_string(),
            "CM119 GPIO operation failed (-4)"
        );
        assert_eq!(
            GpioError::InvalidSnapshot.to_string(),
            "CM119 GPIO input snapshot is invalid"
        );
        assert_eq!(
            GpioError::ThreadStart.to_string(),
            "cannot start CM119 GPIO service thread"
        );
    }

    #[test]
    fn incomplete_adapter_is_rejected_before_device_open() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        reset_adapter_results();
        let mut api = fake_api();
        api.device_service = None;

        assert!(matches!(
            Cm119GpioDevice::open(&api, &radio(), &device()),
            Err(GpioError::IncompleteAdapter)
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn rejects_nul_identity_and_adapter_open_failures_safely() {
        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        reset_adapter_results();
        let api = fake_api();
        let mut invalid_device = device();
        invalid_device.usb_interface_path = "bad\0path".into();
        assert!(matches!(
            Cm119GpioDevice::open(&api, &radio(), &invalid_device),
            Err(GpioError::InvalidIdentity)
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 0);

        OPEN_RESULT.store(-7, Ordering::SeqCst);
        assert!(matches!(
            Cm119GpioDevice::open(&api, &radio(), &device()),
            Err(GpioError::Adapter(-7))
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);

        OPEN_PARTIAL_HANDLE.store(1, Ordering::SeqCst);
        assert!(matches!(
            Cm119GpioDevice::open(&api, &radio(), &device()),
            Err(GpioError::Adapter(-7))
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 3);

        OPEN_RESULT.store(0, Ordering::SeqCst);
        OPEN_NULL_HANDLE.store(1, Ordering::SeqCst);
        assert!(matches!(
            Cm119GpioDevice::open(&api, &radio(), &device()),
            Err(GpioError::InvalidConfiguration)
        ));
    }

    #[test]
    fn maps_each_supported_cm119_profile_to_the_gpio_abi() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_adapter_results();
        let api = fake_api();
        for (configured, expected) in [
            (
                "dudeusb",
                abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_DUDEUSB,
            ),
            (
                "sphusb",
                abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_SPHUSB,
            ),
            (
                "nhrc",
                abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC,
            ),
            (
                "custom",
                abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_CUSTOM,
            ),
        ] {
            let gpio = Cm119GpioDevice::open(&api, &radio_profile(configured), &device()).unwrap();
            assert_eq!(LAST_PROFILE.load(Ordering::SeqCst), expected as usize);
            drop(gpio);
        }
    }

    #[test]
    fn reports_output_service_and_input_adapter_errors() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_adapter_results();
        let api = fake_api();
        let gpio = Cm119GpioDevice::open(&api, &radio(), &device()).unwrap();

        PUBLISH_RESULT.store(-12, Ordering::SeqCst);
        assert_eq!(gpio.publish_ptt(true), Err(GpioError::Adapter(-12)));
        PUBLISH_RESULT.store(0, Ordering::SeqCst);
        SERVICE_RESULT.store(-13, Ordering::SeqCst);
        assert_eq!(gpio.service(), Err(GpioError::Adapter(-13)));
        SERVICE_RESULT.store(0, Ordering::SeqCst);
        INPUT_RESULT.store(-14, Ordering::SeqCst);
        assert_eq!(gpio.inputs().err(), Some(GpioError::Adapter(-14)));
    }

    #[test]
    fn rejects_each_malformed_input_snapshot() {
        let _guard = TEST_LOCK.lock().unwrap();
        reset_adapter_results();
        let api = fake_api();
        let gpio = Cm119GpioDevice::open(&api, &radio(), &device()).unwrap();
        for mode in 1..=5 {
            SNAPSHOT_MODE.store(mode, Ordering::SeqCst);
            assert_eq!(gpio.inputs().err(), Some(GpioError::InvalidSnapshot));
        }
        SNAPSHOT_MODE.store(6, Ordering::SeqCst);
        assert_eq!(gpio.inputs().unwrap().online, 0);
    }

    #[test]
    fn gpio_service_releases_ptt_when_input_service_fails() {
        use super::GpioService;
        use crate::audio_callbacks::StandaloneRadioState;
        use std::{sync::Arc, time::Instant};

        let _guard = TEST_LOCK.lock().unwrap();
        reset_adapter_results();
        SERVICE_RESULT.store(-1, Ordering::SeqCst);
        let api = Box::leak(Box::new(fake_api()));
        let device = Cm119GpioDevice::open(api, &radio(), &device()).unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        state.set_logical_ptt_for_test(true);
        CALLS.store(0, Ordering::SeqCst);
        let service = GpioService::start(device, Arc::clone(&state)).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        while CALLS.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(state.receive_inputs(), (false, false, false));
        drop(service);
        assert!(!state.physical_ptt());
        assert_eq!(LAST_PTT.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn gpio_service_maps_thread_spawn_failure() {
        use super::{GpioService, GpioServiceJob};
        use crate::audio_callbacks::StandaloneRadioState;
        use std::{io, sync::Arc};

        let _guard = TEST_LOCK.lock().unwrap();
        let api = Box::leak(Box::new(fake_api()));
        reset_adapter_results();
        let device = Cm119GpioDevice::open(api, &radio(), &device()).unwrap();
        let result = GpioService::start_with_spawn(
            device,
            Arc::new(StandaloneRadioState::default()),
            |_job: GpioServiceJob| Err(io::Error::other("simulated spawn failure")),
        );
        assert_eq!(result.err(), Some(GpioError::ThreadStart));
    }

    #[test]
    fn gpio_service_never_keys_an_offline_device() {
        use super::GpioService;
        use crate::audio_callbacks::StandaloneRadioState;
        use std::{sync::Arc, time::Instant};

        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        reset_adapter_results();
        SNAPSHOT_MODE.store(6, Ordering::SeqCst);
        let api = Box::leak(Box::new(fake_api()));
        let device = Cm119GpioDevice::open(api, &radio(), &device()).unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        state.set_logical_ptt_for_test(true);
        CALLS.store(0, Ordering::SeqCst);
        let service = GpioService::start(device, Arc::clone(&state)).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        while CALLS.load(Ordering::SeqCst) < 6 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(CALLS.load(Ordering::SeqCst) >= 6);
        assert_eq!(state.receive_inputs(), (false, false, false));
        assert!(!state.physical_ptt());
        assert_eq!(LAST_PTT.load(Ordering::SeqCst), 0);
        drop(service);
    }

    #[test]
    fn gpio_service_reports_failed_ptt_publish_and_keeps_physical_state_safe() {
        use super::GpioService;
        use crate::audio_callbacks::StandaloneRadioState;
        use std::{sync::Arc, time::Instant};

        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        reset_adapter_results();
        PUBLISH_RESULT.store(-15, Ordering::SeqCst);
        let api = Box::leak(Box::new(fake_api()));
        let device = Cm119GpioDevice::open(api, &radio(), &device()).unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        state.set_logical_ptt_for_test(true);
        CALLS.store(0, Ordering::SeqCst);
        let service = GpioService::start(device, Arc::clone(&state)).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        while LAST_PTT.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(LAST_PTT.load(Ordering::SeqCst), 1);
        assert!(!state.physical_ptt());
        drop(service);
        assert!(!state.physical_ptt());
    }

    #[test]
    fn service_drop_tolerates_an_absent_worker_handle() {
        use super::GpioService;
        use std::sync::{Arc, atomic::AtomicBool};

        let service = GpioService {
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        };
        drop(service);
    }

    #[test]
    fn gpio_service_publishes_receive_state_off_callback_and_joins_on_drop() {
        use super::GpioService;
        use crate::audio_callbacks::StandaloneRadioState;
        use std::{sync::Arc, time::Instant};

        let _guard = TEST_LOCK.lock().unwrap();
        CALLS.store(0, Ordering::SeqCst);
        reset_adapter_results();
        let api = Box::leak(Box::new(fake_api()));
        let device = Cm119GpioDevice::open(api, &radio(), &device()).unwrap();
        let state = Arc::new(StandaloneRadioState::default());
        let service = GpioService::start(device, Arc::clone(&state)).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_millis(100);
        while !state.receive_inputs().0 && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(state.receive_inputs(), (true, false, false));
        drop(service);

        assert!(!state.receive_inputs().0);
        assert_eq!(LAST_PTT.load(Ordering::SeqCst), 0);
        assert!(CALLS.load(Ordering::SeqCst) >= 5);
    }

    unsafe extern "C" fn open(
        config: *const abi::rptadv_gpio_device_config,
        device: *mut *mut abi::rptadv_gpio_device,
    ) -> i32 {
        let config = unsafe { &*config };
        assert_eq!(config.abi_version, 1);
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(config.usb_port_path) },
            c"3-1:1.0"
        );
        assert_eq!(config.vendor_id, 0x0d8c);
        assert_eq!(config.product_id, 0x013c);
        LAST_PROFILE.store(config.profile as usize, Ordering::SeqCst);
        assert_eq!(config.ptt_inverted, 1);
        assert_eq!(config.gpio_output_enable_mask, 0x07);
        assert_eq!(config.gpio_output_initial_mask, 0x05);
        CALLS.fetch_add(1, Ordering::SeqCst);
        let result = OPEN_RESULT.load(Ordering::SeqCst);
        if result == 0 && OPEN_NULL_HANDLE.load(Ordering::SeqCst) == 0
            || result != 0 && OPEN_PARTIAL_HANDLE.load(Ordering::SeqCst) != 0
        {
            unsafe { *device = std::ptr::dangling_mut() };
        }
        result
    }
    unsafe extern "C" fn publish(
        _: *mut abi::rptadv_gpio_device,
        action: *const abi::rptadv_gpio_output_action,
    ) -> i32 {
        let action = unsafe { &*action };
        LAST_PTT.store(action.ptt_asserted as usize, Ordering::SeqCst);
        let result = PUBLISH_RESULT.load(Ordering::SeqCst);
        if result != 0 {
            return result;
        }
        if action.ptt_asserted > 1 || action.gpio_output_mask != 0x05 {
            return -1;
        }
        CALLS.fetch_add(1, Ordering::SeqCst);
        0
    }
    unsafe extern "C" fn service(_: *mut abi::rptadv_gpio_device) -> i32 {
        CALLS.fetch_add(1, Ordering::SeqCst);
        SERVICE_RESULT.load(Ordering::SeqCst)
    }
    unsafe extern "C" fn inputs(
        _: *const abi::rptadv_gpio_device,
        snapshot: *mut abi::rptadv_gpio_input_snapshot,
    ) -> i32 {
        let result = INPUT_RESULT.load(Ordering::SeqCst);
        if result != 0 {
            return result;
        }
        let snapshot = unsafe { &mut *snapshot };
        snapshot.struct_size = std::mem::size_of_val(snapshot) as u32;
        snapshot.abi_version = 1;
        snapshot.online = 1;
        snapshot.cor_active = 1;
        snapshot.ctcss_active = 0;
        snapshot.gpio_input_mask = 0x0a;
        match SNAPSHOT_MODE.load(Ordering::SeqCst) {
            1 => snapshot.struct_size = 0,
            2 => snapshot.abi_version = 2,
            3 => snapshot.online = 2,
            4 => snapshot.cor_active = 2,
            5 => snapshot.ctcss_active = 2,
            6 => snapshot.online = 0,
            _ => {}
        }
        CALLS.fetch_add(1, Ordering::SeqCst);
        0
    }
    unsafe extern "C" fn close(_: *mut abi::rptadv_gpio_device) {
        CALLS.fetch_add(1, Ordering::SeqCst);
    }

    fn fake_api() -> abi::rptadv_gpio_adapter_descriptor {
        abi::rptadv_gpio_adapter_descriptor {
            device_open: Some(open),
            device_publish_outputs: Some(publish),
            device_service: Some(service),
            device_get_inputs: Some(inputs),
            device_close: Some(close),
            ..unsafe { std::mem::zeroed() }
        }
    }

    fn radio() -> ResolvedRadioNode {
        radio_profile("nhrc")
    }

    fn radio_profile(profile: &str) -> ResolvedRadioNode {
        crate::resolve_radio_nodes(
            &rpt_advanced_core::config::ConfigDocument::parse(
                &format!("[radio]\ncm119_profile={profile}\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\ncm119_gpio_2_mode=out0\ncm119_gpio_3_mode=out1\n[1000]\n"),
            )
            .unwrap(),
        )
        .unwrap()
        .value
        .remove(0)
    }

    fn reset_adapter_results() {
        OPEN_RESULT.store(0, Ordering::SeqCst);
        OPEN_PARTIAL_HANDLE.store(0, Ordering::SeqCst);
        OPEN_NULL_HANDLE.store(0, Ordering::SeqCst);
        PUBLISH_RESULT.store(0, Ordering::SeqCst);
        SERVICE_RESULT.store(0, Ordering::SeqCst);
        INPUT_RESULT.store(0, Ordering::SeqCst);
        SNAPSHOT_MODE.store(0, Ordering::SeqCst);
    }

    fn device() -> ResolvedCm119Device {
        ResolvedCm119Device {
            usb_interface_path: "3-1:1.0".into(),
            usb_serial: Some("SERIAL-A".into()),
            alsa_card_index: 4,
            input_device_index: 6,
            output_device_index: 7,
            gpio_vendor_id: 0x0d8c,
            gpio_product_id: 0x013c,
        }
    }
}
