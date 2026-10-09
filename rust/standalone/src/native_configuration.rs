//! Owned copies of the shared radio product's resolved native request.

use crate::abi;

/// Native radio policy copied from the synchronous product inspection callback.
/// No parser, DSP, or hardware policy is implemented by this owner.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeRadioConfiguration {
    config: abi::UrpNativeStationConfig,
    radio: abi::rptadv_radio_session_config,
    device_identifier: String,
    usb_serial: String,
    receive_graph: String,
    transmit_graph: String,
}

// SAFETY: all stored request pointers are cleared after copying; the immutable
// strings and flat radio configuration own all data used by request().
unsafe impl Send for NativeRadioConfiguration {}
// SAFETY: request() borrows immutable owner data and creates a fresh pointer view.
unsafe impl Sync for NativeRadioConfiguration {}

impl NativeRadioConfiguration {
    /// Copy an inspection callback's borrowed native request.
    ///
    /// # Safety
    /// Non-null pointers must expose their advertised structure/byte spans for
    /// this synchronous call. No supplied pointer is retained.
    pub unsafe fn copy_from(pointer: *const abi::UrpNativeStationConfig) -> Result<Self, String> {
        if pointer.is_null() {
            return Err("missing native radio configuration".to_owned());
        }
        // SAFETY: inspection supplies at least a readable size prefix.
        if unsafe { pointer.cast::<u32>().read_unaligned() }
            < std::mem::size_of::<abi::UrpNativeStationConfig>() as u32
        {
            return Err("short native radio configuration".to_owned());
        }
        // SAFETY: the size prefix covers the complete native request.
        let mut config = unsafe { pointer.read_unaligned() };
        if config.abi_version != 1 || config.radio.is_null() {
            return Err("incompatible native radio configuration".to_owned());
        }
        // SAFETY: a non-null radio template exposes its readable size prefix.
        if unsafe { config.radio.cast::<u32>().read_unaligned() }
            < std::mem::size_of::<abi::rptadv_radio_session_config>() as u32
        {
            return Err("short native radio session template".to_owned());
        }
        // SAFETY: the size prefix covers the released radio-session template.
        let radio = unsafe { config.radio.read_unaligned() };
        if radio.abi_version != 4 {
            return Err("incompatible native radio session template".to_owned());
        }
        // SAFETY: the caller guarantees each advertised span for this call.
        let (device_identifier, usb_serial, receive_graph, transmit_graph) = unsafe {
            (
                copy_text(config.device_identifier, config.device_identifier_length)?,
                copy_text(config.usb_serial, config.usb_serial_length)?,
                copy_text(config.receive_graph, config.receive_graph_length)?,
                copy_text(config.transmit_graph, config.transmit_graph_length)?,
            )
        };
        config.radio = std::ptr::null();
        config.device_identifier = std::ptr::null();
        config.usb_serial = std::ptr::null();
        config.receive_graph = std::ptr::null();
        config.transmit_graph = std::ptr::null();
        Ok(Self {
            config,
            radio,
            device_identifier,
            usb_serial,
            receive_graph,
            transmit_graph,
        })
    }

    /// Create a borrowed request view valid while this owner remains borrowed.
    pub fn request(&self) -> abi::UrpNativeStationConfig {
        abi::UrpNativeStationConfig {
            radio: &self.radio,
            device_identifier: self.device_identifier.as_ptr(),
            usb_serial: self.usb_serial.as_ptr(),
            receive_graph: self.receive_graph.as_ptr(),
            transmit_graph: self.transmit_graph.as_ptr(),
            ..self.config
        }
    }
}

unsafe fn copy_text(pointer: *const u8, length: u32) -> Result<String, String> {
    if length == 0 {
        return Ok(String::new());
    }
    if pointer.is_null() {
        return Err("missing native request text".to_owned());
    }
    // SAFETY: inspection guarantees exactly length readable bytes.
    let bytes = unsafe { std::slice::from_raw_parts(pointer, length as usize) };
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| "invalid native request UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copied_native_request_and_clone_own_all_borrowed_data() {
        // SAFETY: these C records contain only scalar fields and nullable pointers.
        let mut radio: abi::rptadv_radio_session_config = unsafe { std::mem::zeroed() };
        radio.struct_size = std::mem::size_of_val(&radio) as u32;
        radio.abi_version = 4;
        radio.receive_input_gain = 0.7943282;
        let receive = String::from("highpass=f=60,lowpass=f=5500");
        let transmit = String::from("volume=-2dB");
        let device = String::from("3-1");
        // SAFETY: zero initializes nullable pointers and scalars, never an enum.
        let mut request: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
        request.struct_size = std::mem::size_of_val(&request) as u32;
        request.abi_version = 1;
        request.radio = &radio;
        request.device_identifier = device.as_ptr();
        request.device_identifier_length = device.len() as u32;
        request.receive_graph = receive.as_ptr();
        request.receive_graph_length = receive.len() as u32;
        request.transmit_graph = transmit.as_ptr();
        request.transmit_graph_length = transmit.len() as u32;
        request.receive_output_gain_db = -6;
        // SAFETY: all request storage remains live for this synchronous copy.
        let owned = unsafe { NativeRadioConfiguration::copy_from(&request) }.unwrap();
        let cloned = owned.clone();
        assert_eq!(owned, cloned);
        assert_ne!(owned.request().radio, cloned.request().radio);
        drop((device, receive, transmit, owned));
        let view = cloned.request();
        // SAFETY: the view borrows the still-live cloned owner.
        unsafe {
            assert_eq!((*view.radio).receive_input_gain, 0.7943282);
            assert_eq!(
                std::slice::from_raw_parts(view.receive_graph, view.receive_graph_length as usize),
                b"highpass=f=60,lowpass=f=5500"
            );
            assert_eq!(
                std::slice::from_raw_parts(
                    view.transmit_graph,
                    view.transmit_graph_length as usize
                ),
                b"volume=-2dB"
            );
        }
        assert_eq!(view.receive_output_gain_db, -6);
    }

    #[test]
    fn copied_native_request_rejects_null_and_size_only_header() {
        // SAFETY: null is rejected before access.
        assert!(unsafe { NativeRadioConfiguration::copy_from(std::ptr::null()) }.is_err());
        let short = 4_u32;
        // SAFETY: only the readable size word may be accessed for this short request.
        assert!(
            unsafe { NativeRadioConfiguration::copy_from(std::ptr::from_ref(&short).cast()) }
                .is_err()
        );
    }

    #[test]
    fn copied_native_request_rejects_incompatible_templates_and_missing_text() {
        // SAFETY: these ABI records contain only scalar fields and nullable pointers.
        let (mut radio, mut request): (
            abi::rptadv_radio_session_config,
            abi::UrpNativeStationConfig,
        ) = unsafe { std::mem::zeroed() };
        radio.struct_size = std::mem::size_of_val(&radio) as u32;
        radio.abi_version = 4;
        request.struct_size = std::mem::size_of_val(&request) as u32;
        request.abi_version = 1;
        request.radio = &radio;

        request.abi_version = 2;
        assert_eq!(
            unsafe { NativeRadioConfiguration::copy_from(&request) }.unwrap_err(),
            "incompatible native radio configuration"
        );
        request.abi_version = 1;
        request.radio = std::ptr::null();
        assert_eq!(
            unsafe { NativeRadioConfiguration::copy_from(&request) }.unwrap_err(),
            "incompatible native radio configuration"
        );
        request.radio = &radio;
        radio.struct_size = 4;
        assert_eq!(
            unsafe { NativeRadioConfiguration::copy_from(&request) }.unwrap_err(),
            "short native radio session template"
        );
        radio.struct_size = std::mem::size_of_val(&radio) as u32;
        radio.abi_version = 3;
        assert_eq!(
            unsafe { NativeRadioConfiguration::copy_from(&request) }.unwrap_err(),
            "incompatible native radio session template"
        );
        radio.abi_version = 4;
        request.device_identifier_length = 1;
        assert_eq!(
            unsafe { NativeRadioConfiguration::copy_from(&request) }.unwrap_err(),
            "missing native request text"
        );
    }
}
