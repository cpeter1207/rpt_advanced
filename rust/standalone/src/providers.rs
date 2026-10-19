//! Dynamic provider loading and descriptor compatibility checks.

use libloading::Library;
use std::{
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
};

const PROVIDERS: [ProviderSpec; 12] = [
    inline(
        "librptadv_product.so.1",
        "rptadv_product_descriptor_v1",
        4,
        b"rptadv.prod4",
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
    inline(
        "librptadv_resolver_adapter.so.1",
        "rptadv_resolver_adapter_descriptor_v1",
        1,
        b"rptadv.resolver",
    ),
    inline(
        "librptadv_jitter_adapter.so.1",
        "rptadv_jitter_adapter_descriptor_v1",
        1,
        b"rptadv.jitter",
    ),
    system(inline(
        "librptadviax2.so.1",
        "rptadv_iax2_client_descriptor_v1",
        1,
        b"rptadv.iax2.v1",
    )),
    pointer(
        "libusbradioplus_product.so.1",
        "usbradioplus_product_descriptor_v1",
        1,
        b"usbradioplus.product1",
        DescriptorLayout::Named,
    ),
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

/// A product-only load for configuration checking, before device providers are needed.
pub(crate) struct ProductInspection {
    _library: ProviderLibrary,
    api: NonNull<crate::abi::rptadv_product_descriptor_v1>,
}

impl ProductInspection {
    pub(crate) fn open() -> Result<Self, ProviderError> {
        let spec = PROVIDERS[0];
        let library = ProviderLibrary::open(spec, None)?;
        let descriptor = library
            .descriptor(spec.symbol)
            .ok_or(ProviderError::MissingDescriptor(spec.library))?;
        let api = inspection_api(descriptor, spec)?;
        Ok(Self {
            _library: library,
            api,
        })
    }

    pub(crate) fn api(&self) -> &crate::abi::rptadv_product_descriptor_v1 {
        // The retained library keeps the complete immutable descriptor loaded.
        unsafe { self.api.as_ref() }
    }
}

fn inspection_api(
    descriptor: *const c_void,
    spec: ProviderSpec,
) -> Result<NonNull<crate::abi::rptadv_product_descriptor_v1>, ProviderError> {
    if !valid_descriptor(descriptor, spec) {
        return Err(ProviderError::IncompatibleDescriptor(spec.library));
    }
    if descriptor_size(descriptor, spec.layout)
        != std::mem::size_of::<crate::abi::rptadv_product_descriptor_v1>()
    {
        return Err(ProviderError::IncompleteDescriptor(spec.library));
    }
    let api = NonNull::new(
        descriptor
            .cast_mut()
            .cast::<crate::abi::rptadv_product_descriptor_v1>(),
    )
    .expect("descriptor was checked for null");
    if !product_functions_complete(unsafe { api.as_ref() }) {
        return Err(ProviderError::IncompleteDescriptor(spec.library));
    }
    Ok(api)
}

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
    /// A required provider could not create its runtime context.
    Initialize(&'static str),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Initialize(name) => {
                write!(formatter, "cannot initialize required provider {name}")
            }
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

/// All standalone runtime libraries, kept loaded for the product's full lifetime.
pub struct ProviderSet {
    _libraries: Vec<Library>,
    descriptors: Vec<LoadedProvider>,
}

/// Native provider descriptors needed to activate one standalone radio.
pub struct RadioRuntimeDescriptors {
    /// Shared native station owner.
    pub product: &'static crate::abi::UrpAstDescriptor,
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
        // Validate the retained composition again on reload, without loading replacement code.
        for spec in PROVIDERS {
            let loaded = self
                .descriptors
                .iter()
                .find(|loaded| loaded.library == spec.library)
                .ok_or(ProviderError::MissingDescriptor(spec.library))?;
            if !valid_descriptor(loaded.descriptor.as_ptr(), spec) {
                return Err(ProviderError::IncompatibleDescriptor(spec.library));
            }
        }
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
        self.resolver_descriptor()?;
        self.jitter_descriptor()?;
        self.native_product_descriptor()?;
        let iax = self
            .descriptor("librptadviax2.so.1")
            .ok_or(ProviderError::MissingDescriptor("librptadviax2.so.1"))?;
        if !unsafe { crate::iax::valid_client_descriptor(iax.pointer.as_ptr()) }
            || !unsafe { crate::iax::executor::valid_descriptor(self.iax_async_descriptor()?) }
        {
            return Err(ProviderError::IncompleteDescriptor("librptadviax2.so.1"));
        }
        Ok(())
    }

    pub(crate) fn resolver_descriptor(
        &self,
    ) -> Result<&crate::abi::rptadv_resolver_descriptor_v1, ProviderError> {
        let api: &crate::abi::rptadv_resolver_descriptor_v1 =
            self.typed_descriptor("librptadv_resolver_adapter.so.1")?;
        if api.create.is_none()
            || api.destroy.is_none()
            || api.resolve_srv.is_none()
            || api.resolve_addresses.is_none()
        {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_resolver_adapter.so.1",
            ));
        }
        Ok(api)
    }

    /// Borrow the complete packet-jitter capability while its DSO remains retained.
    pub(crate) fn jitter_descriptor(
        &self,
    ) -> Result<&crate::abi::rptadv_jitter_descriptor_v1, ProviderError> {
        let api: &crate::abi::rptadv_jitter_descriptor_v1 =
            self.typed_descriptor("librptadv_jitter_adapter.so.1")?;
        if [
            api.create.is_some(),
            api.destroy.is_some(),
            api.reset.is_some(),
            api.put.is_some(),
            api.get.is_some(),
            api.advance.is_some(),
            api.observe.is_some(),
        ]
        .into_iter()
        .all(|present| present)
        {
            Ok(api)
        } else {
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_jitter_adapter.so.1",
            ))
        }
    }

    /// Create the IAX readiness executor from the already-retained protocol DSO.
    pub(crate) fn iax_executor(
        &'static self,
    ) -> Result<crate::iax::executor::Executor, ProviderError> {
        unsafe { crate::iax::executor::Executor::new(self.iax_async_descriptor()?) }.map_err(
            |error| match error {
                crate::iax::executor::Error::Descriptor => {
                    ProviderError::IncompatibleDescriptor("librptadviax2.so.1")
                }
                _ => ProviderError::Initialize("librptadviax2.so.1"),
            },
        )
    }
    fn iax_async_descriptor(&self) -> Result<*const c_void, ProviderError> {
        const NAME: &str = "librptadviax2.so.1";
        let index = self
            .descriptors
            .iter()
            .position(|provider| provider.library == NAME)
            .ok_or(ProviderError::MissingDescriptor(NAME))?;
        type Descriptor = unsafe extern "C" fn() -> *const c_void;
        // The host owns this executor within this provider set's validated lifetime.
        let pointer = unsafe {
            let symbol = self._libraries[index]
                .get::<Descriptor>(b"rptadv_iax2_async_descriptor_v1\0")
                .map_err(|_| ProviderError::MissingDescriptor(NAME))?;
            symbol()
        };
        Ok(pointer)
    }

    /// Borrow the radio providers for an activation owner that lives as long as this set.
    pub fn radio_runtime_descriptors(
        &'static self,
    ) -> Result<RadioRuntimeDescriptors, ProviderError> {
        Ok(RadioRuntimeDescriptors {
            product: self.native_product_descriptor()?,
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

    fn native_product_descriptor(&self) -> Result<&crate::abi::UrpAstDescriptor, ProviderError> {
        let api = self.typed_descriptor("libusbradioplus_product.so.1")?;
        if !native_product_functions_complete(api) {
            return Err(ProviderError::IncompleteDescriptor(
                "libusbradioplus_product.so.1",
            ));
        }
        Ok(api)
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
        let spec = PROVIDERS
            .iter()
            .find(|spec| spec.library == library)
            .ok_or(ProviderError::MissingDescriptor(library))?;
        if !valid_descriptor(provider.descriptor.as_ptr(), *spec) {
            return Err(ProviderError::IncompatibleDescriptor(library));
        }
        if descriptor_size(provider.descriptor.as_ptr(), spec.layout) < std::mem::size_of::<T>() {
            return Err(ProviderError::IncompleteDescriptor(library));
        }
        // SAFETY: load_at checked ABI identity and this method checks the full structure size;
        // each caller uses the corresponding public C header's generated Rust type.
        Ok(unsafe { provider.descriptor.cast::<T>().as_ref() })
    }
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
        && api.inspect_configuration.is_some()
        && api.inspect_secrets.is_some()
}

fn native_product_functions_complete(api: &crate::abi::UrpAstDescriptor) -> bool {
    api.native_create.is_some()
        && api.native_start.is_some()
        && api.native_stop.is_some()
        && api.native_destroy.is_some()
        && api.native_site_bind.is_some()
        && api.native_site_publish.is_some()
        && api.native_site_snapshot.is_some()
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
pub(crate) mod tests {
    use super::{
        DescriptorLayout, ProviderError, ProviderLibrary, ProviderSpec,
        native_product_functions_complete, valid_descriptor,
    };
    use std::ffi::{c_char, c_void};
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

    pub(crate) fn empty_provider_set() -> super::ProviderSet {
        super::ProviderSet {
            _libraries: Vec::new(),
            descriptors: Vec::new(),
        }
    }

    #[test]
    fn jitter_is_required_by_the_standalone_manifest() {
        let spec = super::PROVIDERS
            .iter()
            .find(|spec| spec.library == "librptadv_jitter_adapter.so.1")
            .expect("packet jitter must come from a retained versioned provider");
        assert_eq!(spec.symbol, "rptadv_jitter_adapter_descriptor_v1");
        assert_eq!(spec.abi_version, 1);
        assert_eq!(spec.capability, b"rptadv.jitter");
    }

    #[test]
    fn jitter_requires_a_full_table_and_each_operation() {
        const NAME: &str = "librptadv_jitter_adapter.so.1";
        let mut providers = super::ProviderSet::load().unwrap();
        let valid = *providers.jitter_descriptor().unwrap();
        let changes: &[fn(&mut crate::abi::rptadv_jitter_descriptor_v1)] = &[
            |api| api.create = None,
            |api| api.destroy = None,
            |api| api.reset = None,
            |api| api.put = None,
            |api| api.get = None,
            |api| api.advance = None,
            |api| api.observe = None,
            |api| api.struct_size -= 1,
        ];
        for change in changes {
            let mut api = valid;
            change(&mut api);
            replace_descriptor(&mut providers, NAME, api);
            assert_eq!(
                providers.jitter_descriptor().err(),
                Some(ProviderError::IncompleteDescriptor(NAME))
            );
        }
    }

    #[test]
    fn executor_requires_the_retained_protocol_capability() {
        const NAME: &str = "librptadviax2.so.1";
        let empty = Box::leak(Box::new(empty_provider_set()));
        assert_eq!(
            empty.iax_executor().err(),
            Some(ProviderError::MissingDescriptor(NAME))
        );
        let directory = ProviderFixture::new(4, false, false);
        let providers = Box::leak(Box::new(
            super::ProviderSet::load_from_directory(directory.path()).unwrap(),
        ));
        assert_eq!(
            providers.iax_executor().err(),
            Some(ProviderError::MissingDescriptor(NAME))
        );
    }

    #[test]
    fn executor_preserves_descriptor_and_initialization_errors() {
        const NAME: &str = "librptadviax2.so.1";
        for (version, expected) in [
            (2, ProviderError::IncompatibleDescriptor(NAME)),
            (1, ProviderError::Initialize(NAME)),
        ] {
            let fixture = ProviderFixture::new(4, false, false);
            let source = fixture.path().join("iax-async.c");
            std::fs::write(
                &source,
                format!(
                    "{}\n{}",
                    provider_fixture_source(),
                    r#"
                    #include <stdlib.h>
                    typedef struct {
                        Inline header;
                        int (*create)(void **);
                        void (*destroy)(void *);
                        int (*attach)(void *, void *, int (*)(void *), void *, void **);
                        void (*stop)(void *);
                        void (*detach)(void *);
                        int (*configure)(void *, const void *);
                        int (*enable)(void *, void *);
                        int (*observe)(void *, void *);
                        void (*wake)(void *);
                        int (*deadline)(void *, unsigned long long);
                    } Async;
                    static int fail_create(void **out) { *out = NULL; return -1; }
                    static void unused(void *p) { (void)p; abort(); }
                    static int attach(void *r, void *p, int (*s)(void *), void *c, void **out) {
                        (void)r; (void)p; (void)s; (void)c; (void)out; abort();
                    }
                    static int configure(void *p, const void *j) { (void)p; (void)j; abort(); }
                    static int unused_pair(void *a, void *b) { (void)a; (void)b; abort(); }
                    static int deadline(void *p, unsigned long long n) { (void)p; (void)n; abort(); }
                    static Async api = {
                        { sizeof(Async), ASYNC_VERSION, "rptadv.iaxasync" },
                        fail_create, unused, attach, unused, unused,
                        configure, unused_pair, unused_pair, unused, deadline
                    };
                    void *rptadv_iax2_async_descriptor_v1(void) { return &api; }
                    "#,
                ),
            )
            .unwrap();
            let library = fixture.path().join(NAME);
            std::fs::remove_file(&library).unwrap();
            assert!(
                std::process::Command::new("cc")
                    .args(["-shared", "-fPIC"])
                    .arg(format!("-DASYNC_VERSION={version}"))
                    .arg(source)
                    .arg("-o")
                    .arg(library)
                    .status()
                    .unwrap()
                    .success()
            );
            let providers = Box::leak(Box::new(
                super::ProviderSet::load_from_directory(fixture.path()).unwrap(),
            ));
            assert_eq!(providers.iax_executor().err(), Some(expected));

            let mut runtime = super::ProviderSet::load().unwrap();
            let index = runtime
                .descriptors
                .iter()
                .position(|provider| provider.library == NAME)
                .unwrap();
            // Keep the real client descriptor alive while substituting the async capability.
            let _client_library = std::mem::replace(
                &mut runtime._libraries[index],
                unsafe { libloading::Library::new(fixture.path().join(NAME)) }.unwrap(),
            );
            assert_eq!(
                runtime.validate_runtime(),
                if version == 2 {
                    Err(ProviderError::IncompleteDescriptor(NAME))
                } else {
                    Ok(())
                }
            );
        }
    }

    #[test]
    fn resolver_is_required_by_the_standalone_manifest() {
        let spec = super::PROVIDERS
            .iter()
            .find(|spec| spec.library == "librptadv_resolver_adapter.so.1");
        assert!(
            spec.is_some(),
            "system DNS must come from a required versioned provider"
        );
    }

    #[test]
    fn resolver_loader_rejects_missing_and_incompatible_providers() {
        const NAME: &str = "librptadv_resolver_adapter.so.1";
        for (source, expected) in [
            (None, ProviderError::Load(NAME)),
            (
                Some("void other(void) {}"),
                ProviderError::MissingDescriptor(NAME),
            ),
            (
                Some("void *rptadv_resolver_adapter_descriptor_v1(void) { return 0; }"),
                ProviderError::MissingDescriptor(NAME),
            ),
            (
                Some(
                    "static struct { unsigned size, version; char cap[16]; } api = {24, 2, \"rptadv.resolver\"}; void *rptadv_resolver_adapter_descriptor_v1(void) { return &api; }",
                ),
                ProviderError::IncompatibleDescriptor(NAME),
            ),
            (
                Some(
                    "static struct { unsigned size, version; char cap[16]; } api = {24, 1, \"wrong\"}; void *rptadv_resolver_adapter_descriptor_v1(void) { return &api; }",
                ),
                ProviderError::IncompatibleDescriptor(NAME),
            ),
            (
                Some(
                    "static struct { unsigned size, version; char cap[16]; } api = {0, 1, \"rptadv.resolver\"}; void *rptadv_resolver_adapter_descriptor_v1(void) { return &api; }",
                ),
                ProviderError::IncompatibleDescriptor(NAME),
            ),
        ] {
            let fixture = ProviderFixture::new(4, false, false);
            let library = fixture.path().join(NAME);
            std::fs::remove_file(&library).unwrap();
            if let Some(source) = source {
                let path = fixture.path().join("resolver.c");
                std::fs::write(&path, source).unwrap();
                assert!(
                    std::process::Command::new("cc")
                        .args(["-shared", "-fPIC"])
                        .arg(path)
                        .arg("-o")
                        .arg(library)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            assert_eq!(
                super::ProviderSet::load_from_directory(fixture.path()).err(),
                Some(expected)
            );
        }
    }

    #[test]
    fn resolver_requires_a_full_table_and_each_operation() {
        let mut providers = super::ProviderSet::load().unwrap();
        let valid = *providers.resolver_descriptor().unwrap();
        for operation in 0..5 {
            let mut api = valid;
            match operation {
                0 => api.create = None,
                1 => api.destroy = None,
                2 => api.resolve_srv = None,
                3 => api.resolve_addresses = None,
                _ => api.struct_size -= 1,
            }
            replace_descriptor(&mut providers, "librptadv_resolver_adapter.so.1", api);
            assert_eq!(
                providers.validate_runtime(),
                Err(ProviderError::IncompleteDescriptor(
                    "librptadv_resolver_adapter.so.1"
                ))
            );
        }
    }

    #[test]
    fn runtime_validation_rechecks_descriptor_identity() {
        let mut providers = super::ProviderSet::load().unwrap();
        let mut descriptor = *providers
            .typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>("librptadv_product.so.1")
            .unwrap();
        descriptor.abi_version += 1;
        replace_descriptor(&mut providers, "librptadv_product.so.1", descriptor);
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompatibleDescriptor(
                "librptadv_product.so.1"
            ))
        );
    }

    #[test]
    fn resolver_borrow_rejects_a_changed_provider_identity() {
        let mut providers = super::ProviderSet::load().unwrap();
        let mut resolver = *providers.resolver_descriptor().unwrap();
        resolver.abi_version += 1;
        replace_descriptor(&mut providers, "librptadv_resolver_adapter.so.1", resolver);
        assert_eq!(
            providers.resolver_descriptor().err(),
            Some(ProviderError::IncompatibleDescriptor(
                "librptadv_resolver_adapter.so.1"
            ))
        );
    }

    #[test]
    fn native_runtime_rejects_missing_lifecycle_operations() {
        let mut api: crate::abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.struct_size = std::mem::size_of_val(&api) as u32;
        api.abi_version = 1;
        api.capability_name = c"usbradioplus.product1".as_ptr();
        assert!(!native_product_functions_complete(&api));
        api.native_create = Some(fake_native_create);
        api.native_start = Some(fake_native_control);
        api.native_stop = Some(fake_native_control);
        api.native_destroy = Some(fake_native_destroy);
        api.native_site_bind = Some(fake_site_bind);
        api.native_site_publish = Some(fake_site_publish);
        api.native_site_snapshot = Some(fake_site_snapshot);

        assert!(native_product_functions_complete(&api));
        missing_fields!(api, native_product_functions_complete, native_create native_start native_stop native_destroy native_site_bind native_site_publish native_site_snapshot);
        let mut incomplete = api;
        incomplete.native_start = None;
        let providers = Box::leak(Box::new(super::ProviderSet {
            _libraries: Vec::new(),
            descriptors: vec![loaded_provider(
                "libusbradioplus_product.so.1",
                Box::leak(Box::new(incomplete)),
            )],
        }));
        assert_eq!(
            providers.radio_runtime_descriptors().err(),
            Some(ProviderError::IncompleteDescriptor(
                "libusbradioplus_product.so.1"
            ))
        );
    }

    unsafe extern "C" fn fake_native_create(
        _: *const crate::abi::UrpNativeCreateArgs,
        _: *mut *mut c_void,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn fake_native_control(_: *mut c_void) -> i32 {
        0
    }

    unsafe extern "C" fn fake_native_destroy(_: *mut c_void) {}

    unsafe extern "C" fn fake_site_bind(
        _: *mut c_void,
        _: u32,
        _: *const crate::abi::UrpAstSiteBinding,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn fake_site_publish(
        _: *mut c_void,
        _: u32,
        _: *const crate::abi::UrpAstSiteRequest,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn fake_site_snapshot(
        _: *mut c_void,
        _: u32,
        _: *mut crate::abi::UrpAstSiteSnapshot,
    ) -> i32 {
        -1
    }

    #[test]
    fn site_bridge_requires_all_native_operations_before_hardware_creation() {
        // SAFETY: nullable function pointers and scalar fields admit all-zero initialization.
        let mut api: crate::abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        api.native_create = Some(fake_native_create);
        api.native_start = Some(fake_native_control);
        api.native_stop = Some(fake_native_control);
        api.native_destroy = Some(fake_native_destroy);
        assert!(!native_product_functions_complete(&api));
    }

    #[test]
    fn provider_error_messages_are_stable() {
        for (error, expected) in [
            (
                ProviderError::Initialize("initialize"),
                "cannot initialize required provider initialize",
            ),
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
    fn native_product_uses_the_shared_system_descriptor() {
        let provider = super::PROVIDERS
            .iter()
            .find(|provider| provider.library == "libusbradioplus_product.so.1")
            .expect("the standalone runtime requires the shared radio product");

        assert_eq!(provider.symbol, "usbradioplus_product_descriptor_v1");
        assert_eq!(provider.abi_version, 1);
        assert_eq!(provider.capability, b"usbradioplus.product1");
        assert!(matches!(provider.layout, DescriptorLayout::Named));
        assert!(matches!(provider.location, super::LibraryLocation::System));
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
        let capability = b"rptadv.prod4\0\0\0\0";
        let descriptor = InlineDescriptor {
            struct_size: std::mem::size_of::<InlineDescriptor>() as u32,
            abi_version: 4,
            capability: *capability,
        };
        let provider = ProviderSpec {
            library: "test",
            symbol: "test",
            abi_version: 4,
            capability: b"rptadv.prod4",
            layout: DescriptorLayout::Inline,
            location: super::LibraryLocation::Private,
        };

        assert!(valid_descriptor(
            (&descriptor as *const InlineDescriptor).cast::<c_void>(),
            provider
        ));
        let mut incompatible = descriptor;
        incompatible.abi_version = 5;
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
    fn runtime_product_validation_rejects_missing_lifecycle_operations() {
        let mut descriptor: crate::abi::rptadv_product_descriptor_v1 =
            unsafe { std::mem::zeroed() };
        descriptor.struct_size = std::mem::size_of_val(&descriptor) as u32;
        descriptor.abi_version = 4;
        descriptor.capability = *b"rptadv.prod4\0\0\0\0";
        assert!(!super::product_functions_complete(&descriptor));
    }

    #[test]
    fn configuration_only_product_rejects_incompatible_and_incomplete_tables() {
        let spec = super::PROVIDERS[0];
        // SAFETY: the descriptor consists only of scalar fields and nullable function pointers.
        let mut descriptor: crate::abi::rptadv_product_descriptor_v1 =
            unsafe { std::mem::zeroed() };
        descriptor.struct_size = std::mem::size_of_val(&descriptor) as u32;
        descriptor.abi_version = 4;
        descriptor.capability = *b"rptadv.prod4\0\0\0\0";
        let pointer = (&descriptor as *const crate::abi::rptadv_product_descriptor_v1).cast();
        assert_eq!(
            super::inspection_api(pointer, spec).err(),
            Some(ProviderError::IncompleteDescriptor(spec.library))
        );
        descriptor.struct_size = std::mem::size_of::<super::InlineDescriptorHeader>() as u32;
        assert_eq!(
            super::inspection_api(pointer, spec).err(),
            Some(ProviderError::IncompleteDescriptor(spec.library))
        );
        descriptor.abi_version = 5;
        assert_eq!(
            super::inspection_api(pointer, spec).err(),
            Some(ProviderError::IncompatibleDescriptor(spec.library))
        );
    }

    #[cfg(unix)]
    #[test]
    fn product_runtime_descriptors_reject_incomplete_product_tables() {
        let directory = ProviderFixture::new(4, false, false);
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
        descriptor.abi_version = 4;
        descriptor.capability = *b"rptadv.prod4\0\0\0\0";
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
        let directory = ProviderFixture::new(4, false, false);
        let providers = super::ProviderSet::load_from_directory(directory.path()).unwrap();
        assert!(providers.descriptor("librptadv_product.so.1").is_some());
        assert!(providers.descriptor("librptadviax2.so.1").is_some());
        assert!(
            providers
                .descriptor("libusbradioplus_product.so.1")
                .is_some()
        );
        assert!(providers.descriptor("missing-provider.so").is_none());
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.1"
            ))
        );

        let missing = ProviderFixture::new(4, true, false);
        assert_eq!(
            super::ProviderSet::load_from_directory(missing.path()).err(),
            Some(ProviderError::MissingDescriptor("librptadv_product.so.1"))
        );

        let incompatible = ProviderFixture::new(5, false, false);
        assert_eq!(
            super::ProviderSet::load_from_directory(incompatible.path()).err(),
            Some(ProviderError::IncompatibleDescriptor(
                "librptadv_product.so.1"
            ))
        );

        let null = ProviderFixture::new(4, false, true);
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
        assert!(radio.product.native_create.is_some());
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
        assert!(super::product_functions_complete(&product));
        missing_fields!(product, super::product_functions_complete, start reload stop authorize_incoming incoming link_command link_status digit inspect_configuration inspect_secrets);
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
        let valid_native = *providers
            .typed_descriptor::<crate::abi::UrpAstDescriptor>("libusbradioplus_product.so.1")
            .unwrap();

        let mut incomplete = valid_product;
        incomplete.start = None;
        replace_descriptor(&mut providers, "librptadv_product.so.1", incomplete);
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

        let mut incomplete = valid_native;
        incomplete.native_start = None;
        replace_descriptor(&mut providers, "libusbradioplus_product.so.1", incomplete);
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "libusbradioplus_product.so.1"
            ))
        );
        replace_descriptor(&mut providers, "libusbradioplus_product.so.1", valid_native);

        let iax_library = "librptadviax2.so.1";
        let truncated = super::InlineDescriptorHeader {
            struct_size: std::mem::size_of::<super::InlineDescriptorHeader>() as u32,
            abi_version: 1,
            capability: *b"rptadv.iax2.v1\0\0",
        };
        replace_descriptor(&mut providers, iax_library, truncated);
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(iax_library))
        );
    }

    pub(crate) fn replace_descriptor<T>(
        providers: &mut super::ProviderSet,
        library: &str,
        descriptor: T,
    ) {
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
            #define PRODUCT_VERSION 4
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
            INLINE(product, rptadv_product_descriptor_v1, PRODUCT_VERSION, "rptadv.prod4")
            #endif
            static Control control = { 1, sizeof(Control), "rptadv.control" };
            void *rptadv_control_standalone_descriptor_v1(void) { return &control; }
            INLINE(file, rptadv_file_adapter_descriptor, 2, "rptadv.file")
            INLINE(speech, rptadv_speech_adapter_descriptor, 2, "rptadv.speech")
            INLINE(resolver, rptadv_resolver_adapter_descriptor_v1, 1, "rptadv.resolver")
            INLINE(jitter, rptadv_jitter_adapter_descriptor_v1, 1, "rptadv.jitter")
            INLINE(iax2, rptadv_iax2_client_descriptor_v1, 1, "rptadv.iax2.v1")
            NAMED(native, usbradioplus_product_descriptor_v1, 1, "usbradioplus.product1")
            NAMED(radio, rptadv_radio_descriptor, 4, "rptadv.radio-core")
            NAMED(audio, rptadv_portaudio_alsa_adapter_descriptor, 2, "rptadv.portaudio-alsa-audio")
            NAMED(ffmpeg, rptadv_ffmpeg_adapter_descriptor, 1, "rptadv.ffmpeg")
            NAMED(gpio, rptadv_gpio_adapter_descriptor, 1, "rptadv.cm119-hid-gpio")
        "#
    }
}
