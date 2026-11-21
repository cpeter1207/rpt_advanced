//! Selected site providers are retained outside all audio and device callbacks.
use crate::abi::{rptadv_gpiod_descriptor_v1, rptadv_hwmon_descriptor_v1};
use libloading::Library;
use rpt_advanced_core::{
    config::{ConfigDocument, ResolvedSiteSettings, SiteBackend},
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
        Self::load_scopes(crate::site::configured(document)?.values())
    }
    pub(crate) fn load_scopes<'a>(
        scopes: impl IntoIterator<Item = &'a ResolvedSiteSettings>,
    ) -> Result<Self, RuntimeError> {
        Self::load_scopes_at(scopes, Path::new(env!("RPT_ADVANCED_LIBDIR")))
    }
    #[cfg(test)]
    fn load_at(document: &ConfigDocument, directory: &Path) -> Result<Self, RuntimeError> {
        Self::load_scopes_at(crate::site::configured(document)?.values(), directory)
    }
    fn load_scopes_at<'a>(
        scopes: impl IntoIterator<Item = &'a ResolvedSiteSettings>,
        directory: &Path,
    ) -> Result<Self, RuntimeError> {
        let selected = required(scopes);
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
        providers.validate_required(selected)?;
        Ok(providers)
    }
    /// Reject reloads requiring a provider not retained by this composition.
    pub fn validate(&self, document: &ConfigDocument) -> Result<(), RuntimeError> {
        self.validate_scopes(crate::site::configured(document)?.values())
    }
    pub(crate) fn validate_scopes<'a>(
        &self,
        scopes: impl IntoIterator<Item = &'a ResolvedSiteSettings>,
    ) -> Result<(), RuntimeError> {
        self.validate_required(required(scopes))
    }
    fn validate_required(&self, selected: BTreeSet<SiteBackend>) -> Result<(), RuntimeError> {
        for backend in selected {
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
fn required<'a>(
    scopes: impl IntoIterator<Item = &'a ResolvedSiteSettings>,
) -> BTreeSet<SiteBackend> {
    let mut selected = BTreeSet::new();
    for settings in scopes {
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
    selected
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
