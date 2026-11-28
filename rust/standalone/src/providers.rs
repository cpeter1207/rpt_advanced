//! Dynamic provider loading and descriptor compatibility checks.

use libloading::Library;
use std::{
    ffi::{CStr, c_char, c_void},
    marker::PhantomData,
    path::Path,
    ptr::NonNull,
    sync::OnceLock,
};

const PROVIDERS: [ProviderSpec; 14] = [
    inline(
        "librptadv_product.so.3",
        "rptadv_product_descriptor_v1",
        8,
        b"rptadv.prod8",
    ),
    private(pointer(
        "librptadv_control_standalone_adapter.so.1",
        "rptadv_control_standalone_descriptor_v1",
        1,
        b"rptadv.control",
        DescriptorLayout::Control,
    )),
    inline(
        "librptadv_file_adapter.so.2",
        "rptadv_file_adapter_descriptor",
        3,
        b"rptadv.file",
    ),
    inline(
        "librptadv_speech_adapter.so.2",
        "rptadv_speech_adapter_descriptor",
        3,
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
        "librptadviax2.so.2",
        "rptadv_iax2_client_descriptor_v2",
        2,
        b"rptadv.iax2.v2",
    )),
    pointer(
        "libusbradioplus_product.so.1",
        "usbradioplus_product_descriptor_v1",
        1,
        b"usbradioplus.product1",
        DescriptorLayout::Named,
    ),
    pointer(
        "librptadvradio.so.5",
        "rptadv_radio_descriptor",
        5,
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
    pointer(
        "librptadv_rnnoise_adapter.so.1",
        "rptadv_rnnoise_adapter_descriptor",
        1,
        b"rptadv.rnnoise",
        DescriptorLayout::Named,
    ),
    pointer(
        "librptadvschedule.so.1",
        "rptadv_recurrence_descriptor",
        1,
        b"rptadv.recurrence",
        DescriptorLayout::Named,
    ),
];

/// A product-only load for configuration checking, before device providers are needed.
pub(crate) struct ProductInspection {
    library: ProviderLibrary,
}

impl ProductInspection {
    pub(crate) fn open() -> Result<Self, ProviderError> {
        let library = ProviderLibrary::open(PROVIDERS[0], None)?;
        Ok(Self { library })
    }

    pub(crate) fn managed_inspection(
        &self,
    ) -> Result<&crate::abi::rptadv_product_inspection_v1, ProviderError> {
        const NAME: &str = "librptadv_product.so.3";
        type Getter = unsafe extern "C" fn() -> *const crate::abi::rptadv_product_inspection_v1;
        // SAFETY: the named getter belongs to the checked DSO retained by this inspection owner.
        let table = unsafe {
            self.library
                .0
                .get::<Getter>(b"rptadv_product_inspection_descriptor_v1\0")
                .map_err(|_| ProviderError::MissingDescriptor(NAME))?()
        };
        // SAFETY: the returned immutable capability advertises its readable prefix.
        unsafe { inspection_table(table) }
    }
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
    _libraries: std::sync::Arc<[Library]>,
    descriptors: Vec<LoadedProvider>,
    iax: OnceLock<Result<crate::iax::IaxClient, crate::iax::IaxError>>,
}

/// The immutable native descriptor set shared by dormant and active physical stations.
pub(crate) struct PhysicalSet(ProviderSet);

// SAFETY: this private constructor selects only immutable physical ABI tables. The owning
// libraries are thread-safe. Radio operations receive independently serialized station handles.
unsafe impl Send for PhysicalSet {}
// SAFETY: the only shared access borrows immutable descriptors from the retained libraries.
unsafe impl Sync for PhysicalSet {}

impl PhysicalSet {
    pub(crate) fn load() -> Result<Self, ProviderError> {
        ProviderSet::load_selected(None, |name| {
            matches!(
                name,
                "libusbradioplus_product.so.1"
                    | "librptadvradio.so.5"
                    | "librptadv_portaudio_alsa_adapter.so.2"
                    | "librptadv_ffmpeg_adapter.so.1"
                    | "librptadv_gpio_adapter.so.1"
                    | "librptadv_rnnoise_adapter.so.1"
            )
        })
        .map(Self)
    }
}

/// Existing legacy catalog or the physical libraries retained on the first modular request.
#[derive(Clone)]
pub(crate) enum PhysicalProviders {
    Existing(&'static ProviderSet),
    Loaded(std::sync::Arc<PhysicalSet>),
}

impl AsRef<ProviderSet> for PhysicalProviders {
    fn as_ref(&self) -> &ProviderSet {
        match self {
            Self::Existing(providers) => providers,
            Self::Loaded(providers) => &providers.0,
        }
    }
}

/// Native provider descriptors needed to activate one standalone radio.
pub struct RadioRuntimeDescriptors<'a> {
    /// Shared native station owner.
    pub product: &'a crate::abi::UrpAstDescriptor,
    /// Retained radio capability; the native owner validates its consumed session prefix.
    pub radio: *const c_void,
    /// PortAudio/ALSA stream API.
    pub audio: &'a crate::abi::rptadv_audio_adapter_descriptor,
    /// CM119 HID/GPIO API.
    pub gpio: &'a crate::abi::rptadv_gpio_adapter_descriptor,
    /// Exact-frame FFmpeg graph API.
    pub ffmpeg: &'a crate::abi::rptadv_ffmpeg_adapter_descriptor,
    /// RNNoise provider retained for optional local receive noise reduction.
    pub rnnoise: *const c_void,
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
        Self::load_selected(directory, |_| true)
    }

    /// Load exactly the libraries required by the selected frontend capability.
    pub(crate) fn load_selected(
        directory: Option<&Path>,
        accepts: impl Fn(&str) -> bool,
    ) -> Result<Self, ProviderError> {
        let mut libraries = Vec::with_capacity(PROVIDERS.len());
        let mut descriptors = Vec::with_capacity(PROVIDERS.len());
        for provider in PROVIDERS.into_iter().filter(|spec| accepts(spec.library)) {
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
            _libraries: libraries.into(),
            descriptors,
            iax: OnceLock::new(),
        })
    }

    /// Open the selected codec composition once, retaining it across node reloads.
    pub(crate) fn iax_client(&self) -> Result<crate::iax::IaxClient, ProviderError> {
        self.iax_client_with(|| crate::iax::IaxClient::load(Path::new("librptadviax2.so.2")))
    }

    fn iax_client_with(
        &self,
        load: impl FnOnce() -> Result<crate::iax::IaxClient, crate::iax::IaxError>,
    ) -> Result<crate::iax::IaxClient, ProviderError> {
        self.iax
            .get_or_init(|| {
                load()?.with_provider_libraries(std::sync::Arc::clone(&self._libraries))
            })
            .clone()
            .map_err(|_| ProviderError::IncompleteDescriptor("librptadviax2.so.2"))
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
            "librptadv_product.so.3",
        )?;
        if !product_functions_complete(product) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.3",
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
            "librptadv_file_adapter.so.2",
        )?;
        if file.create.is_none()
            || file.destroy.is_none()
            || file.open_file.is_none()
            || file.step_stream.is_none()
            || file.close_stream.is_none()
        {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_file_adapter.so.2",
            ));
        }
        let speech = self.typed_descriptor::<crate::abi::rptadv_speech_descriptor>(
            "librptadv_speech_adapter.so.2",
        )?;
        if speech.create.is_none()
            || speech.destroy.is_none()
            || speech.open_speech.is_none()
            || speech.step_stream.is_none()
            || speech.close_stream.is_none()
        {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_speech_adapter.so.2",
            ));
        }
        self.resolver_descriptor()?;
        self.jitter_descriptor()?;
        self.native_product_descriptor()?;
        let iax = self
            .descriptor("librptadviax2.so.2")
            .ok_or(ProviderError::MissingDescriptor("librptadviax2.so.2"))?;
        if !unsafe { crate::iax::valid_client_descriptor(iax.pointer.as_ptr()) }
            || !unsafe { crate::iax::executor::valid_descriptor(self.iax_async_descriptor()?) }
        {
            return Err(ProviderError::IncompleteDescriptor("librptadviax2.so.2"));
        }
        // Startup prepares the immutable selection; reload reuses that catalog,
        // never loading provider code from newly replaced files.
        self.iax_client()?;
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
                    ProviderError::IncompatibleDescriptor("librptadviax2.so.2")
                }
                _ => ProviderError::Initialize("librptadviax2.so.2"),
            },
        )
    }
    fn iax_async_descriptor(&self) -> Result<*const c_void, ProviderError> {
        const NAME: &str = "librptadviax2.so.2";
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
    pub fn radio_runtime_descriptors(&self) -> Result<RadioRuntimeDescriptors<'_>, ProviderError> {
        Ok(RadioRuntimeDescriptors {
            product: self.native_product_descriptor()?,
            radio: self
                .checked_descriptor::<NamedDescriptorHeader>("librptadvradio.so.5")?
                .as_ptr(),
            audio: self.typed_descriptor("librptadv_portaudio_alsa_adapter.so.2")?,
            gpio: self.typed_descriptor("librptadv_gpio_adapter.so.1")?,
            ffmpeg: self.typed_descriptor("librptadv_ffmpeg_adapter.so.1")?,
            rnnoise: self
                .checked_descriptor::<NamedDescriptorHeader>("librptadv_rnnoise_adapter.so.1")?
                .as_ptr(),
        })
    }

    /// Retain the product and its three direct lifecycle/media descriptors. Other
    /// providers are selected only by actual attached protocol or RF owners.
    pub(crate) fn load_management() -> Result<Self, ProviderError> {
        Self::load_selected(None, |name| {
            matches!(
                name,
                "librptadv_product.so.3"
                    | "librptadv_control_standalone_adapter.so.1"
                    | "librptadv_file_adapter.so.2"
                    | "librptadv_speech_adapter.so.2"
            )
        })
    }

    pub(crate) fn management_descriptor(
        &self,
    ) -> Result<&crate::abi::rptadv_product_management_v1, ProviderError> {
        const NAME: &str = "librptadv_product.so.3";
        let index = self
            .descriptors
            .iter()
            .position(|p| p.library == NAME)
            .ok_or(ProviderError::MissingDescriptor(NAME))?;
        checked_management(&self._libraries[index])
    }

    /// Borrow validated product lifecycle descriptors while retaining all provider libraries.
    pub fn product_runtime_descriptors(
        &'static self,
    ) -> Result<ProductRuntimeDescriptors, ProviderError> {
        let product = self.typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>(
            "librptadv_product.so.3",
        )?;
        if !product_functions_complete(product) {
            return Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.3",
            ));
        }
        let control = self.typed_descriptor::<crate::abi::rptadv_control_descriptor_v1>(
            "librptadv_control_standalone_adapter.so.1",
        )?;
        let file = self.typed_descriptor::<crate::abi::rptadv_file_descriptor>(
            "librptadv_file_adapter.so.2",
        )?;
        let speech = self.typed_descriptor::<crate::abi::rptadv_speech_descriptor>(
            "librptadv_speech_adapter.so.2",
        )?;
        Ok(ProductRuntimeDescriptors {
            product,
            control,
            file,
            speech,
        })
    }

    /// Resolve a station SDK extension within the retained product DSO that owns the prepared handle.
    pub(crate) unsafe fn native_extension<T>(&self, symbol: &[u8]) -> Option<*const T> {
        let index = self
            .descriptors
            .iter()
            .position(|entry| entry.library == "libusbradioplus_product.so.1")?;
        // SAFETY: caller selects a documented immutable descriptor getter with the specified return type.
        let getter = unsafe {
            self._libraries
                .get(index)?
                .get::<unsafe extern "C" fn() -> *const T>(symbol)
        }
        .ok()?;
        // SAFETY: this retained library owns the process-lifetime table addressed by its getter.
        Some(unsafe { getter() })
    }

    pub(crate) fn native_product_descriptor(
        &self,
    ) -> Result<&crate::abi::UrpAstDescriptor, ProviderError> {
        let api = self.typed_descriptor("libusbradioplus_product.so.1")?;
        if !native_product_functions_complete(api) {
            return Err(ProviderError::IncompleteDescriptor(
                "libusbradioplus_product.so.1",
            ));
        }
        Ok(api)
    }

    fn typed_descriptor<T>(&self, library: &'static str) -> Result<&T, ProviderError> {
        let pointer = self.checked_descriptor::<T>(library)?;
        // SAFETY: the shared validation checks the complete requested type, and this
        // borrow retains the provider set and its library for the reference's lifetime.
        Ok(unsafe { pointer.cast::<T>().as_ref() })
    }

    /// Validate the consumed prefix while preserving the original descriptor pointer.
    fn checked_descriptor<T>(
        &self,
        library: &'static str,
    ) -> Result<NonNull<c_void>, ProviderError> {
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
        Ok(provider.descriptor)
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

fn checked_management(
    library: &Library,
) -> Result<&crate::abi::rptadv_product_management_v1, ProviderError> {
    const NAME: &str = "librptadv_product.so.3";
    type Getter = unsafe extern "C" fn() -> *const crate::abi::rptadv_product_management_v1;
    // SAFETY: the optional getter belongs to the same retained, ABI-checked product DSO.
    // Its borrowed immutable table cannot outlive this ProviderSet.
    unsafe {
        let getter = library
            .get::<Getter>(b"rptadv_product_management_descriptor_v1\0")
            .map_err(|_| ProviderError::MissingDescriptor(NAME))?;
        management_table(getter())
    }
}

/// Check the size word before borrowing fields from the separately versioned table.
/// The caller retains the exporting DSO and its truthful immutable readable prefix.
unsafe fn management_table<'a>(
    p: *const crate::abi::rptadv_product_management_v1,
) -> Result<&'a crate::abi::rptadv_product_management_v1, ProviderError> {
    const NAME: &str = "librptadv_product.so.3";
    if p.is_null() || !p.is_aligned() {
        return Err(ProviderError::MissingDescriptor(NAME));
    }
    // SAFETY: a non-null native getter lends at least the published size word.
    if unsafe { std::ptr::addr_of!((*p).struct_size).read() } as usize
        != size_of::<crate::abi::rptadv_product_management_v1>()
    {
        return Err(ProviderError::IncompatibleDescriptor(NAME));
    }
    // SAFETY: the exact 32-byte table is readable and retained as described above.
    let table = unsafe { &*p };
    if table.abi_version != 1
        || table.capability != *b"rptadv.mgmt1\0\0\0\0"
        || table.start.is_none()
    {
        return Err(ProviderError::IncompatibleDescriptor(NAME));
    }
    Ok(table)
}

unsafe fn inspection_table<'a>(
    p: *const crate::abi::rptadv_product_inspection_v1,
) -> Result<&'a crate::abi::rptadv_product_inspection_v1, ProviderError> {
    const NAME: &str = "librptadv_product.so.3";
    if p.is_null() || !p.is_aligned() {
        return Err(ProviderError::MissingDescriptor(NAME));
    }
    // SAFETY: the negotiated getter lends at least the initial size word.
    if unsafe { std::ptr::addr_of!((*p).struct_size).read() } as usize
        != size_of::<crate::abi::rptadv_product_inspection_v1>()
    {
        return Err(ProviderError::IncompatibleDescriptor(NAME));
    }
    // SAFETY: the advertised table is complete and its DSO remains retained by the caller.
    let table = unsafe { &*p };
    if table.abi_version != 1
        || table.capability != *b"rptadv.inspect1\0"
        || table.inspect.is_none()
    {
        return Err(ProviderError::IncompatibleDescriptor(NAME));
    }
    Ok(table)
}

fn product_functions_complete(api: &crate::abi::rptadv_product_descriptor_v1) -> bool {
    api.reload_working.is_some()
        && api.reload_poll.is_some()
        && api.stop.is_some()
        && api.authorize_incoming.is_some()
        && api.incoming.is_some()
        && api.link_command.is_some()
        && api.site_command.is_some()
        && api.link_status.is_some()
        && api.digit.is_some()
}

fn native_product_functions_complete(api: &crate::abi::UrpAstDescriptor) -> bool {
    api.native_create.is_some()
        && api.native_start.is_some()
        && api.native_stop.is_some()
        && api.native_destroy.is_some()
        && api.native_site_bind.is_some()
        && api.native_site_publish.is_some()
        && api.native_site_snapshot.is_some()
        && api.native_processing_validate.is_some()
        && api.native_link_prepare.is_some()
        && api.link_process_f32.is_some()
        && api.link_destroy.is_some()
        && api.native_configuration_resolve.is_some()
        && api.native_configuration_visit.is_some()
        && api.native_configuration_destroy.is_some()
        && api.native_get_status.is_some()
        && api.native_command.is_some()
        && api.native_set_transmit.is_some()
        && api.native_clear_manual.is_some()
        && api.native_prepare_update.is_some()
        && api.native_begin_update.is_some()
        && api.native_accept_update.is_some()
        && api.native_finish_update.is_some()
        && api.native_destroy_update.is_some()
        && api.native_status_text.is_some()
        && api.native_processing_status_text.is_some()
        && api.native_service.is_some()
        && api.native_quiesce.is_some()
        && api.native_activation_status.is_some()
        && api.native_remote_submit.is_some()
        && api.remote_source.is_some()
        && api.remote_take.is_some()
        && api.remote_destroy.is_some()
        && api.native_configuration_remote_options.is_some()
        && api.link_observer_create.is_some()
        && api.link_observer_read.is_some()
        && api.link_observer_destroy.is_some()
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
            _libraries: std::sync::Arc::from([]),
            descriptors: Vec::new(),
            iax: std::sync::OnceLock::new(),
        }
    }

    /// Use the real selected-client cache while keeping all fixture code in-process.
    pub(crate) fn selected_client_for_lifetime_test(
        destroy: unsafe extern "C" fn(*mut c_void),
        catalog: *mut c_void,
    ) -> (
        super::ProviderSet,
        crate::iax::IaxClient,
        std::sync::Weak<[libloading::Library]>,
    ) {
        let providers = empty_provider_set();
        let lease = std::sync::Arc::downgrade(&providers._libraries);
        let client = providers
            .iax_client_with(|| {
                Ok(crate::iax::tests::client_with_live_catalog(
                    destroy, catalog,
                ))
            })
            .unwrap();
        (providers, client, lease)
    }

    #[test]
    fn selected_library_lease_covers_peer_destruction() {
        use std::cell::{Cell, RefCell};
        use std::sync::{Arc, Weak};

        thread_local! {
            static SELECTED: RefCell<Option<Weak<[libloading::Library]>>> = const { RefCell::new(None) };
            static OBSERVED: Cell<Option<bool>> = const { Cell::new(None) };
        }
        unsafe extern "C" fn destroy(_: *mut c_void) {
            let live = SELECTED.with_borrow(|slot| slot.as_ref().and_then(Weak::upgrade).is_some());
            OBSERVED.set(Some(live));
        }

        let providers = empty_provider_set();
        let lease = Arc::downgrade(&providers._libraries);
        SELECTED.with_borrow_mut(|slot| *slot = Some(lease.clone()));
        let client = providers
            .iax_client_with(|| Ok(crate::iax::tests::client_with_destroy(destroy)))
            .unwrap();
        // The fake descriptor ignores the opaque token and releases it once on peer drop.
        let peer = unsafe { client.adopt_inbound(std::ptr::dangling_mut::<c_void>()) }.unwrap();
        drop(providers);
        drop(peer);
        let observed = OBSERVED.replace(None);
        SELECTED.with_borrow_mut(|slot| *slot = None);

        assert_eq!(
            observed,
            Some(true),
            "peer destroy must retain the selected library lease"
        );
        assert!(
            lease.upgrade().is_none(),
            "final peer drop must release the lease"
        );
    }

    /// Current ABI-5 session table prefix; optional profile operations are not consumed.
    #[repr(C)]
    pub(crate) struct PublishedRadioDescriptor {
        pub(crate) struct_size: u32,
        pub(crate) abi_version: u32,
        pub(crate) capability_name: *const c_char,
        operations: [unsafe extern "C" fn(); 12],
    }

    impl PublishedRadioDescriptor {
        pub(crate) fn new() -> Self {
            unsafe extern "C" fn unused() {
                panic!("standalone must forward radio operations without calling them")
            }
            Self {
                struct_size: std::mem::size_of::<Self>() as u32,
                abi_version: 5,
                capability_name: c"rptadv.radio-core".as_ptr(),
                operations: [unused; 12],
            }
        }
    }

    #[test]
    fn released_radio_descriptor_preserves_header_validation() {
        const NAME: &str = "librptadvradio.so.5";
        for (size, version, capability, expected) in [
            (112, 5, c"rptadv.radio-core", None),
            (
                15,
                5,
                c"rptadv.radio-core",
                Some(ProviderError::IncompatibleDescriptor(NAME)),
            ),
            (
                112,
                4,
                c"rptadv.radio-core",
                Some(ProviderError::IncompatibleDescriptor(NAME)),
            ),
            (
                112,
                5,
                c"rptadv.other",
                Some(ProviderError::IncompatibleDescriptor(NAME)),
            ),
        ] {
            let mut providers = super::ProviderSet::load().unwrap();
            let mut descriptor = PublishedRadioDescriptor::new();
            descriptor.struct_size = size;
            descriptor.abi_version = version;
            descriptor.capability_name = capability.as_ptr();
            replace_descriptor(&mut providers, NAME, descriptor);
            let providers = Box::leak(Box::new(providers));
            assert_eq!(providers.radio_runtime_descriptors().err(), expected);
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
        const NAME: &str = "librptadviax2.so.2";
        let empty = Box::leak(Box::new(empty_provider_set()));
        assert_eq!(
            empty.iax_executor().err(),
            Some(ProviderError::MissingDescriptor(NAME))
        );
        let directory = ProviderFixture::new(8, false, false);
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
        const NAME: &str = "librptadviax2.so.2";
        for (version, expected) in [
            (2, ProviderError::IncompatibleDescriptor(NAME)),
            (1, ProviderError::Initialize(NAME)),
        ] {
            let fixture = ProviderFixture::new(8, false, false);
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
                &mut std::sync::Arc::get_mut(&mut runtime._libraries)
                    .expect("unique fixture owner")[index],
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
            let fixture = ProviderFixture::new(8, false, false);
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
            .typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>("librptadv_product.so.3")
            .unwrap();
        descriptor.abi_version += 1;
        replace_descriptor(&mut providers, "librptadv_product.so.3", descriptor);
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompatibleDescriptor(
                "librptadv_product.so.3"
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
    fn native_runtime_requires_activation_and_remote_control_before_hardware() {
        let spec = *super::PROVIDERS
            .iter()
            .find(|spec| spec.library == "libusbradioplus_product.so.1")
            .unwrap();
        let library = ProviderLibrary::open(spec, None).unwrap();
        let descriptor = library.descriptor(spec.symbol).unwrap();
        // SAFETY: this matched SDK exports a complete immutable descriptor.
        let mut api = unsafe { descriptor.cast::<crate::abi::UrpAstDescriptor>().read() };
        assert!(native_product_functions_complete(&api));
        missing_fields!(api, native_product_functions_complete,
            native_service native_quiesce native_activation_status native_remote_submit
            remote_source remote_take remote_destroy native_configuration_remote_options);
    }

    #[test]
    fn native_runtime_rejects_missing_lifecycle_operations() {
        let providers = super::ProviderSet::load().unwrap();
        let mut api = *providers.native_product_descriptor().unwrap();
        assert!(native_product_functions_complete(&api));
        missing_fields!(api, native_product_functions_complete,
            native_create native_start native_stop native_destroy native_site_bind
            native_site_publish native_site_snapshot native_processing_validate
            native_link_prepare link_process_f32 link_destroy
            native_configuration_resolve native_configuration_visit native_configuration_destroy
            native_get_status native_command native_set_transmit native_clear_manual
            native_prepare_update native_begin_update native_accept_update native_finish_update
            native_destroy_update native_status_text native_processing_status_text
            link_observer_create link_observer_read link_observer_destroy);
        let mut incomplete = api;
        incomplete.native_start = None;
        let providers = Box::leak(Box::new(super::ProviderSet {
            _libraries: Default::default(),
            iax: std::sync::OnceLock::new(),
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

    #[test]
    fn native_admission_rejects_truncated_allocations_before_forming_a_full_reference() {
        const NAME: &str = "libusbradioplus_product.so.1";
        for bytes in [
            std::mem::size_of::<super::NamedDescriptorHeader>(),
            std::mem::offset_of!(crate::abi::UrpAstDescriptor, native_configuration_resolve),
            std::mem::offset_of!(crate::abi::UrpAstDescriptor, native_get_status),
            std::mem::offset_of!(crate::abi::UrpAstDescriptor, native_processing_status_text),
            std::mem::offset_of!(crate::abi::UrpAstDescriptor, link_observer_destroy),
            std::mem::size_of::<crate::abi::UrpAstDescriptor>() - 1,
        ] {
            let mut allocation = vec![0_usize; bytes.div_ceil(std::mem::size_of::<usize>())];
            let pointer = std::ptr::NonNull::new(allocation.as_mut_ptr().cast::<c_void>()).unwrap();
            // SAFETY: the aligned allocation contains the complete named header,
            // but deliberately does not promise a readable full SDK descriptor.
            unsafe {
                pointer
                    .cast::<super::NamedDescriptorHeader>()
                    .as_ptr()
                    .write(super::NamedDescriptorHeader {
                        struct_size: bytes as u32,
                        abi_version: 1,
                        capability: c"usbradioplus.product1".as_ptr(),
                    });
            }
            let providers = super::ProviderSet {
                _libraries: Default::default(),
                iax: std::sync::OnceLock::new(),
                descriptors: vec![super::LoadedProvider {
                    library: NAME,
                    size: bytes,
                    descriptor: pointer,
                }],
            };
            assert_eq!(
                providers.native_product_descriptor().err(),
                Some(ProviderError::IncompleteDescriptor(NAME))
            );
        }
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
        let capability = b"rptadv.prod8\0\0\0\0";
        let descriptor = InlineDescriptor {
            struct_size: std::mem::size_of::<InlineDescriptor>() as u32,
            abi_version: 8,
            capability: *capability,
        };
        let provider = ProviderSpec {
            library: "test",
            symbol: "test",
            abi_version: 8,
            capability: b"rptadv.prod8",
            layout: DescriptorLayout::Inline,
            location: super::LibraryLocation::Private,
        };

        assert!(valid_descriptor(
            (&descriptor as *const InlineDescriptor).cast::<c_void>(),
            provider
        ));
        let mut incompatible = descriptor;
        incompatible.abi_version = 7;
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
        let descriptor = providers.descriptor("librptadviax2.so.2").unwrap();
        // SAFETY: `descriptor` is borrowed from the live set and the fixture exposes its own ABI.
        assert!(!unsafe { descriptor.as_ptr() }.is_null());
        assert!(providers.descriptor("not-installed.so").is_none());
    }

    #[test]
    fn managed_inspection_rejects_missing_truncated_and_incomplete_tables() {
        #[repr(C, align(8))]
        struct Prefix(u32);
        let prefix = Prefix(size_of::<u32>() as u32);
        // SAFETY: the advertised prefix contains exactly its aligned size word.
        assert!(unsafe { super::inspection_table(std::ptr::from_ref(&prefix).cast()) }.is_err());
        // SAFETY: a missing getter has no readable table at all.
        assert!(unsafe { super::inspection_table(std::ptr::null()) }.is_err());
        let incomplete = crate::abi::rptadv_product_inspection_v1 {
            struct_size: size_of::<crate::abi::rptadv_product_inspection_v1>() as u32,
            abi_version: 1,
            capability: *b"rptadv.inspect1\0",
            inspect: None,
        };
        // SAFETY: all fields of the complete negative fixture remain readable.
        assert!(unsafe { super::inspection_table(&incomplete) }.is_err());
    }

    #[test]
    fn management_provider_rejects_missing_and_prefix_only_getters() {
        use crate::abi::rptadv_product_management_v1 as Table;

        #[repr(C, align(8))]
        struct Prefix(u32);
        let size_word = Prefix(size_of::<u32>() as u32);
        // SAFETY: the getter's readable length prefix is its entire allocation.
        assert!(unsafe { super::management_table(std::ptr::from_ref(&size_word).cast()) }.is_err());
        // SAFETY: a missing optional getter result lends no bytes.
        assert!(unsafe { super::management_table(std::ptr::null()) }.is_err());

        // SAFETY: scalar fields and nullable function pointers admit the zero representation.
        let mut api: Table = unsafe { std::mem::zeroed() };
        api.struct_size = size_of::<Table>() as u32;
        api.abi_version = 1;
        api.capability = *b"rptadv.mgmt1\0\0\0\0";
        // SAFETY: the complete local table is readable; its empty operation must be rejected.
        assert!(unsafe { super::management_table(&api) }.is_err());
    }

    #[test]
    fn runtime_product_validation_rejects_missing_lifecycle_operations() {
        let mut descriptor: crate::abi::rptadv_product_descriptor_v1 =
            unsafe { std::mem::zeroed() };
        descriptor.struct_size = std::mem::size_of_val(&descriptor) as u32;
        descriptor.abi_version = 8;
        descriptor.capability = *b"rptadv.prod8\0\0\0\0";
        assert!(!super::product_functions_complete(&descriptor));
    }

    #[cfg(unix)]
    #[test]
    fn product_runtime_descriptors_reject_incomplete_product_tables() {
        let directory = ProviderFixture::new(8, false, false);
        let providers = Box::leak(Box::new(
            super::ProviderSet::load_from_directory(directory.path()).unwrap(),
        ));
        assert_eq!(
            providers.product_runtime_descriptors().err(),
            Some(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.3"
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
        descriptor.abi_version = 8;
        descriptor.capability = *b"rptadv.prod8\0\0\0\0";
        replace_descriptor(&mut providers, "librptadv_product.so.3", descriptor);
        let providers = Box::leak(Box::new(providers));

        assert_eq!(
            providers.product_runtime_descriptors().err(),
            Some(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.3"
            ))
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_set_loads_only_matching_native_descriptors() {
        let directory = ProviderFixture::new(8, false, false);
        let providers = super::ProviderSet::load_from_directory(directory.path()).unwrap();
        assert!(providers.descriptor("librptadv_product.so.3").is_some());
        assert!(providers.descriptor("librptadviax2.so.2").is_some());
        assert!(
            providers
                .descriptor("libusbradioplus_product.so.1")
                .is_some()
        );
        assert!(providers.descriptor("missing-provider.so").is_none());
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.3"
            ))
        );

        let missing = ProviderFixture::new(8, true, false);
        assert_eq!(
            super::ProviderSet::load_from_directory(missing.path()).err(),
            Some(ProviderError::MissingDescriptor("librptadv_product.so.3"))
        );

        let incompatible = ProviderFixture::new(7, false, false);
        assert_eq!(
            super::ProviderSet::load_from_directory(incompatible.path()).err(),
            Some(ProviderError::IncompatibleDescriptor(
                "librptadv_product.so.3"
            ))
        );

        let null = ProviderFixture::new(8, false, true);
        assert_eq!(
            super::ProviderSet::load_from_directory(null.path()).err(),
            Some(ProviderError::MissingDescriptor("librptadv_product.so.3"))
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
        assert!(product.product.reload_working.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn runtime_function_tables_reject_each_missing_operation() {
        let providers = super::ProviderSet::load().unwrap();
        let mut product = *providers
            .typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>("librptadv_product.so.3")
            .unwrap();
        assert!(super::product_functions_complete(&product));
        missing_fields!(product, super::product_functions_complete, reload_working reload_poll stop authorize_incoming incoming link_command link_status digit site_command);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_consumers_reject_header_only_control_file_and_speech_tables() {
        for (index, library) in [
            (1, "librptadv_control_standalone_adapter.so.1"),
            (2, "librptadv_file_adapter.so.2"),
            (3, "librptadv_speech_adapter.so.2"),
        ] {
            let mut providers = super::ProviderSet::load().unwrap();
            providers.validate_runtime().unwrap();
            if index == 1 {
                let prefix = ControlDescriptor {
                    abi_version: 1,
                    struct_size: std::mem::size_of::<ControlDescriptor>(),
                    capability: c"rptadv.control".as_ptr(),
                };
                assert!(super::valid_descriptor(
                    std::ptr::from_ref(&prefix).cast(),
                    super::PROVIDERS[index],
                ));
                replace_descriptor(&mut providers, library, prefix);
            } else {
                let mut capability = [0; 16];
                let expected = super::PROVIDERS[index].capability;
                capability[..expected.len()].copy_from_slice(expected);
                let prefix = InlineDescriptor {
                    struct_size: std::mem::size_of::<InlineDescriptor>() as u32,
                    abi_version: 3,
                    capability,
                };
                assert!(super::valid_descriptor(
                    std::ptr::from_ref(&prefix).cast(),
                    super::PROVIDERS[index],
                ));
                replace_descriptor(&mut providers, library, prefix);
            }
            let providers = Box::leak(Box::new(providers));
            for result in [
                providers.validate_runtime().err(),
                providers.product_runtime_descriptors().err(),
            ] {
                assert_eq!(result, Some(ProviderError::IncompleteDescriptor(library)));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn runtime_validation_reports_each_incomplete_provider_table() {
        let mut providers = super::ProviderSet::load().unwrap();
        let valid_product = *providers
            .typed_descriptor::<crate::abi::rptadv_product_descriptor_v1>("librptadv_product.so.3")
            .unwrap();
        let valid_control = *providers
            .typed_descriptor::<crate::abi::rptadv_control_descriptor_v1>(
                "librptadv_control_standalone_adapter.so.1",
            )
            .unwrap();
        let valid_file = *providers
            .typed_descriptor::<crate::abi::rptadv_file_descriptor>("librptadv_file_adapter.so.2")
            .unwrap();
        let valid_speech = *providers
            .typed_descriptor::<crate::abi::rptadv_speech_descriptor>(
                "librptadv_speech_adapter.so.2",
            )
            .unwrap();
        let valid_native = *providers
            .typed_descriptor::<crate::abi::UrpAstDescriptor>("libusbradioplus_product.so.1")
            .unwrap();

        let mut incomplete = valid_product;
        incomplete.reload_working = None;
        replace_descriptor(&mut providers, "librptadv_product.so.3", incomplete);
        assert_eq!(
            providers.validate_runtime(),
            Err(ProviderError::IncompleteDescriptor(
                "librptadv_product.so.3"
            ))
        );
        replace_descriptor(&mut providers, "librptadv_product.so.3", valid_product);

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

        let file_library = "librptadv_file_adapter.so.2";
        for operation in 0..5 {
            let mut descriptor = valid_file;
            match operation {
                0 => descriptor.create = None,
                1 => descriptor.destroy = None,
                2 => descriptor.open_file = None,
                3 => descriptor.step_stream = None,
                _ => descriptor.close_stream = None,
            }
            replace_descriptor(&mut providers, file_library, descriptor);
            assert_eq!(
                providers.validate_runtime(),
                Err(ProviderError::IncompleteDescriptor(file_library))
            );
        }
        replace_descriptor(&mut providers, file_library, valid_file);

        let speech_library = "librptadv_speech_adapter.so.2";
        for operation in 0..5 {
            let mut descriptor = valid_speech;
            match operation {
                0 => descriptor.create = None,
                1 => descriptor.destroy = None,
                2 => descriptor.open_speech = None,
                3 => descriptor.step_stream = None,
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

        let iax_library = "librptadviax2.so.2";
        let truncated = super::InlineDescriptorHeader {
            struct_size: std::mem::size_of::<super::InlineDescriptorHeader>() as u32,
            abi_version: 2,
            capability: *b"rptadv.iax2.v2\0\0",
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
            compile.arg(format!(
                "-DNATIVE_TABLE_BYTES={}",
                std::mem::size_of::<crate::abi::UrpAstDescriptor>()
            ));
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
            #define PRODUCT_VERSION 8
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
            INLINE(product, rptadv_product_descriptor_v1, PRODUCT_VERSION, "rptadv.prod8")
            #endif
            static Control control = { 1, sizeof(Control), "rptadv.control" };
            void *rptadv_control_standalone_descriptor_v1(void) { return &control; }
            INLINE(file, rptadv_file_adapter_descriptor, 3, "rptadv.file")
            INLINE(speech, rptadv_speech_adapter_descriptor, 3, "rptadv.speech")
            INLINE(resolver, rptadv_resolver_adapter_descriptor_v1, 1, "rptadv.resolver")
            INLINE(jitter, rptadv_jitter_adapter_descriptor_v1, 1, "rptadv.jitter")
            INLINE(iax2, rptadv_iax2_client_descriptor_v2, 2, "rptadv.iax2.v2")
            #ifndef NATIVE_VERSION
            #define NATIVE_VERSION 1
            #endif
            #ifndef NATIVE_SIZE
            #define NATIVE_SIZE sizeof(Named)
            #endif
            #ifndef NATIVE_TABLE_BYTES
            #define NATIVE_TABLE_BYTES sizeof(Named)
            #endif
            static struct { Named header; uint8_t tail[NATIVE_TABLE_BYTES]; } native = {
                { NATIVE_SIZE, NATIVE_VERSION, "usbradioplus.product1" }, {0}
            };
            void *usbradioplus_product_descriptor_v1(void) { return &native; }
            NAMED(radio, rptadv_radio_descriptor, 5, "rptadv.radio-core")
            NAMED(audio, rptadv_portaudio_alsa_adapter_descriptor, 2, "rptadv.portaudio-alsa-audio")
            NAMED(ffmpeg, rptadv_ffmpeg_adapter_descriptor, 1, "rptadv.ffmpeg")
            NAMED(gpio, rptadv_gpio_adapter_descriptor, 1, "rptadv.cm119-hid-gpio")
            NAMED(rnnoise, rptadv_rnnoise_adapter_descriptor, 1, "rptadv.rnnoise")
            NAMED(recurrence, rptadv_recurrence_descriptor, 1, "rptadv.recurrence")
        "#
    }
}
