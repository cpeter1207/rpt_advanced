//! Hardware-free configuration and peer preparation from the matched USBRadioPlus product DSO.
mod lifecycle;
#[path = "../configuration_radio.rs"]
mod resolved;
pub use lifecycle::{NativeBindings, NativeStation, PreparedUpdate, UpdateStatus};
mod policy;
pub use policy::ControllerRadioPolicy;
mod peer;
mod station;
mod template;
use crate::abi;
use libloading::Library;
pub use peer::{PreparedPeer, PreparedRole};
pub use station::{PreparedStation, StationInspection};
use std::{
    ffi::{CStr, OsStr, c_char, c_void},
    fmt,
    mem::size_of,
    path::Path,
    ptr::{self, NonNull},
    sync::Arc,
};

// The matched header and runtime table must both advertise physical preparation ABI 1.
const _: () = assert!(abi::URP_NATIVE_PHYSICAL_PREPARATION_ABI_VERSION == 1);

/// Value-free preparation or lifecycle diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreparationError {
    /// Required library or entry point is absent.
    Unavailable,
    /// Provider ABI, identity, required operation or returned object is invalid.
    Incompatible,
    /// Requests or owned objects do not belong to the same loaded provider.
    DifferentProvider,
    /// The candidate has a different stable port identity.
    DifferentPort,
    /// A reversible physical update is still pending.
    UpdateInProgress,
    /// No physical update is awaiting acknowledgment.
    NoUpdate,
    /// The released station template cannot acquire this selected signal.
    Unsupported(&'static str),
    /// More settings or samples than the provider ABI accepts.
    Limit,
    /// Provider status and its optional synchronous field identifier.
    Rejected {
        /// Native status.
        status: i32,
        /// Static field token copied while the provider remains loaded.
        field: Option<String>,
    },
}
impl fmt::Display for PreparationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("radio preparation provider unavailable"),
            Self::Incompatible => f.write_str("incompatible radio preparation provider"),
            Self::DifferentProvider => f.write_str("radio owners belong to different providers"),
            Self::Unsupported(field) => write!(f, "radio capability unavailable for {field}"),
            Self::DifferentPort => f.write_str("radio candidate belongs to another port"),
            Self::UpdateInProgress => f.write_str("radio update already pending"),
            Self::NoUpdate => f.write_str("no radio update pending"),
            Self::Limit => f.write_str("radio preparation bound exceeded"),
            Self::Rejected {
                status,
                field: Some(name),
            } => write!(f, "radio provider rejected {name} ({status})"),
            Self::Rejected {
                status,
                field: None,
            } => write!(f, "radio provider returned status {status}"),
        }
    }
}
impl std::error::Error for PreparationError {}
type Result<T> = std::result::Result<T, PreparationError>;

#[repr(C)]
struct Header {
    size: u32,
    version: u32,
    name: *const c_char,
}
/// Load a known getter from a retained, trusted library. Check common prefix before table access.
unsafe fn named<T>(library: &Library, getter: &[u8], name: &[u8]) -> Result<NonNull<T>> {
    // SAFETY: the caller supplies the known getter and C table from the matching SDK.
    let pointer = unsafe {
        library
            .get::<unsafe extern "C" fn() -> *mut T>(getter)
            .map_err(|_| PreparationError::Unavailable)?()
    };
    let pointer = NonNull::new(pointer).ok_or(PreparationError::Incompatible)?;
    if !pointer.as_ptr().is_aligned() {
        return Err(PreparationError::Incompatible);
    }
    let head = pointer.cast::<Header>().as_ptr();
    // SAFETY: all provider descriptors expose size as the first uint32_t. Other fields
    // are inspected only once that size covers the entire expected descriptor.
    if unsafe { ptr::addr_of!((*head).size).read_unaligned() } < size_of::<T>() as u32 {
        return Err(PreparationError::Incompatible);
    }
    // SAFETY: these fields are part of the admitted immutable prefix.
    let (version, identity) = unsafe {
        (
            ptr::addr_of!((*head).version).read_unaligned(),
            ptr::addr_of!((*head).name).read_unaligned(),
        )
    };
    if version != 1 || identity.is_null() || unsafe { CStr::from_ptr(identity) }.to_bytes() != name
    {
        return Err(PreparationError::Incompatible);
    }
    Ok(pointer)
}
pub(super) fn succeeded(status: i32) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(PreparationError::Rejected {
            status,
            field: None,
        })
    }
}
fn equivalent(status: i32, flag: u32) -> Result<bool> {
    succeeded(status)?;
    match flag {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(PreparationError::Incompatible),
    }
}
fn owned<T>(status: i32, pointer: *mut T, field: Option<String>) -> Result<NonNull<T>> {
    if status != 0 {
        return Err(PreparationError::Rejected { status, field });
    }
    NonNull::new(pointer).ok_or(PreparationError::Incompatible)
}
unsafe extern "C" fn diagnostic(context: *mut c_void, bytes: *const u8, count: u32) {
    if context.is_null() || bytes.is_null() || count == 0 || count > 96 {
        return;
    }
    // SAFETY: preparation lends one static ASCII field and our Option<String> for this call.
    let s = unsafe { std::slice::from_raw_parts(bytes, count as usize) };
    if s.iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
    {
        if let Ok(s) = std::str::from_utf8(s) {
            unsafe { *context.cast::<Option<String>>() = Some(s.to_owned()) }
        }
    }
}
struct Product {
    product: NonNull<abi::UrpAstDescriptor>,
    station: NonNull<abi::UrpPreparationDescriptor>,
    link: NonNull<abi::UrpLinkPreparationDescriptor>,
    _library: Library,
    #[cfg(test)]
    _fixture_descriptor: Option<Box<abi::UrpAstDescriptor>>,
    #[cfg(test)]
    _fixture_roles: Option<Box<abi::UrpLinkPreparationDescriptor>>,
}
// SAFETY: all three admitted tables are immutable for the full lifetime of the retained DSO.
// Mutable native objects are accessed through their individually owned, serialized handles.
unsafe impl Send for Product {}
unsafe impl Sync for Product {}
impl Product {
    fn ordinary(&self) -> &abi::UrpAstDescriptor {
        // SAFETY: the complete immutable descriptor and its DSO outlive this borrow.
        unsafe { self.product.as_ref() }
    }
    fn roles(&self) -> &abi::UrpLinkPreparationDescriptor {
        // SAFETY: the complete immutable descriptor and its DSO outlive this borrow.
        unsafe { self.link.as_ref() }
    }
    fn stations(&self) -> &abi::UrpPreparationDescriptor {
        // SAFETY: the complete immutable descriptor and its DSO outlive this borrow.
        unsafe { self.station.as_ref() }
    }
    fn same(&self, other: &Self) -> Result<()> {
        if self.product == other.product {
            Ok(())
        } else {
            Err(PreparationError::DifferentProvider)
        }
    }
}
/// Control-thread owner of validated product, station-preparation and peer-role capabilities.
#[derive(Clone)]
pub struct RadioPreparationApi {
    product: Arc<Product>,
}
impl RadioPreparationApi {
    /// Load the fixed release SONAME and validate every operation before preparing any object.
    pub fn open() -> Result<Self> {
        // SAFETY: this is the trusted native release SONAME in the normal product loader path.
        unsafe { Self::open_library(OsStr::new("libusbradioplus_product.so.1")) }
    }
    /// Load a selected release from a trusted installation or fixture directory.
    ///
    /// # Safety
    /// The path must identify a trusted product DSO exposing the published C SDK.
    pub unsafe fn open_at(path: &Path) -> Result<Self> {
        // SAFETY: the caller guarantees the library and native API contracts.
        unsafe { Self::open_library(path.as_os_str()) }
    }
    unsafe fn open_library(path: &OsStr) -> Result<Self> {
        // SAFETY: all three immutable getters are loaded from the very same trusted DSO.
        let library = unsafe { Library::new(path) }.map_err(|_| PreparationError::Unavailable)?;
        let (product, station, link) = unsafe {
            (
                named(
                    &library,
                    b"usbradioplus_product_descriptor_v1\0",
                    b"usbradioplus.product1",
                )?,
                named(
                    &library,
                    b"usbradioplus_physical_preparation_descriptor\0",
                    b"usbradioplus.physical-preparation1",
                )?,
                named(
                    &library,
                    b"usbradioplus_link_preparation_descriptor\0",
                    b"usbradioplus.link-preparation1",
                )?,
            )
        };
        let p = Product {
            product,
            station,
            link,
            _library: library,
            #[cfg(test)]
            _fixture_descriptor: None,
            #[cfg(test)]
            _fixture_roles: None,
        };
        let o = p.ordinary();
        let l = p.roles();
        let s = p.stations();
        if [
            o.link_process.is_none(),
            o.link_destroy.is_none(),
            o.link_observe.is_none(),
            o.native_configuration_visit.is_none(),
            o.native_configuration_remote_options.is_none(),
            o.native_configuration_destroy.is_none(),
            o.native_destroy.is_none(),
            o.native_begin_update.is_none(),
            o.native_accept_update.is_none(),
            o.native_finish_update.is_none(),
            o.native_destroy_update.is_none(),
            l.prepare.is_none(),
            l.equal.is_none(),
            l.link_prepare.is_none(),
            l.destroy.is_none(),
            s.prepare.is_none(),
            s.equal.is_none(),
            s.create.is_none(),
            s.prepare_update.is_none(),
            s.link_prepare.is_none(),
        ]
        .contains(&true)
        {
            return Err(PreparationError::Incompatible);
        }
        Ok(Self {
            product: Arc::new(p),
        })
    }
}

/// Build the ordinary typed graph with a retained SDK descriptor containing test callbacks.
///
/// # Safety
/// The substituted callbacks and factory must follow the selected operation's
/// exact ABI and transfer a unique context whose lifetime extends through destroy.
#[cfg(test)]
pub(crate) unsafe fn fixture_native_peer(
    rate: u32,
    configure: impl FnOnce(&mut abi::UrpAstDescriptor),
    prepare: impl FnOnce(*const c_void, *mut *mut c_void) -> i32,
) -> Result<PreparedPeer> {
    let api =
        unsafe { RadioPreparationApi::fixture_descriptors(|ordinary, _| configure(ordinary)) }?;
    PreparedPeer::create(api.product, rate, prepare)
}

#[cfg(test)]
impl RadioPreparationApi {
    /// Keep the descriptor copies and their original DSO alive for all resulting roles and peers.
    ///
    /// # Safety
    /// Replacements must implement the selected SDK operations and handle lifetimes.
    pub(crate) unsafe fn fixture_descriptors(
        configure: impl FnOnce(&mut abi::UrpAstDescriptor, &mut abi::UrpLinkPreparationDescriptor),
    ) -> Result<Self> {
        let mut api = Self::open()?;
        let product = Arc::get_mut(&mut api.product).expect("new provider has one owner");
        let mut ordinary = Box::new(*product.ordinary());
        let mut roles = Box::new(*product.roles());
        configure(&mut ordinary, &mut roles);
        product.product = NonNull::from(ordinary.as_mut());
        product.link = NonNull::from(roles.as_mut());
        product._fixture_descriptor = Some(ordinary);
        product._fixture_roles = Some(roles);
        Ok(api)
    }
}
