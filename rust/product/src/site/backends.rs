//! Selected site providers are retained outside all audio and device callbacks.
use crate::abi::{rptadv_gpiod_descriptor_v1, rptadv_hwmon_descriptor_v1};
use libloading::Library;
use rpt_advanced_core::{
    config::{ConfigDocument, NodeId, ResolvedNodeSettings, ResolvedSiteSettings, SiteBackend},
    runtime::RuntimeError,
};
use std::{collections::BTreeSet, mem::size_of, path::Path};

#[path = "endpoint.rs"]
pub mod endpoint;

/// Complete immutable function table plus the executable code it borrows.
struct Provider<T> {
    table: T,
    _library: Library,
}

impl<T: Copy> Provider<T> {
    /// Load a trusted, fixed ABI symbol and inspect size before reading its tail.
    unsafe fn open(directory: &Path, name: &str) -> Result<Self, RuntimeError> {
        let library =
            unsafe { Library::new(directory.join(format!("librptadv_{name}_adapter.so.1"))) }
                .map_err(|_| RuntimeError::Preparation)?;
        let symbol = unsafe {
            library.get::<unsafe extern "C" fn() -> *const T>(
                format!("rptadv_{name}_adapter_descriptor_v1").as_bytes(),
            )
        }
        .map_err(|_| RuntimeError::Preparation)?;
        let pointer = unsafe { symbol() };
        // Both selected descriptors start with a readable u32 size. Do not
        // dereference any later field until the complete record is available.
        if pointer.is_null() || unsafe { pointer.cast::<u32>().read() } as usize != size_of::<T>() {
            return Err(RuntimeError::Preparation);
        }
        Ok(Self {
            table: unsafe { pointer.read() },
            _library: library,
        })
    }
}

/// Both capability contracts require their entire table, not a usable subset.
macro_rules! complete {
    ($table:expr, $name:literal) => {{
        let table = &$table;
        table.abi_version == 1
            && table.capability == *$name
            && table.open.is_some()
            && table.read.is_some()
            && table.write.is_some()
            && table.capabilities.is_some()
            && table.destroy.is_some()
            && table.set_safe.is_some()
    }};
}

/// Site capability composition, retained until every endpoint has retired.
pub struct SiteBackends {
    gpiod: Option<Provider<rptadv_gpiod_descriptor_v1>>,
    hwmon: Option<Provider<rptadv_hwmon_descriptor_v1>>,
}
impl SiteBackends {
    /// Load only capabilities used by enabled site definitions.
    pub fn load(document: &ConfigDocument) -> Result<Self, RuntimeError> {
        Self::load_at(document, Path::new(env!("RPT_ADVANCED_LIBDIR")))
    }
    fn load_at(document: &ConfigDocument, directory: &Path) -> Result<Self, RuntimeError> {
        let selected = required(document)?;
        let providers = Self {
            gpiod: if selected.contains(&SiteBackend::Gpiod) {
                Some(unsafe { Provider::open(directory, "gpiod")? })
            } else {
                None
            },
            hwmon: if selected.contains(&SiteBackend::Hwmon) {
                Some(unsafe { Provider::open(directory, "hwmon")? })
            } else {
                None
            },
        };
        providers.validate(document)?;
        Ok(providers)
    }
    /// Reject reloads requiring a provider not retained by this composition.
    pub fn validate(&self, document: &ConfigDocument) -> Result<(), RuntimeError> {
        for backend in required(document)? {
            let valid = match backend {
                SiteBackend::Gpiod => self
                    .gpiod
                    .as_ref()
                    .is_some_and(|provider| complete!(provider.table, b"rptadv.gpiod\0\0\0\0")),
                SiteBackend::Hwmon => self
                    .hwmon
                    .as_ref()
                    .is_some_and(|provider| complete!(provider.table, b"rptadv.hwmon\0\0\0\0")),
                // IIO is filesystem I/O; CM119/parallel reuse the retained radio owner.
                _ => true,
            };
            if !valid {
                return Err(RuntimeError::Preparation);
            }
        }
        Ok(())
    }
}

/// Defaults do not instantiate devices, and disabled nodes select no providers.
fn required(document: &ConfigDocument) -> Result<BTreeSet<SiteBackend>, RuntimeError> {
    let mut selected = BTreeSet::new();
    for name in document.nodes() {
        let node = NodeId::new(name)?;
        if !ResolvedNodeSettings::resolve(document, &node)?
            .value
            .enabled
        {
            continue;
        }
        let settings = ResolvedSiteSettings::resolve(document, &node)?.value;
        for endpoint in settings
            .sensors
            .iter()
            .filter(|v| v.enabled)
            .filter_map(|v| v.endpoint.as_ref())
            .chain(
                settings
                    .inputs
                    .iter()
                    .filter(|v| v.enabled)
                    .filter_map(|v| v.endpoint.as_ref()),
            )
            .chain(
                settings
                    .outputs
                    .iter()
                    .filter(|v| v.enabled)
                    .filter_map(|v| v.endpoint.as_ref()),
            )
        {
            selected.insert(endpoint.backend);
        }
    }
    Ok(selected)
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
