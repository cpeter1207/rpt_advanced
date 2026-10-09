//! Dynamic provider loading and descriptor compatibility checks.

use libloading::Library;
use rpt_advanced_core::config::{Cm119Profile, RadioDeviceSelection};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
};

const PROVIDERS: [ProviderSpec; 9] = [
    inline(
        "librptadv_product.so.1",
        "rptadv_product_descriptor_v1",
        3,
        b"rptadv.prod3",
    ),
    private(pointer(
        "librptadv_control_standalone_adapter.so.1",
        "rptadv_control_standalone_descriptor_v1",
        1,
        b"rptadv.control",
        DescriptorLayout::Control,
    )),
    inline(
        "librptadv_file_adapter.so.1",
        "rptadv_file_adapter_descriptor",
        2,
        b"rptadv.file",
    ),
    inline(
        "librptadv_speech_adapter.so.1",
        "rptadv_speech_adapter_descriptor",
        2,
        b"rptadv.speech",
    ),
    system(inline(
        "librptadviax2.so.1",
        "rptadv_iax2_client_descriptor_v1",
        1,
        b"rptadv.iax2.v1",
    )),
    pointer(
        "librptadvradio.so.4",
        "rptadv_radio_descriptor",
        4,
        b"rptadv.radio-core",
        DescriptorLayout::Named,
    ),
    pointer(
        "librptadv_portaudio_alsa_adapter.so.2",
        "rptadv_portaudio_alsa_adapter_descriptor",
        2,
        b"rptadv.portaudio-alsa-audio",
        DescriptorLayout::Named,
    ),
    pointer(
        "librptadv_ffmpeg_adapter.so.1",
        "rptadv_ffmpeg_adapter_descriptor",
        1,
        b"rptadv.ffmpeg",
        DescriptorLayout::Named,
    ),
    pointer(
        "librptadv_gpio_adapter.so.1",
        "rptadv_gpio_adapter_descriptor",
        1,
        b"rptadv.cm119-hid-gpio",
        DescriptorLayout::Named,
    ),
];

#[derive(Clone, Copy)]
struct ProviderSpec {
    library: &'static str,
    symbol: &'static str,
    abi_version: u32,
    capability: &'static [u8],
    layout: DescriptorLayout,
    location: LibraryLocation,
}

#[derive(Clone, Copy)]
enum DescriptorLayout {
    Inline,
    Named,
    Control,
}

#[derive(Clone, Copy)]
enum LibraryLocation {
    Private,
    System,
}

const fn inline(
    library: &'static str,
    symbol: &'static str,
    abi_version: u32,
    capability: &'static [u8],
) -> ProviderSpec {
    ProviderSpec {
        library,
        symbol,
        abi_version,
        capability,
        layout: DescriptorLayout::Inline,
        location: LibraryLocation::Private,
    }
}

const fn pointer(
    library: &'static str,
    symbol: &'static str,
    abi_version: u32,
    capability: &'static [u8],
    layout: DescriptorLayout,
) -> ProviderSpec {
    ProviderSpec {
        library,
        symbol,
        abi_version,
        capability,
        layout,
        location: LibraryLocation::System,
    }
}

const fn private(provider: ProviderSpec) -> ProviderSpec {
    ProviderSpec {
        location: LibraryLocation::Private,
        ..provider
    }
}

const fn system(provider: ProviderSpec) -> ProviderSpec {
    ProviderSpec {
        location: LibraryLocation::System,
        ..provider
    }
}

/// A required shared library could not be loaded or did not expose its descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderError {
    /// The named shared library could not be loaded.
    Load(&'static str),
    /// The named shared library did not expose a non-null descriptor.
    MissingDescriptor(&'static str),
    /// The named descriptor has an unsupported ABI or capability.
    IncompatibleDescriptor(&'static str),
    /// The named descriptor does not expose its complete callable table.
    IncompleteDescriptor(&'static str),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(name) => write!(formatter, "cannot load required provider {name}"),
            Self::MissingDescriptor(name) => {
                write!(formatter, "required provider {name} has no descriptor")
            }
            Self::IncompatibleDescriptor(name) => {
                write!(
                    formatter,
                    "required provider {name} has an incompatible descriptor"
                )
            }
            Self::IncompleteDescriptor(name) => {
                write!(
                    formatter,
                    "required provider {name} has an incomplete runtime descriptor"
                )
            }
        }
    }
}

impl std::error::Error for ProviderError {}

/// A CM119 identity selected consistently by the audio and GPIO adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCm119Device {
    /// Stable USB interface path used to identify the ALSA audio device.
    pub usb_interface_path: String,
    /// Linux USB topology path used to identify the HID/GPIO device.
    pub usb_port_path: String,
    /// USB serial reported for the selected interface, when present.
    pub usb_serial: Option<String>,
    /// ALSA card selected by the PortAudio adapter.
    pub alsa_card_index: u32,
    /// PortAudio capture endpoint selected by the adapter.
    pub input_device_index: i32,
    /// PortAudio playback endpoint selected by the adapter.
    pub output_device_index: i32,
    /// USB vendor ID observed by the GPIO adapter.
    pub gpio_vendor_id: u16,
    /// USB product ID observed by the GPIO adapter.
    pub gpio_product_id: u16,
}

/// A required CM119 adapter operation failed or returned an invalid identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Cm119DeviceError {
    /// The selected audio or GPIO provider does not expose the needed operation.
    IncompleteProvider,
    /// Exact device selection has no identifier or the configured identity contains NUL.
    InvalidIdentity,
    /// The audio adapter could not find or select the configured USB audio device.
    AudioSelectionFailed,
    /// The audio adapter returned malformed selection data.
    InvalidAudioSelection,
    /// The configured USB serial differs from the selected interface's serial.
    AudioSerialMismatch,
    /// The GPIO adapter could not query the selected interface.
    GpioProbeFailed(i32),
    /// The selected interface is not visible to the GPIO adapter.
    GpioDeviceNotPresent,
    /// The GPIO adapter reports a different serial for the selected interface.
    GpioSerialMismatch,
}

impl std::fmt::Display for Cm119DeviceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::IncompleteProvider => "CM119 audio or GPIO provider is incomplete",
            Self::InvalidIdentity => "CM119 selection needs a valid device identifier or serial",
            Self::AudioSelectionFailed => "PortAudio adapter could not select the CM119 device",
            Self::InvalidAudioSelection => "PortAudio adapter returned an invalid CM119 selection",
            Self::AudioSerialMismatch => "selected CM119 serial does not match configuration",
            Self::GpioProbeFailed(result) => {
                return write!(formatter, "GPIO adapter probe failed with code {result}");
            }
            Self::GpioDeviceNotPresent => "selected CM119 device is not present on GPIO",
            Self::GpioSerialMismatch => "audio and GPIO adapters selected different CM119 devices",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for Cm119DeviceError {}

/// All standalone runtime libraries, kept loaded for the product's full lifetime.
pub struct ProviderSet {
    _libraries: Vec<Library>,
    descriptors: Vec<LoadedProvider>,
}

/// Native provider descriptors needed to activate one standalone radio.
pub struct RadioRuntimeDescriptors {
    /// Radio-core session API.
    pub radio: &'static crate::abi::rptadv_radio_descriptor,
    /// PortAudio/ALSA stream API.
    pub audio: &'static crate::abi::rptadv_audio_adapter_descriptor,
    /// CM119 HID/GPIO API.
    pub gpio: &'static crate::abi::rptadv_gpio_adapter_descriptor,
    /// Exact-frame FFmpeg graph API.
    pub ffmpeg: &'static crate::abi::rptadv_ffmpeg_adapter_descriptor,
}

/// Product/control/media descriptors retained for the standalone product lifetime.
pub struct ProductRuntimeDescriptors {
    /// Portable controller implementation.
    pub product: &'static crate::abi::rptadv_product_descriptor_v1,
    /// Lock-free standalone control-task executor.
    pub control: &'static crate::abi::rptadv_control_descriptor_v1,
    /// Local sound-file stream provider.
    pub file: &'static crate::abi::rptadv_file_descriptor,
    /// Offline speech stream provider.
    pub speech: &'static crate::abi::rptadv_speech_descriptor,
}

struct LoadedProvider {
    library: &'static str,
    size: usize,
    descriptor: NonNull<c_void>,
}

/// A validated provider descriptor borrowed from its loaded shared object.
pub struct ProviderDescriptor<'providers> {
    pointer: NonNull<c_void>,
    _providers: PhantomData<&'providers ProviderSet>,
}

impl ProviderDescriptor<'_> {
    /// Borrow the ABI pointer; the provider set must outlive every call through it.
    ///
    /// # Safety
    /// The caller must cast the pointer to the descriptor type for the named provider and
    /// obey that ABI's ownership and concurrency contract. The `ProviderSet` must remain
    /// alive for the complete use of the pointer.
    pub unsafe fn as_ptr(&self) -> *const c_void {
        self.pointer.as_ptr()
    }
}

impl ProviderSet {
    /// Load required providers and verify each descriptor's ABI and capability.
    /// This does not resolve or open radio hardware.
    pub fn load() -> Result<Self, ProviderError> {
        Self::load_at(None)
    }

    #[cfg(test)]
    fn load_from_directory(directory: &Path) -> Result<Self, ProviderError> {
        Self::load_at(Some(directory))
    }

    fn load_at(directory: Option<&Path>) -> Result<Self, ProviderError> {
        let mut libraries = Vec::with_capacity(PROVIDERS.len());
        let mut descriptors = Vec::with_capacity(PROVIDERS.len());
        for provider in PROVIDERS {
            let library = ProviderLibrary::open(provider, directory)?;
            let Some(descriptor) = library.descriptor(provider.symbol) else {
                return Err(ProviderError::MissingDescriptor(provider.library));
            };
            if !valid_descriptor(descriptor, provider) {
                return Err(ProviderError::IncompatibleDescriptor(provider.library));
            }
            // `ProviderLibrary::descriptor` filters null exports before returning them.
            let descriptor = NonNull::new(descriptor.cast_mut())
                .expect("provider descriptor was checked for null");
            descriptors.push(LoadedProvider {
                library: provider.library,
                size: descriptor_size(descriptor.as_ptr(), provider.layout),
                descriptor,
            });
            libraries.push(library.0);
        }
        Ok(Self {
            _libraries: libraries,
            descriptors,
        })
    }

    /// Return a validated descriptor whose access is bounded by this provider set.
    pub fn descriptor(&self, library: &str) -> Option<ProviderDescriptor<'_>> {
        let provider = self
            .descriptors
            .iter()
            .find(|provider| provider.library == library)?;
        Some(ProviderDescriptor {
            pointer: provider.descriptor,
            _providers: PhantomData,
        })
    }

    /// Ensure the product and its direct lifecycle providers expose complete function tables.
    pub fn validate_runtime(&self) -> Result<(), ProviderError> {
        let product = self.typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>(
            "librptadv_product.so.1",
        )?;
        if !product_functions_complete(product) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1",
            ));
        }
        let control = self.typed_descriptor::<crate::abi::rptadv_control_descriptor_v1>(
            "librptadv_control_standalone_adapter.so.1",
        )?;
        if control.open.is_none()
            || control.submit.is_none()
            || control.stop_and_drain.is_none()
            || control.close.is_none()
        {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_control_standalone_adapter.so.1",
            ));
        }
        let file = self.typed_descriptor::<crate::abi::rptadv_file_descriptor>(
            "librptadv_file_adapter.so.1",
        )?;
        if file.create.is_none()
            || file.destroy.is_none()
            || file.open_file.is_none()
            || file.read_stream.is_none()
            || file.close_stream.is_none()
        {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_file_adapter.so.1",
            ));
        }
        let speech = self.typed_descriptor::<crate::abi::rptadv_speech_descriptor>(
            "librptadv_speech_adapter.so.1",
        )?;
        if speech.create.is_none()
            || speech.destroy.is_none()
            || speech.open_speech.is_none()
            || speech.read_stream.is_none()
            || speech.close_stream.is_none()
        {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_speech_adapter.so.1",
            ));
        }
        let radio =
            self.typed_descriptor::<crate::abi::rptadv_radio_descriptor>("librptadvradio.so.4")?;
        if !radio_functions_complete(radio) {
            return Err(ProviderError::IncompleteDescriptor("librptadvradio.so.4"));
        }
        let audio = self.typed_descriptor::<crate::abi::rptadv_audio_adapter_descriptor>(
            "librptadv_portaudio_alsa_adapter.so.2",
        )?;
        if !audio_functions_complete(audio) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_portaudio_alsa_adapter.so.2",
            ));
        }
        let gpio = self.typed_descriptor::<crate::abi::rptadv_gpio_adapter_descriptor>(
            "librptadv_gpio_adapter.so.1",
        )?;
        if !gpio_functions_complete(gpio) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_gpio_adapter.so.1",
            ));
        }
        let ffmpeg = self.typed_descriptor::<crate::abi::rptadv_ffmpeg_adapter_descriptor>(
            "librptadv_ffmpeg_adapter.so.1",
        )?;
        if !ffmpeg_functions_complete(ffmpeg) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_ffmpeg_adapter.so.1",
            ));
        }
        Ok(())
    }

    /// Borrow the radio providers for an activation owner that lives as long as this set.
    pub fn radio_runtime_descriptors(
        &'static self,
    ) -> Result<RadioRuntimeDescriptors, ProviderError> {
        Ok(RadioRuntimeDescriptors {
            radio: self.typed_descriptor("librptadvradio.so.4")?,
            audio: self.typed_descriptor("librptadv_portaudio_alsa_adapter.so.2")?,
            gpio: self.typed_descriptor("librptadv_gpio_adapter.so.1")?,
            ffmpeg: self.typed_descriptor("librptadv_ffmpeg_adapter.so.1")?,
        })
    }

    /// Borrow validated product lifecycle descriptors while retaining all provider libraries.
    pub fn product_runtime_descriptors(
        &'static self,
    ) -> Result<ProductRuntimeDescriptors, ProviderError> {
        let product = self.typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>(
            "librptadv_product.so.1",
        )?;
        if !product_functions_complete(product) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1",
            ));
        }
        let control = self.typed_descriptor::<crate::abi::rptadv_control_descriptor_v1>(
            "librptadv_control_standalone_adapter.so.1",
        )?;
        let file = self.typed_descriptor::<crate::abi::rptadv_file_descriptor>(
            "librptadv_file_adapter.so.1",
        )?;
        let speech = self.typed_descriptor::<crate::abi::rptadv_speech_descriptor>(
            "librptadv_speech_adapter.so.1",
        )?;
        Ok(ProductRuntimeDescriptors {
            product,
            control,
            file,
            speech,
        })
    }

    /// Resolve one node's configured CM119 identity across the audio and GPIO adapters.
    pub fn resolve_cm119_device(
        &self,
        radio: &crate::ResolvedRadioNode,
    ) -> Result<ResolvedCm119Device, Cm119DeviceError> {
        let audio = self
            .typed_descriptor::<crate::abi::rptadv_audio_adapter_descriptor>(
                "librptadv_portaudio_alsa_adapter.so.2",
            )
            .map_err(|_| Cm119DeviceError::IncompleteProvider)?;
        let gpio = self
            .typed_descriptor::<crate::abi::rptadv_gpio_adapter_descriptor>(
                "librptadv_gpio_adapter.so.1",
            )
            .map_err(|_| Cm119DeviceError::IncompleteProvider)?;
        resolve_cm119_device(audio, gpio, radio)
    }

    fn typed_descriptor<T>(&self, library: &'static str) -> Result<&T, ProviderError> {
        let provider = self
            .descriptors
            .iter()
            .find(|provider| provider.library == library)
            .ok_or(ProviderError::MissingDescriptor(library))?;
        if provider.size < std::mem::size_of::<T>() {
            return Err(ProviderError::IncompleteDescriptor(library));
        }
        // SAFETY: load_at checked ABI identity and this method checks the full structure size;
        // each caller uses the corresponding public C header's generated Rust type.
        Ok(unsafe { provider.descriptor.cast::<T>().as_ref() })
    }
}

fn resolve_cm119_device(
    audio: &crate::abi::rptadv_audio_adapter_descriptor,
    gpio: &crate::abi::rptadv_gpio_adapter_descriptor,
    radio: &crate::ResolvedRadioNode,
) -> Result<ResolvedCm119Device, Cm119DeviceError> {
    let request = radio
        .audio_device_request()
        .map_err(|_| Cm119DeviceError::InvalidIdentity)?;
    let identifier = request
        .device_identifier
        .map(CString::new)
        .transpose()
        .map_err(|_| Cm119DeviceError::InvalidIdentity)?;
    let serial = request
        .usb_serial
        .as_deref()
        .map(CString::new)
        .transpose()
        .map_err(|_| Cm119DeviceError::InvalidIdentity)?;
    let Some(select) = audio.usb_device_select else {
        return Err(Cm119DeviceError::IncompleteProvider);
    };
    let mut audio_match: crate::abi::rptadv_audio_usb_device_match = unsafe { std::mem::zeroed() };
    audio_match.struct_size = std::mem::size_of_val(&audio_match) as u32;
    audio_match.abi_version = 2;
    let selector = crate::abi::rptadv_audio_usb_device_selector {
        struct_size: std::mem::size_of::<crate::abi::rptadv_audio_usb_device_selector>() as u32,
        selection_policy: match request.policy {
            RadioDeviceSelection::Exact => {
                crate::abi::rptadv_audio_usb_selection_policy_RPTADV_AUDIO_USB_SELECTION_EXACT
            }
            RadioDeviceSelection::AutomaticLowestAlsaCard => crate::abi::rptadv_audio_usb_selection_policy_RPTADV_AUDIO_USB_SELECTION_AUTOMATIC_LOWEST_ALSA_CARD,
        },
        device_identifier: identifier.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
        usb_serial: serial.as_ref().map_or(std::ptr::null(), |value| value.as_ptr()),
        input_device_channels: request.input_channels,
        output_device_channels: request.output_channels,
    };
    if unsafe { select(&selector, &mut audio_match) } != 0 {
        return Err(Cm119DeviceError::AudioSelectionFailed);
    }
    if audio_match.struct_size < std::mem::size_of_val(&audio_match) as u32
        || audio_match.abi_version != 2
        || audio_match.selection.struct_size
            < std::mem::size_of::<crate::abi::rptadv_audio_usb_device_selection>() as u32
        || audio_match.selection.abi_version != 2
    {
        return Err(Cm119DeviceError::InvalidAudioSelection);
    }
    let usb_interface_path = c_string_field(&audio_match.usb_interface_path)
        .filter(|path| !path.is_empty())
        .ok_or(Cm119DeviceError::InvalidAudioSelection)?;
    let usb_port_path = usb_interface_path
        .split(':')
        .next()
        .filter(|path| !path.is_empty())
        .ok_or(Cm119DeviceError::InvalidAudioSelection)?
        .to_owned();
    let audio_serial = c_string_field(&audio_match.usb_serial).filter(|value| !value.is_empty());
    if serial.is_some() && audio_serial.as_deref() != request.usb_serial.as_deref() {
        return Err(Cm119DeviceError::AudioSerialMismatch);
    }
    let path = CString::new(usb_port_path.as_bytes())
        .map_err(|_| Cm119DeviceError::InvalidAudioSelection)?;
    let hardware = radio.cm119_hardware_request();
    let gpio_config = crate::abi::rptadv_gpio_device_config {
        struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_device_config>() as u32,
        abi_version: 1,
        usb_port_path: path.as_ptr(),
        vendor_id: 0,
        product_id: 0,
        profile: match hardware.profile {
            Cm119Profile::DudeUsb => {
                crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_DUDEUSB
            }
            Cm119Profile::SphUsb => crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_SPHUSB,
            Cm119Profile::Nhrc => crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_NHRC,
            Cm119Profile::Custom => crate::abi::rptadv_gpio_cm119_profile_RPTADV_GPIO_CM119_CUSTOM,
        },
        ptt_inverted: u32::from(hardware.ptt_inverted),
        gpio_output_enable_mask: u32::from(hardware.output_enable_mask),
        gpio_output_initial_mask: u32::from(hardware.initial_output_mask),
    };
    let Some(probe) = gpio.device_probe else {
        return Err(Cm119DeviceError::IncompleteProvider);
    };
    let mut gpio_info: crate::abi::rptadv_gpio_device_info = unsafe { std::mem::zeroed() };
    gpio_info.struct_size = std::mem::size_of_val(&gpio_info) as u32;
    gpio_info.abi_version = 1;
    let probe_result = unsafe { probe(&gpio_config, &mut gpio_info) };
    if probe_result != 0 {
        return Err(Cm119DeviceError::GpioProbeFailed(probe_result));
    }
    if gpio_info.struct_size < std::mem::size_of_val(&gpio_info) as u32
        || gpio_info.abi_version != 1
    {
        return Err(Cm119DeviceError::IncompleteProvider);
    }
    if gpio_info.present == 0 {
        return Err(Cm119DeviceError::GpioDeviceNotPresent);
    }
    let gpio_serial = c_string_field(&gpio_info.serial).filter(|value| !value.is_empty());
    if let (Some(audio_serial), Some(gpio_serial)) = (&audio_serial, &gpio_serial) {
        if audio_serial != gpio_serial {
            return Err(Cm119DeviceError::GpioSerialMismatch);
        }
    }
    Ok(ResolvedCm119Device {
        usb_interface_path,
        usb_port_path,
        usb_serial: audio_serial.or(gpio_serial),
        alsa_card_index: audio_match.selection.alsa_card_index,
        input_device_index: audio_match.selection.input_device_index,
        output_device_index: audio_match.selection.output_device_index,
        gpio_vendor_id: gpio_info.vendor_id,
        gpio_product_id: gpio_info.product_id,
    })
}

fn c_string_field<const N: usize>(value: &[c_char; N]) -> Option<String> {
    let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), N) };
    CStr::from_bytes_until_nul(bytes)
        .ok()
        .map(|value| value.to_string_lossy().into_owned())
}

fn descriptor_size(pointer: *const c_void, layout: DescriptorLayout) -> usize {
    match layout {
        DescriptorLayout::Inline | DescriptorLayout::Named => {
            // SAFETY: valid_descriptor checked the full size/version prefix first.
            unsafe { pointer.cast::<u32>().read_unaligned() as usize }
        }
        DescriptorLayout::Control => {
            // SAFETY: valid_descriptor checked the control prefix before this read.
            unsafe {
                pointer
                    .cast::<ControlDescriptorHeader>()
                    .read_unaligned()
                    .struct_size
            }
        }
    }
}

fn product_functions_complete(api: &crate::abi::rptadv_product_descriptor_v1) -> bool {
    api.start.is_some()
        && api.reload.is_some()
        && api.stop.is_some()
        && api.authorize_incoming.is_some()
        && api.incoming.is_some()
        && api.link_command.is_some()
        && api.link_status.is_some()
        && api.digit.is_some()
}

fn radio_functions_complete(api: &crate::abi::rptadv_radio_descriptor) -> bool {
    api.session_create.is_some()
        && api.session_warm.is_some()
        && api.session_receive.is_some()
        && api.session_transmit.is_some()
        && api.session_snapshot.is_some()
        && api.session_pop_receive_event.is_some()
        && api.session_pop_transmit_event.is_some()
        && api.session_destroy.is_some()
        && api.session_prepare_update.is_some()
        && api.session_apply_receive_update.is_some()
        && api.session_apply_transmit_update.is_some()
        && api.session_destroy_update.is_some()
}

fn audio_functions_complete(api: &crate::abi::rptadv_audio_adapter_descriptor) -> bool {
    api.stream_create.is_some()
        && api.stream_start.is_some()
        && api.stream_stop.is_some()
        && api.stream_get_stats.is_some()
        && api.stream_destroy.is_some()
        && api.usb_device_select.is_some()
        && api.stream_get_timing.is_some()
}

fn gpio_functions_complete(api: &crate::abi::rptadv_gpio_adapter_descriptor) -> bool {
    api.device_probe.is_some()
        && api.device_open.is_some()
        && api.device_publish_outputs.is_some()
        && api.device_service.is_some()
        && api.device_get_inputs.is_some()
        && api.device_get_stats.is_some()
        && api.device_close.is_some()
}

fn ffmpeg_functions_complete(api: &crate::abi::rptadv_ffmpeg_adapter_descriptor) -> bool {
    api.create.is_some() && api.destroy.is_some() && api.process_block.is_some()
}

struct ProviderLibrary(Library);

impl ProviderLibrary {
    fn open(provider: ProviderSpec, directory: Option<&Path>) -> Result<Self, ProviderError> {
        let path = directory.map_or_else(
            || match provider.location {
                LibraryLocation::Private => {
                    Path::new(env!("RPT_ADVANCED_LIBDIR")).join(provider.library)
                }
                LibraryLocation::System => Path::new(provider.library).to_owned(),
            },
            |directory| directory.join(provider.library),
        );
        // SAFETY: provider paths are fixed package paths or versioned system SONAMEs.
        unsafe { Library::new(&path) }
            .map(Self)
            .map_err(|_| ProviderError::Load(provider.library))
    }

    fn descriptor(&self, name: &str) -> Option<*const c_void> {
        type Descriptor = unsafe extern "C" fn() -> *const c_void;
        // SAFETY: all standalone provider descriptor entry points return a pointer.
        let symbol = unsafe { self.0.get::<Descriptor>(format!("{name}\0").as_bytes()) }.ok()?;
        let descriptor = unsafe { symbol() };
        (!descriptor.is_null()).then_some(descriptor)
    }
}

#[repr(C)]
struct InlineDescriptorHeader {
    struct_size: u32,
    abi_version: u32,
    capability: [u8; 16],
}

#[repr(C)]
struct NamedDescriptorHeader {
    struct_size: u32,
    abi_version: u32,
    capability: *const c_char,
}

#[repr(C)]
struct ControlDescriptorHeader {
    abi_version: u32,
    struct_size: usize,
    capability: *const c_char,
}

fn valid_descriptor(pointer: *const c_void, provider: ProviderSpec) -> bool {
    match provider.layout {
        DescriptorLayout::Inline => {
            // SAFETY: trusted versioned providers expose readable fixed ABI prefixes.
            let header = unsafe { pointer.cast::<InlineDescriptorHeader>().read_unaligned() };
            header.struct_size as usize >= std::mem::size_of::<InlineDescriptorHeader>()
                && header.abi_version == provider.abi_version
                && header.capability.starts_with(provider.capability)
                && header.capability[provider.capability.len()..]
                    .iter()
                    .all(|byte| *byte == 0)
        }
        DescriptorLayout::Named => {
            // SAFETY: trusted adapter descriptors expose this fixed prefix.
            let header = unsafe { pointer.cast::<NamedDescriptorHeader>().read_unaligned() };
            named_descriptor_matches(
                header.struct_size as usize,
                header.abi_version,
                header.capability,
                provider,
                std::mem::size_of::<NamedDescriptorHeader>(),
            )
        }
        DescriptorLayout::Control => {
            // SAFETY: control adapter ABI has a distinct version/size/pointer order.
            let header = unsafe { pointer.cast::<ControlDescriptorHeader>().read_unaligned() };
            named_descriptor_matches(
                header.struct_size,
                header.abi_version,
                header.capability,
                provider,
                std::mem::size_of::<ControlDescriptorHeader>(),
            )
        }
    }
}

fn named_descriptor_matches(
    size: usize,
    version: u32,
    capability: *const c_char,
    provider: ProviderSpec,
    minimum_size: usize,
) -> bool {
    if size < minimum_size || version != provider.abi_version || capability.is_null() {
        return false;
    }
    // SAFETY: provider descriptors supply static NUL-terminated capability names.
    unsafe { CStr::from_ptr(capability) }.to_bytes() == provider.capability
}

#[cfg(test)]
mod tests {
    use super::{
        Cm119DeviceError, DescriptorLayout, ProviderError, ProviderLibrary, ProviderSpec,
        audio_functions_complete, ffmpeg_functions_complete, gpio_functions_complete,
        radio_functions_complete, resolve_cm119_device, valid_descriptor,
    };
    use std::ffi::{CStr, c_char, c_void};
    use std::path::Path;

    macro_rules! missing_fields {
        ($api:ident, $check:path, $($field:ident)+) => {
            $(
                let callback = $api.$field;
                $api.$field = None;
                assert!(!$check(&$api), stringify!($field));
                $api.$field = callback;
            )+
        };
    }

    impl super::ProviderSet {
        pub(crate) fn for_radio_activation_tests(
            radio: &'static crate::abi::rptadv_radio_descriptor,
            audio: &'static crate::abi::rptadv_audio_adapter_descriptor,
            gpio: &'static crate::abi::rptadv_gpio_adapter_descriptor,
            ffmpeg: &'static crate::abi::rptadv_ffmpeg_adapter_descriptor,
        ) -> Self {
            let entries = [
                (
                    "librptadvradio.so.4",
                    radio as *const _ as *const c_void,
                    std::mem::size_of_val(radio),
                ),
                (
                    "librptadv_portaudio_alsa_adapter.so.2",
                    audio as *const _ as *const c_void,
                    std::mem::size_of_val(audio),
                ),
                (
                    "librptadv_gpio_adapter.so.1",
                    gpio as *const _ as *const c_void,
                    std::mem::size_of_val(gpio),
                ),
                (
                    "librptadv_ffmpeg_adapter.so.1",
                    ffmpeg as *const _ as *const c_void,
                    std::mem::size_of_val(ffmpeg),
                ),
            ];
            Self {
                _libraries: Vec::new(),
                descriptors: entries
                    .into_iter()
                    .map(|(library, descriptor, size)| super::LoadedProvider {
                        library,
                        size,
                        descriptor: std::ptr::NonNull::new(descriptor.cast_mut())
                            .expect("static descriptor"),
                    })
                    .collect(),
            }
        }
    }

    #[test]
    fn runtime_rejects_native_descriptors_without_required_operations() {
        let radio: crate::abi::rptadv_radio_descriptor = unsafe { std::mem::zeroed() };
        let audio: crate::abi::rptadv_audio_adapter_descriptor = unsafe { std::mem::zeroed() };
        let gpio: crate::abi::rptadv_gpio_adapter_descriptor = unsafe { std::mem::zeroed() };
        let ffmpeg: crate::abi::rptadv_ffmpeg_adapter_descriptor = unsafe { std::mem::zeroed() };

        assert!(!radio_functions_complete(&radio));
        assert!(!audio_functions_complete(&audio));
        assert!(!gpio_functions_complete(&gpio));
        assert!(!ffmpeg_functions_complete(&ffmpeg));
    }

    #[test]
    fn provider_error_messages_are_stable() {
        for (error, expected) in [
            (
                ProviderError::Load("load"),
                "cannot load required provider load",
            ),
            (
                ProviderError::MissingDescriptor("missing"),
                "required provider missing has no descriptor",
            ),
            (
                ProviderError::IncompatibleDescriptor("incompatible"),
                "required provider incompatible has an incompatible descriptor",
            ),
            (
                ProviderError::IncompleteDescriptor("incomplete"),
                "required provider incomplete has an incomplete runtime descriptor",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn cm119_error_messages_are_stable() {
        for (error, expected) in [
            (
                Cm119DeviceError::IncompleteProvider,
                "CM119 audio or GPIO provider is incomplete",
            ),
            (
                Cm119DeviceError::InvalidIdentity,
                "CM119 selection needs a valid device identifier or serial",
            ),
            (
                Cm119DeviceError::AudioSelectionFailed,
                "PortAudio adapter could not select the CM119 device",
            ),
            (
                Cm119DeviceError::InvalidAudioSelection,
                "PortAudio adapter returned an invalid CM119 selection",
            ),
            (
                Cm119DeviceError::AudioSerialMismatch,
                "selected CM119 serial does not match configuration",
            ),
            (
                Cm119DeviceError::GpioProbeFailed(-4),
                "GPIO adapter probe failed with code -4",
            ),
            (
                Cm119DeviceError::GpioDeviceNotPresent,
                "selected CM119 device is not present on GPIO",
            ),
            (
                Cm119DeviceError::GpioSerialMismatch,
                "audio and GPIO adapters selected different CM119 devices",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    fn provider_spec_constructors_select_the_requested_layout_and_library_scope() {
        let inline = super::inline("inline", "symbol", 1, b"capability");
        let system_pointer = super::pointer(
            "pointer",
            "symbol",
            2,
            b"capability",
            DescriptorLayout::Named,
        );

        assert!(matches!(inline.layout, DescriptorLayout::Inline));
        assert!(matches!(inline.location, super::LibraryLocation::Private));
        assert!(matches!(
            system_pointer.location,
            super::LibraryLocation::System
        ));
        assert!(matches!(
            super::private(system_pointer).location,
            super::LibraryLocation::Private
        ));
        assert!(matches!(
            super::system(inline).location,
            super::LibraryLocation::System
        ));
    }

    #[test]
    fn missing_provider_library_returns_a_safe_load_error() {
        let provider = ProviderSpec {
            library: "/rpt-advanced-test-only/missing-provider.so",
            symbol: "missing",
            abi_version: 1,
            capability: b"test",
            layout: DescriptorLayout::Inline,
            location: super::LibraryLocation::System,
        };
        let error = ProviderLibrary::open(provider, None).err().unwrap();

        assert_eq!(error, ProviderError::Load(provider.library));
    }

    #[test]
    fn descriptor_contract_checks_abi_and_capability() {
        let capability = b"rptadv.prod3\0\0\0\0";
        let descriptor = InlineDescriptor {
            struct_size: std::mem::size_of::<InlineDescriptor>() as u32,
            abi_version: 3,
            capability: *capability,
        };
        let provider = ProviderSpec {
            library: "test",
            symbol: "test",
            abi_version: 3,
            capability: b"rptadv.prod3",
            layout: DescriptorLayout::Inline,
            location: super::LibraryLocation::Private,
        };

        assert!(valid_descriptor(
            (&descriptor as *const InlineDescriptor).cast::<c_void>(),
            provider
        ));
        let mut incompatible = descriptor;
        incompatible.abi_version = 4;
        assert!(!valid_descriptor(
            (&incompatible as *const InlineDescriptor).cast(),
            provider
        ));
        incompatible = descriptor;
        incompatible.struct_size = (std::mem::size_of::<InlineDescriptor>() - 1) as u32;
        assert!(!valid_descriptor(
            (&incompatible as *const InlineDescriptor).cast(),
            provider
        ));
        incompatible = descriptor;
        incompatible.capability[0] = b'x';
        assert!(!valid_descriptor(
            (&incompatible as *const InlineDescriptor).cast(),
            provider
        ));
        incompatible = descriptor;
        incompatible.capability[15] = b'x';
        assert!(!valid_descriptor(
            (&incompatible as *const InlineDescriptor).cast(),
            provider
        ));
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct InlineDescriptor {
        struct_size: u32,
        abi_version: u32,
        capability: [u8; 16],
    }

    #[repr(C)]
    struct NamedDescriptor {
        struct_size: u32,
        abi_version: u32,
        capability: *const c_char,
    }

    #[repr(C)]
    struct ControlDescriptor {
        abi_version: u32,
        struct_size: usize,
        capability: *const c_char,
    }

    #[test]
    fn descriptor_contract_supports_named_and_control_layouts() {
        let named_capability = c"rptadv.named";
        let named = NamedDescriptor {
            struct_size: std::mem::size_of::<NamedDescriptor>() as u32,
            abi_version: 2,
            capability: named_capability.as_ptr(),
        };
        let provider = ProviderSpec {
            library: "test",
            symbol: "test",
            abi_version: 2,
            capability: b"rptadv.named",
            layout: DescriptorLayout::Named,
            location: super::LibraryLocation::System,
        };
        assert!(valid_descriptor(
            (&named as *const NamedDescriptor).cast(),
            provider
        ));

        let control_capability = c"rptadv.control";
        let control = ControlDescriptor {
            abi_version: 1,
            struct_size: std::mem::size_of::<ControlDescriptor>(),
            capability: control_capability.as_ptr(),
        };
        let provider = ProviderSpec {
            abi_version: 1,
            capability: b"rptadv.control",
            layout: DescriptorLayout::Control,
            ..provider
        };
        assert!(valid_descriptor(
            (&control as *const ControlDescriptor).cast(),
            provider
        ));

        let named_provider = ProviderSpec {
            abi_version: 2,
            capability: b"rptadv.named",
            layout: DescriptorLayout::Named,
            ..provider
        };
        let invalid_cases = [
            (0, 2, named_capability.as_ptr()),
            (
                std::mem::size_of::<NamedDescriptor>() as u32,
                3,
                named_capability.as_ptr(),
            ),
            (
                std::mem::size_of::<NamedDescriptor>() as u32,
                2,
                std::ptr::null(),
            ),
        ];
        for (size, version, capability) in invalid_cases {
            let invalid = NamedDescriptor {
                struct_size: size,
                abi_version: version,
                capability,
            };
            assert!(!valid_descriptor(
                (&invalid as *const NamedDescriptor).cast(),
                named_provider
            ));
        }
        let wrong_name = c"rptadv.other";
        let invalid_control = ControlDescriptor {
            capability: wrong_name.as_ptr(),
            ..control
        };
        assert!(!valid_descriptor(
            (&invalid_control as *const ControlDescriptor).cast(),
            provider
        ));
    }

    #[cfg(unix)]
    #[test]
    fn provider_descriptor_exposes_only_the_validated_pointer() {
        let providers = super::ProviderSet::load().unwrap();
        let descriptor = providers.descriptor("librptadviax2.so.1").unwrap();
        // SAFETY: `descriptor` is borrowed from the live set and the fixture exposes its own ABI.
        assert!(!unsafe { descriptor.as_ptr() }.is_null());
        assert!(providers.descriptor("not-installed.so").is_none());
    }

    #[test]
    fn cm119_resolution_passes_node_settings_to_audio_and_gpio_adapters() {
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\ndevice_identifier=3-1\nusb_serial=SERIAL-A\ncm119_profile=nhrc\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\ncm119_clip_led_gpio=2\n[1000]\n",
        )
        .unwrap();
        let radio = &crate::resolve_radio_nodes(&document).unwrap().value[0];
        let audio = crate::abi::rptadv_audio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_adapter_descriptor>() as u32,
            abi_version: 2,
            capability_name: c"rptadv.portaudio-alsa-audio".as_ptr(),
            usb_device_select: Some(fake_audio_device_select),
            ..unsafe { std::mem::zeroed() }
        };
        let gpio = crate::abi::rptadv_gpio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.cm119-hid-gpio".as_ptr(),
            device_probe: Some(fake_gpio_probe),
            ..unsafe { std::mem::zeroed() }
        };

        let selected = super::resolve_cm119_device(&audio, &gpio, radio).unwrap();

        assert_eq!(selected.usb_interface_path, "3-1:1.0");
        assert_eq!(selected.usb_port_path, "3-1");
        assert_eq!(selected.usb_serial.as_deref(), Some("SERIAL-A"));
        assert_eq!(selected.alsa_card_index, 4);
        assert_eq!(selected.input_device_index, 6);
        assert_eq!(selected.output_device_index, 7);
        assert_eq!(selected.gpio_vendor_id, 0x0d8c);
        assert_eq!(selected.gpio_product_id, 0x013c);
    }

    #[test]
    fn cm119_resolution_uses_the_configured_automatic_audio_selection() {
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\ndevice_selection=automatic_lowest_alsa_card\ncm119_profile=nhrc\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\n[1000]\n",
        )
        .unwrap();
        let radio = &crate::resolve_radio_nodes(&document).unwrap().value[0];
        let audio = crate::abi::rptadv_audio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_adapter_descriptor>() as u32,
            abi_version: 2,
            capability_name: c"rptadv.portaudio-alsa-audio".as_ptr(),
            usb_device_select: Some(fake_audio_automatic_select),
            ..unsafe { std::mem::zeroed() }
        };
        let gpio = crate::abi::rptadv_gpio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.cm119-hid-gpio".as_ptr(),
            device_probe: Some(fake_gpio_probe),
            ..unsafe { std::mem::zeroed() }
        };

        let selected = resolve_cm119_device(&audio, &gpio, radio).unwrap();
        assert_eq!(selected.usb_serial.as_deref(), Some("SERIAL-A"));

        let gpio = crate::abi::rptadv_gpio_adapter_descriptor {
            device_probe: Some(fake_gpio_no_serial),
            ..gpio
        };
        let selected = resolve_cm119_device(&audio, &gpio, radio).unwrap();
        assert_eq!(selected.usb_serial.as_deref(), Some("SERIAL-A"));
    }

    #[test]
    fn provider_set_resolves_cm119_and_reports_missing_adapter_descriptors() {
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\ndevice_identifier=3-1\nusb_serial=SERIAL-A\ncm119_profile=nhrc\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\n[1000]\n",
        )
        .unwrap();
        let radio = &crate::resolve_radio_nodes(&document).unwrap().value[0];
        let audio = Box::leak(Box::new(crate::abi::rptadv_audio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_adapter_descriptor>() as u32,
            abi_version: 2,
            capability_name: c"rptadv.portaudio-alsa-audio".as_ptr(),
            usb_device_select: Some(fake_audio_device_select),
            ..unsafe { std::mem::zeroed() }
        }));
        let gpio = Box::leak(Box::new(crate::abi::rptadv_gpio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.cm119-hid-gpio".as_ptr(),
            device_probe: Some(fake_gpio_probe),
            ..unsafe { std::mem::zeroed() }
        }));

        let empty = super::ProviderSet {
            _libraries: Vec::new(),
            descriptors: Vec::new(),
        };
        assert_eq!(
            empty.resolve_cm119_device(radio),
            Err(Cm119DeviceError::IncompleteProvider)
        );
        let audio_only = super::ProviderSet {
            _libraries: Vec::new(),
            descriptors: vec![loaded_provider(
                "librptadv_portaudio_alsa_adapter.so.2",
                audio,
            )],
        };
        assert_eq!(
            audio_only.resolve_cm119_device(radio),
            Err(Cm119DeviceError::IncompleteProvider)
        );
        let complete = super::ProviderSet {
            _libraries: Vec::new(),
            descriptors: vec![
                loaded_provider("librptadv_portaudio_alsa_adapter.so.2", audio),
                loaded_provider("librptadv_gpio_adapter.so.1", gpio),
            ],
        };
        assert!(complete.resolve_cm119_device(radio).is_ok());
    }

    #[test]
    fn cm119_resolution_rejects_incomplete_or_mismatched_adapters() {
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\ndevice_identifier=3-1\nusb_serial=SERIAL-A\ncm119_profile=nhrc\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\n[1000]\n",
        )
        .unwrap();
        let radio = &crate::resolve_radio_nodes(&document).unwrap().value[0];
        let mut audio = crate::abi::rptadv_audio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_adapter_descriptor>() as u32,
            abi_version: 2,
            capability_name: c"rptadv.portaudio-alsa-audio".as_ptr(),
            usb_device_select: Some(fake_audio_device_select),
            ..unsafe { std::mem::zeroed() }
        };
        let mut gpio = crate::abi::rptadv_gpio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.cm119-hid-gpio".as_ptr(),
            device_probe: Some(fake_gpio_probe),
            ..unsafe { std::mem::zeroed() }
        };

        audio.usb_device_select = None;
        assert_eq!(
            resolve_cm119_device(&audio, &gpio, radio),
            Err(Cm119DeviceError::IncompleteProvider)
        );
        audio.usb_device_select = Some(fake_audio_device_select);
        gpio.device_probe = None;
        assert_eq!(
            resolve_cm119_device(&audio, &gpio, radio),
            Err(Cm119DeviceError::IncompleteProvider)
        );
        gpio.device_probe = Some(fake_gpio_probe);

        let mut no_identity = radio.clone();
        no_identity.settings.device_identifier.clear();
        no_identity.settings.usb_serial.clear();
        assert_eq!(
            resolve_cm119_device(&audio, &gpio, &no_identity),
            Err(Cm119DeviceError::InvalidIdentity)
        );

        for (select, expected) in [
            (
                fake_audio_failure as _,
                Cm119DeviceError::AudioSelectionFailed,
            ),
            (
                fake_audio_invalid_output as _,
                Cm119DeviceError::InvalidAudioSelection,
            ),
            (
                fake_audio_invalid_version as _,
                Cm119DeviceError::InvalidAudioSelection,
            ),
            (
                fake_audio_invalid_selection_size as _,
                Cm119DeviceError::InvalidAudioSelection,
            ),
            (
                fake_audio_invalid_selection_version as _,
                Cm119DeviceError::InvalidAudioSelection,
            ),
            (
                fake_audio_wrong_serial as _,
                Cm119DeviceError::AudioSerialMismatch,
            ),
        ] {
            audio.usb_device_select = Some(select);
            assert_eq!(resolve_cm119_device(&audio, &gpio, radio), Err(expected));
        }
        audio.usb_device_select = Some(fake_audio_device_select);

        for (probe, expected) in [
            (
                fake_gpio_failure as _,
                Cm119DeviceError::GpioProbeFailed(-1),
            ),
            (
                fake_gpio_absent as _,
                Cm119DeviceError::GpioDeviceNotPresent,
            ),
            (
                fake_gpio_wrong_serial as _,
                Cm119DeviceError::GpioSerialMismatch,
            ),
        ] {
            gpio.device_probe = Some(probe);
            assert_eq!(resolve_cm119_device(&audio, &gpio, radio), Err(expected));
        }
        for probe in [fake_gpio_short_info as _, fake_gpio_wrong_version as _] {
            gpio.device_probe = Some(probe);
            assert_eq!(
                resolve_cm119_device(&audio, &gpio, radio),
                Err(Cm119DeviceError::IncompleteProvider)
            );
        }
    }

    #[test]
    fn cm119_profile_mapping_covers_all_supported_profiles() {
        let document = rpt_advanced_core::config::ConfigDocument::parse(
            "[radio]\ndevice_identifier=3-1\nusb_serial=SERIAL-A\ncm119_ptt_inverted=yes\ncm119_gpio_1_mode=out1\n[1000]\n",
        )
        .unwrap();
        let mut radio = crate::resolve_radio_nodes(&document).unwrap().value[0].clone();
        let audio = crate::abi::rptadv_audio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_adapter_descriptor>() as u32,
            abi_version: 2,
            capability_name: c"rptadv.portaudio-alsa-audio".as_ptr(),
            usb_device_select: Some(fake_audio_device_select),
            ..unsafe { std::mem::zeroed() }
        };
        let gpio = crate::abi::rptadv_gpio_adapter_descriptor {
            struct_size: std::mem::size_of::<crate::abi::rptadv_gpio_adapter_descriptor>() as u32,
            abi_version: 1,
            capability_name: c"rptadv.cm119-hid-gpio".as_ptr(),
            device_probe: Some(fake_gpio_probe),
            ..unsafe { std::mem::zeroed() }
        };
        for (profile, probe) in [
            (
                rpt_advanced_core::config::Cm119Profile::DudeUsb,
                fake_gpio_dudeusb as _,
            ),
            (
                rpt_advanced_core::config::Cm119Profile::SphUsb,
                fake_gpio_sphusb as _,
            ),
            (
                rpt_advanced_core::config::Cm119Profile::Nhrc,
                fake_gpio_probe as _,
            ),
            (
                rpt_advanced_core::config::Cm119Profile::Custom,
                fake_gpio_custom as _,
            ),
        ] {
            radio.settings.cm119_profile = profile;
            let mut profile_gpio = gpio;
            profile_gpio.device_probe = Some(probe);
            assert!(resolve_cm119_device(&audio, &profile_gpio, &radio).is_ok());
        }
    }

    unsafe extern "C" fn fake_audio_device_select(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 0) }
    }

    unsafe extern "C" fn fake_audio_failure(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 1) }
    }

    unsafe extern "C" fn fake_audio_invalid_output(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 2) }
    }

    unsafe extern "C" fn fake_audio_wrong_serial(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 3) }
    }

    unsafe extern "C" fn fake_audio_invalid_version(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 4) }
    }

    unsafe extern "C" fn fake_audio_invalid_selection_size(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 5) }
    }

    unsafe extern "C" fn fake_audio_invalid_selection_version(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 6) }
    }

    unsafe extern "C" fn fake_audio_automatic_select(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
    ) -> crate::abi::rptadv_audio_result {
        unsafe { audio_select(selector, matched, 8) }
    }

    unsafe fn audio_select(
        selector: *const crate::abi::rptadv_audio_usb_device_selector,
        matched: *mut crate::abi::rptadv_audio_usb_device_match,
        mode: u32,
    ) -> crate::abi::rptadv_audio_result {
        let Some(selector) = (unsafe { selector.as_ref() }) else {
            return -1;
        };
        if mode == 1 {
            return -1;
        }
        let automatic = mode == 8;
        let id_matches = if automatic {
            selector.device_identifier.is_null()
        } else {
            !selector.device_identifier.is_null()
                && unsafe { CStr::from_ptr(selector.device_identifier) }.to_bytes() == b"3-1"
        };
        let serial_matches = if automatic {
            selector.usb_serial.is_null()
        } else {
            !selector.usb_serial.is_null()
                && unsafe { CStr::from_ptr(selector.usb_serial) }.to_bytes() == b"SERIAL-A"
        };
        let expected_policy = if automatic {
            crate::abi::rptadv_audio_usb_selection_policy_RPTADV_AUDIO_USB_SELECTION_AUTOMATIC_LOWEST_ALSA_CARD
        } else {
            crate::abi::rptadv_audio_usb_selection_policy_RPTADV_AUDIO_USB_SELECTION_EXACT
        };
        if !id_matches
            || !serial_matches
            || selector.selection_policy != expected_policy
            || selector.input_device_channels != 1
            || selector.output_device_channels != 1
            || matched.is_null()
        {
            return -1;
        }
        let matched = unsafe { &mut *matched };
        if mode == 2 {
            matched.struct_size = 0;
            return 0;
        }
        matched.struct_size = std::mem::size_of_val(matched) as u32;
        matched.abi_version = 2;
        if mode == 4 {
            matched.abi_version = 3;
        }
        copy_c_string(&mut matched.usb_interface_path, b"3-1:1.0");
        copy_c_string(
            &mut matched.usb_serial,
            if mode == 3 {
                &b"OTHER"[..]
            } else {
                &b"SERIAL-A"[..]
            },
        );
        matched.selection = crate::abi::rptadv_audio_usb_device_selection {
            struct_size: std::mem::size_of::<crate::abi::rptadv_audio_usb_device_selection>()
                as u32,
            abi_version: 2,
            alsa_card_index: 4,
            input_device_index: 6,
            output_device_index: 7,
        };
        if mode == 5 {
            matched.selection.struct_size = 0;
        } else if mode == 6 {
            matched.selection.abi_version = 3;
        }
        0
    }

    unsafe extern "C" fn fake_gpio_probe(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 0) }
    }

    unsafe extern "C" fn fake_gpio_failure(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 1) }
    }

    unsafe extern "C" fn fake_gpio_absent(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 2) }
    }

    unsafe extern "C" fn fake_gpio_wrong_serial(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 3) }
    }

    unsafe extern "C" fn fake_gpio_short_info(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 4) }
    }

    unsafe extern "C" fn fake_gpio_wrong_version(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 5) }
    }

    unsafe extern "C" fn fake_gpio_no_serial(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 2, 6) }
    }

    unsafe extern "C" fn fake_gpio_dudeusb(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 0, 0) }
    }

    unsafe extern "C" fn fake_gpio_sphusb(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 1, 0) }
    }

    unsafe extern "C" fn fake_gpio_custom(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
    ) -> crate::abi::rptadv_gpio_result {
        unsafe { gpio_probe(config, info, 3, 0) }
    }

    unsafe fn gpio_probe(
        config: *const crate::abi::rptadv_gpio_device_config,
        info: *mut crate::abi::rptadv_gpio_device_info,
        expected_profile: u32,
        mode: u32,
    ) -> crate::abi::rptadv_gpio_result {
        let Some(config) = (unsafe { config.as_ref() }) else {
            return -1;
        };
        let path_matches = !config.usb_port_path.is_null()
            && unsafe { CStr::from_ptr(config.usb_port_path) }.to_bytes() == b"3-1";
        if !path_matches
            || config.profile != expected_profile
            || config.ptt_inverted != 1
            || config.gpio_output_enable_mask != 1
            || config.gpio_output_initial_mask != 1
            || info.is_null()
        {
            return -1;
        }
        if mode == 1 {
            return -1;
        }
        let info = unsafe { &mut *info };
        info.struct_size = std::mem::size_of_val(info) as u32;
        info.abi_version = 1;
        if mode == 4 {
            info.struct_size = 0;
        } else if mode == 5 {
            info.abi_version = 2;
        }
        info.present = u32::from(mode != 2);
        info.vendor_id = 0x0d8c;
        info.product_id = 0x013c;
        copy_c_string(
            &mut info.serial,
            match mode {
                3 => &b"OTHER"[..],
                6 => &b""[..],
                _ => &b"SERIAL-A"[..],
            },
        );
        0
    }

    fn copy_c_string<const N: usize>(destination: &mut [c_char; N], value: &[u8]) {
        assert!(value.len() < N);
        for (target, source) in destination.iter_mut().zip(value.iter().copied().chain([0])) {
            *target = source as c_char;
        }
    }

    #[test]
    fn runtime_product_validation_rejects_missing_lifecycle_operations() {
        let mut descriptor: crate::abi::rptadv_product_descriptor_v1 =
            unsafe { std::mem::zeroed() };
        descriptor.struct_size = std::mem::size_of_val(&descriptor) as u32;
        descriptor.abi_version = 3;
        descriptor.capability = *b"rptadv.prod3\0\0\0\0";
        assert!(!super::product_functions_complete(&descriptor));
    }

    #[cfg(unix)]
    #[test]
    fn product_runtime_descriptors_reject_incomplete_product_tables() {
        let directory = ProviderFixture::new(3, false, false);
        let providers = Box::leak(Box::new(
            super::ProviderSet::load_from_directory(directory.path()).unwrap(),
        ));
        assert_eq!(
            providers.product_runtime_descriptors().err(),
            Some(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1"
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn product_runtime_descriptors_reject_full_size_tables_with_missing_operations() {
        let mut providers = super::ProviderSet::load().unwrap();
        let mut descriptor: crate::abi::rptadv_product_descriptor_v1 =
            unsafe { std::mem::zeroed() };
        descriptor.struct_size = std::mem::size_of_val(&descriptor) as u32;
        descriptor.abi_version = 3;
        descriptor.capability = *b"rptadv.prod3\0\0\0\0";
        replace_descriptor(&mut providers, "librptadv_product.so.1", descriptor);
        let providers = Box::leak(Box::new(providers));

        assert_eq!(
            providers.product_runtime_descriptors().err(),
            Some(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1"
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_set_loads_only_matching_native_descriptors() {
        let directory = ProviderFixture::new(3, false, false);
        let providers = super::ProviderSet::load_from_directory(directory.path()).unwrap();
        assert!(providers.descriptor("librptadv_product.so.1").is_some());
        assert!(providers.descriptor("librptadviax2.so.1").is_some());
        assert!(providers.descriptor("missing-provider.so").is_none());
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1"
            ))
        );

        let missing = ProviderFixture::new(3, true, false);
        assert_eq!(
            super::ProviderSet::load_from_directory(missing.path()).err(),
            Some(ProviderError::MissingDescriptor("librptadv_product.so.1"))
        );

        let incompatible = ProviderFixture::new(4, false, false);
        assert_eq!(
            super::ProviderSet::load_from_directory(incompatible.path()).err(),
            Some(ProviderError::IncompatibleDescriptor(
                "librptadv_product.so.1"
            ))
        );

        let null = ProviderFixture::new(3, false, true);
        assert_eq!(
            super::ProviderSet::load_from_directory(null.path()).err(),
            Some(ProviderError::MissingDescriptor("librptadv_product.so.1"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn installed_provider_set_has_complete_runtime_tables() {
        let providers = Box::leak(Box::new(super::ProviderSet::load().unwrap()));

        providers.validate_runtime().unwrap();
        let radio = providers.radio_runtime_descriptors().unwrap();
        assert!(radio.radio.session_create.is_some());
        let product = providers.product_runtime_descriptors().unwrap();
        assert!(product.product.start.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_function_tables_reject_each_missing_operation() {
        let providers = super::ProviderSet::load().unwrap();
        let mut product = *providers
            .typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>("librptadv_product.so.1")
            .unwrap();
        let mut radio = *providers
            .typed_descriptor::<crate::abi::rptadv_radio_descriptor>("librptadvradio.so.4")
            .unwrap();
        let mut audio = *providers
            .typed_descriptor::<crate::abi::rptadv_audio_adapter_descriptor>(
                "librptadv_portaudio_alsa_adapter.so.2",
            )
            .unwrap();
        let mut gpio = *providers
            .typed_descriptor::<crate::abi::rptadv_gpio_adapter_descriptor>(
                "librptadv_gpio_adapter.so.1",
            )
            .unwrap();
        let mut ffmpeg = *providers
            .typed_descriptor::<crate::abi::rptadv_ffmpeg_adapter_descriptor>(
                "librptadv_ffmpeg_adapter.so.1",
            )
            .unwrap();

        assert!(super::product_functions_complete(&product));
        missing_fields!(product, super::product_functions_complete, start reload stop authorize_incoming incoming link_command link_status digit);
        assert!(super::radio_functions_complete(&radio));
        missing_fields!(radio, super::radio_functions_complete, session_create session_warm session_receive session_transmit session_snapshot session_pop_receive_event session_pop_transmit_event session_destroy session_prepare_update session_apply_receive_update session_apply_transmit_update session_destroy_update);
        assert!(super::audio_functions_complete(&audio));
        missing_fields!(audio, super::audio_functions_complete, stream_create stream_start stream_stop stream_get_stats stream_destroy usb_device_select stream_get_timing);
        assert!(super::gpio_functions_complete(&gpio));
        missing_fields!(gpio, super::gpio_functions_complete, device_probe device_open device_publish_outputs device_service device_get_inputs device_get_stats device_close);
        assert!(super::ffmpeg_functions_complete(&ffmpeg));
        missing_fields!(ffmpeg, super::ffmpeg_functions_complete, create destroy process_block);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_validation_reports_each_incomplete_provider_table() {
        let mut providers = super::ProviderSet::load().unwrap();
        let valid_product = *providers
            .typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>("librptadv_product.so.1")
            .unwrap();
        let valid_control = *providers
            .typed_descriptor::<crate::abi::rptadv_control_descriptor_v1>(
                "librptadv_control_standalone_adapter.so.1",
            )
            .unwrap();
        let valid_file = *providers
            .typed_descriptor::<crate::abi::rptadv_file_descriptor>("librptadv_file_adapter.so.1")
            .unwrap();
        let valid_speech = *providers
            .typed_descriptor::<crate::abi::rptadv_speech_descriptor>(
                "librptadv_speech_adapter.so.1",
            )
            .unwrap();
        let valid_radio = *providers
            .typed_descriptor::<crate::abi::rptadv_radio_descriptor>("librptadvradio.so.4")
            .unwrap();
        let valid_audio = *providers
            .typed_descriptor::<crate::abi::rptadv_audio_adapter_descriptor>(
                "librptadv_portaudio_alsa_adapter.so.2",
            )
            .unwrap();
        let valid_gpio = *providers
            .typed_descriptor::<crate::abi::rptadv_gpio_adapter_descriptor>(
                "librptadv_gpio_adapter.so.1",
            )
            .unwrap();
        let valid_ffmpeg = *providers
            .typed_descriptor::<crate::abi::rptadv_ffmpeg_adapter_descriptor>(
                "librptadv_ffmpeg_adapter.so.1",
            )
            .unwrap();

        replace_descriptor(&mut providers, "librptadv_product.so.1", unsafe {
            std::mem::zeroed::<crate::abi::rptadv_product_descriptor_v1>()
        });
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1"
            ))
        );
        replace_descriptor(&mut providers, "librptadv_product.so.1", valid_product);

        let control_library = "librptadv_control_standalone_adapter.so.1";
        for operation in 0..4 {
            let mut descriptor = valid_control;
            match operation {
                0 => descriptor.open = None,
                1 => descriptor.submit = None,
                2 => descriptor.stop_and_drain = None,
                _ => descriptor.close = None,
            }
            replace_descriptor(&mut providers, control_library, descriptor);
            assert_eq!(
                providers.validate_runtime(),
                Err(ProviderError::IncompleteDescriptor(control_library))
            );
        }
        replace_descriptor(&mut providers, control_library, valid_control);

        let file_library = "librptadv_file_adapter.so.1";
        for operation in 0..5 {
            let mut descriptor = valid_file;
            match operation {
                0 => descriptor.create = None,
                1 => descriptor.destroy = None,
                2 => descriptor.open_file = None,
                3 => descriptor.read_stream = None,
                _ => descriptor.close_stream = None,
            }
            replace_descriptor(&mut providers, file_library, descriptor);
            assert_eq!(
                providers.validate_runtime(),
                Err(ProviderError::IncompleteDescriptor(file_library))
            );
        }
        replace_descriptor(&mut providers, file_library, valid_file);

        let speech_library = "librptadv_speech_adapter.so.1";
        for operation in 0..5 {
            let mut descriptor = valid_speech;
            match operation {
                0 => descriptor.create = None,
                1 => descriptor.destroy = None,
                2 => descriptor.open_speech = None,
                3 => descriptor.read_stream = None,
                _ => descriptor.close_stream = None,
            }
            replace_descriptor(&mut providers, speech_library, descriptor);
            assert_eq!(
                providers.validate_runtime(),
                Err(ProviderError::IncompleteDescriptor(speech_library))
            );
        }
        replace_descriptor(&mut providers, speech_library, valid_speech);

        replace_descriptor(&mut providers, "librptadvradio.so.4", unsafe {
            std::mem::zeroed::<crate::abi::rptadv_radio_descriptor>()
        });
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor("librptadvradio.so.4"))
        );
        replace_descriptor(&mut providers, "librptadvradio.so.4", valid_radio);

        replace_descriptor(
            &mut providers,
            "librptadv_portaudio_alsa_adapter.so.2",
            unsafe { std::mem::zeroed::<crate::abi::rptadv_audio_adapter_descriptor>() },
        );
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_portaudio_alsa_adapter.so.2"
            ))
        );
        replace_descriptor(
            &mut providers,
            "librptadv_portaudio_alsa_adapter.so.2",
            valid_audio,
        );

        replace_descriptor(&mut providers, "librptadv_gpio_adapter.so.1", unsafe {
            std::mem::zeroed::<crate::abi::rptadv_gpio_adapter_descriptor>()
        });
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_gpio_adapter.so.1"
            ))
        );
        replace_descriptor(&mut providers, "librptadv_gpio_adapter.so.1", valid_gpio);

        replace_descriptor(&mut providers, "librptadv_ffmpeg_adapter.so.1", unsafe {
            std::mem::zeroed::<crate::abi::rptadv_ffmpeg_adapter_descriptor>()
        });
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_ffmpeg_adapter.so.1"
            ))
        );
        replace_descriptor(
            &mut providers,
            "librptadv_ffmpeg_adapter.so.1",
            valid_ffmpeg,
        );
    }

    fn replace_descriptor<T>(providers: &mut super::ProviderSet, library: &str, descriptor: T) {
        let descriptor = Box::leak(Box::new(descriptor));
        let provider = providers
            .descriptors
            .iter_mut()
            .find(|provider| provider.library == library)
            .unwrap();
        provider.descriptor = std::ptr::NonNull::from(descriptor).cast();
        provider.size = std::mem::size_of::<T>();
    }

    fn loaded_provider<T>(library: &'static str, descriptor: &'static T) -> super::LoadedProvider {
        super::LoadedProvider {
            library,
            size: std::mem::size_of::<T>(),
            descriptor: std::ptr::NonNull::from(descriptor).cast(),
        }
    }

    #[cfg(unix)]
    struct ProviderFixture(std::path::PathBuf);

    #[cfg(unix)]
    impl ProviderFixture {
        fn new(product_version: u32, omit_product: bool, null_product: bool) -> Self {
            use std::process::Command;

            static NEXT_FIXTURE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let serial = NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "rpt-advanced-providers-{}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&directory).unwrap();
            let source = directory.join("providers.c");
            let library = directory.join("providers.so");
            std::fs::write(&source, provider_fixture_source()).unwrap();
            let mut compile = Command::new("cc");
            compile.args(["-shared", "-fPIC"]);
            compile.arg(format!("-DPRODUCT_VERSION={product_version}"));
            if omit_product {
                compile.arg("-DOMIT_PRODUCT");
            }
            if null_product {
                compile.arg("-DNULL_PRODUCT");
            }
            let status = compile
                .arg(&source)
                .arg("-o")
                .arg(&library)
                .status()
                .expect("start fixture C compiler");
            assert!(status.success(), "compile provider fixture");
            for provider in super::PROVIDERS {
                std::os::unix::fs::symlink(&library, directory.join(provider.library)).unwrap();
            }
            Self(directory)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    #[cfg(unix)]
    impl Drop for ProviderFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[cfg(unix)]
    fn provider_fixture_source() -> &'static str {
        r#"
            #include <stddef.h>
            #include <stdint.h>
            #ifndef PRODUCT_VERSION
            #define PRODUCT_VERSION 3
            #endif
            typedef struct { uint32_t size, version; char capability[16]; } Inline;
            typedef struct { uint32_t size, version; const char *capability; } Named;
            typedef struct { uint32_t version; size_t size; const char *capability; } Control;
            #define INLINE(name, symbol, abi, text) \
                static Inline name = { sizeof(Inline), abi, text }; \
                void *symbol(void) { return &name; }
            #define NAMED(name, symbol, abi, text) \
                static Named name = { sizeof(Named), abi, text }; \
                void *symbol(void) { return &name; }
            #ifdef NULL_PRODUCT
            void *rptadv_product_descriptor_v1(void) { return NULL; }
            #elif !defined(OMIT_PRODUCT)
            INLINE(product, rptadv_product_descriptor_v1, PRODUCT_VERSION, "rptadv.prod3")
            #endif
            static Control control = { 1, sizeof(Control), "rptadv.control" };
            void *rptadv_control_standalone_descriptor_v1(void) { return &control; }
            INLINE(file, rptadv_file_adapter_descriptor, 2, "rptadv.file")
            INLINE(speech, rptadv_speech_adapter_descriptor, 2, "rptadv.speech")
            INLINE(iax2, rptadv_iax2_client_descriptor_v1, 1, "rptadv.iax2.v1")
            NAMED(radio, rptadv_radio_descriptor, 4, "rptadv.radio-core")
            NAMED(audio, rptadv_portaudio_alsa_adapter_descriptor, 2, "rptadv.portaudio-alsa-audio")
            NAMED(ffmpeg, rptadv_ffmpeg_adapter_descriptor, 1, "rptadv.ffmpeg")
            NAMED(gpio, rptadv_gpio_adapter_descriptor, 1, "rptadv.cm119-hid-gpio")
        "#
    }
}
