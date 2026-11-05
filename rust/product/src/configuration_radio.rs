//! Side-effect-free radio inspection through the existing dynamically linked C SDK.

use crate::abi;
use std::{
    ffi::{CStr, c_void},
    mem::{offset_of, size_of},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{self, NonNull},
};

type Resolve = unsafe extern "C" fn(
    *const abi::UrpNativeProcessingArgs,
    abi::UrpNativeProcessingDiagnostic,
    *mut c_void,
    *mut *mut c_void,
) -> i32;
type Visit =
    unsafe extern "C" fn(*const c_void, abi::UrpNativeConfigurationSink, *mut c_void) -> i32;
type Destroy = unsafe extern "C" fn(*mut c_void);
type RemoteOptions = unsafe extern "C" fn(*mut c_void, *mut abi::UrpRemoteCommandOptions) -> i32;

#[derive(Clone, Copy)]
struct Api {
    resolve: Resolve,
    visit: Visit,
    destroy: Destroy,
    remote_options: RemoteOptions,
}

impl Api {
    unsafe fn from_descriptor(descriptor: *const abi::UrpAstDescriptor) -> Result<Self, String> {
        if descriptor.is_null() {
            return Err("shared radio configuration provider is unavailable".into());
        }
        // SAFETY: every descriptor exposes its size word; no other field is read yet.
        let advertised_size = unsafe { descriptor.cast::<u32>().read_unaligned() };
        let required = offset_of!(abi::UrpAstDescriptor, native_configuration_remote_options)
            + size_of::<Option<RemoteOptions>>();
        if advertised_size < required as u32 {
            return Err(
                "shared radio configuration provider has an incompatible descriptor".into(),
            );
        }
        // SAFETY: the admitted prefix covers the ABI word before all required operations.
        if unsafe { descriptor.cast::<u32>().add(1).read_unaligned() } != 1 {
            return Err(
                "shared radio configuration provider has an incompatible descriptor".into(),
            );
        }
        // SAFETY: the admitted descriptor prefix includes its capability-name pointer.
        let capability = unsafe { ptr::addr_of!((*descriptor).capability_name).read_unaligned() };
        if capability.is_null()
            // SAFETY: non-null provider capability names are static NUL-terminated strings.
            || unsafe { CStr::from_ptr(capability) }.to_bytes() != b"usbradioplus.product1"
        {
            return Err(
                "shared radio configuration provider has an incompatible descriptor".into(),
            );
        }
        // SAFETY: advertised size covers these three fields; do not borrow the longer current table.
        let resolve =
            unsafe { ptr::addr_of!((*descriptor).native_configuration_resolve).read_unaligned() };
        // SAFETY: the same checked descriptor tail contains the visit operation.
        let visit =
            unsafe { ptr::addr_of!((*descriptor).native_configuration_visit).read_unaligned() };
        // SAFETY: the size requirement ends after this checked destroy operation.
        let destroy =
            unsafe { ptr::addr_of!((*descriptor).native_configuration_destroy).read_unaligned() };
        // SAFETY: the required prefix ends after this configuration-only operation.
        let remote_options = unsafe {
            ptr::addr_of!((*descriptor).native_configuration_remote_options).read_unaligned()
        };
        Ok(Self {
            resolve: resolve.ok_or("shared radio configuration resolution is unavailable")?,
            visit: visit.ok_or("shared radio configuration visit is unavailable")?,
            destroy: destroy.ok_or("shared radio configuration destruction is unavailable")?,
            remote_options: remote_options
                .ok_or("shared radio command configuration is unavailable")?,
        })
    }
}

/// One SDK-owned radio request; every borrowed visit ends before destruction.
pub(crate) struct ResolvedRadio {
    owner: NonNull<c_void>,
    api: Api,
}

impl ResolvedRadio {
    /// Copy canonical DTMF policy without acquiring or opening a device.
    pub(crate) fn remote_commands(
        &self,
    ) -> Result<Option<rpt_advanced_core::remote_base_command::RemoteCommandConfiguration>, String>
    {
        use rpt_advanced_core::remote_base_command::{
            RemoteCommandConfiguration, RemoteDtmfAccess,
        };
        // SAFETY: this complete scalar/byte record has no invalid bit patterns.
        let mut output: abi::UrpRemoteCommandOptions = unsafe { std::mem::zeroed() };
        output.struct_size = size_of::<abi::UrpRemoteCommandOptions>() as u32;
        output.abi_version = 1;
        // SAFETY: the unique SDK owner remains live and the output is complete/writable.
        let status = unsafe { (self.api.remote_options)(self.owner.as_ptr(), &mut output) };
        if status != 0 {
            return Err(format!(
                "shared radio command configuration failed ({status})"
            ));
        }
        if output.enabled == 0 {
            return Ok(None);
        }
        let access = match output.access {
            0 => RemoteDtmfAccess::Disabled,
            1 => RemoteDtmfAccess::Open,
            2 => RemoteDtmfAccess::Locked,
            _ => return Err("shared radio command access is invalid".into()),
        };
        let bytes = output
            .prefix
            .get(..output.prefix_length as usize)
            .filter(|prefix| {
                !prefix.is_empty() && prefix.len() <= 63 && prefix.iter().all(u8::is_ascii_digit)
            })
            .ok_or("shared radio command prefix is invalid")?;
        Ok(Some(RemoteCommandConfiguration {
            prefix: String::from_utf8(bytes.to_vec()).expect("validated ASCII digits"),
            access,
        }))
    }
    pub(crate) fn resolve(
        configuration: &str,
        node: &str,
        diagnostic: abi::UrpNativeProcessingDiagnostic,
        context: *mut c_void,
    ) -> Result<Self, String> {
        // SAFETY: the linked SDK returns an immutable process-lifetime descriptor.
        let api = unsafe { Api::from_descriptor(abi::usbradioplus_product_descriptor_v1()) }?;
        let args = abi::UrpNativeProcessingArgs {
            struct_size: size_of::<abi::UrpNativeProcessingArgs>() as u32,
            abi_version: 1,
            configuration: configuration.as_ptr(),
            configuration_length: u32::try_from(configuration.len())
                .map_err(|_| "radio configuration is too long")?,
            node: node.as_ptr(),
            node_length: u32::try_from(node.len())
                .map_err(|_| "radio node identity is too long")?,
        };
        let mut owner = ptr::null_mut();
        // SAFETY: source spans and diagnostic context remain live for this synchronous operation.
        let status = unsafe { (api.resolve)(&args, diagnostic, context, &mut owner) };
        if status != 0 {
            return Err(format!(
                "shared radio configuration failed for node {node} ({status})"
            ));
        }
        Ok(Self {
            owner: NonNull::new(owner).ok_or("shared radio configuration returned no owner")?,
            api,
        })
    }

    pub(super) fn visit<F: FnMut(*const abi::UrpNativeStationConfig) -> Result<(), String>>(
        &self,
        callback: F,
    ) -> Result<(), String> {
        struct Context<F> {
            callback: F,
            error: Option<String>,
        }
        unsafe extern "C" fn sink<
            F: FnMut(*const abi::UrpNativeStationConfig) -> Result<(), String>,
        >(
            context: *mut c_void,
            request: *const abi::UrpNativeStationConfig,
        ) -> i32 {
            // SAFETY: visit retains this exact stack context for the synchronous callback.
            let context = unsafe { &mut *context.cast::<Context<F>>() };
            let result = if request.is_null() {
                Err("shared radio configuration returned no request".into())
            } else {
                catch_unwind(AssertUnwindSafe(|| (context.callback)(request)))
                    .unwrap_or_else(|_| Err("configuration inspection failed".into()))
            };
            match result {
                Ok(()) => 0,
                Err(error) => {
                    context.error = Some(error);
                    -1
                }
            }
        }
        let mut context = Context {
            callback,
            error: None,
        };
        // SAFETY: the SDK owner and callback context remain live until visit returns.
        let status = unsafe {
            (self.api.visit)(
                self.owner.as_ptr(),
                Some(sink::<F>),
                ptr::from_mut(&mut context).cast(),
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(context
                .error
                .unwrap_or_else(|| format!("shared radio configuration visit failed ({status})")))
        }
    }
}

impl Drop for ResolvedRadio {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner produced by this exact SDK instance.
        unsafe { (self.api.destroy)(self.owner.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_remote_command_options_are_copied_without_opening_hardware() {
        for (source, expected) in [
            ("[524950]\n", None),
            (
                "[524950]\n[remote_base 524950]\nenabled=yes\nbackend=hamlib\nhamlib_model=RIG_MODEL_DUMMY\ndtmf_prefix=65\ndtmf_access=open\nstartup_memory=local\nallowed_modes=FM\nallowed_rx_ranges_hz=144000000-148000000\nallowed_tx_ranges_hz=144000000-148000000\n[remote_memory 524950 local]\nselector=1\nrx_frequency_hz=146520000\ntx_frequency_hz=146520000\nmode=FM\n",
                Some((
                    "65",
                    rpt_advanced_core::remote_base_command::RemoteDtmfAccess::Open,
                )),
            ),
        ] {
            let owner = ResolvedRadio::resolve(source, "524950", None, ptr::null_mut()).unwrap();
            assert_eq!(
                owner
                    .remote_commands()
                    .unwrap()
                    .map(|value| (value.prefix, value.access)),
                expected.map(|(prefix, access)| (prefix.to_string(), access))
            );
        }
    }

    #[test]
    fn short_and_wrong_abi_descriptors_are_rejected_before_reading_tails() {
        let size_only = 4_u32;
        // SAFETY: a rejected size-only prefix must not require a readable ABI word.
        assert!(unsafe { Api::from_descriptor(ptr::from_ref(&size_only).cast()) }.is_err());
        for header in [[8_u32, 1], [u32::MAX, 2]] {
            // SAFETY: only the readable prefix may be read before these rejections.
            assert!(unsafe { Api::from_descriptor(ptr::from_ref(&header).cast()) }.is_err());
        }
        // SAFETY: null is explicitly admitted and rejected by the loader.
        assert!(unsafe { Api::from_descriptor(ptr::null()) }.is_err());
    }

    unsafe extern "C" fn resolve_unavailable(
        _: *const abi::UrpNativeProcessingArgs,
        _: abi::UrpNativeProcessingDiagnostic,
        _: *mut c_void,
        _: *mut *mut c_void,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn visit_unavailable(
        _: *const c_void,
        _: abi::UrpNativeConfigurationSink,
        _: *mut c_void,
    ) -> i32 {
        -1
    }
    unsafe extern "C" fn destroy_noop(_: *mut c_void) {}
    unsafe extern "C" fn remote_options_unavailable(
        _: *mut c_void,
        _: *mut abi::UrpRemoteCommandOptions,
    ) -> i32 {
        -1
    }

    unsafe extern "C" fn visit_request(
        _: *const c_void,
        sink: abi::UrpNativeConfigurationSink,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: this fixture contains only scalars and nullable C pointers; tests never dereference its spans.
        let request: abi::UrpNativeStationConfig = unsafe { std::mem::zeroed() };
        // SAFETY: visit retains the callback/context, and the request lives through this synchronous call.
        unsafe { sink.unwrap()(context, &request) }
    }

    #[test]
    fn advertised_tail_requires_all_operations_and_accepts_its_exact_prefix() {
        // SAFETY: this C table contains only scalar fields and nullable function/data pointers.
        let mut descriptor: abi::UrpAstDescriptor = unsafe { std::mem::zeroed() };
        descriptor.struct_size = (offset_of!(abi::UrpAstDescriptor, native_configuration_destroy)
            + size_of::<Option<Destroy>>()) as u32;
        descriptor.abi_version = 1;
        descriptor.capability_name = c"usbradioplus.product1".as_ptr();
        // SAFETY: this test retains the complete readable descriptor, including its checked prefix.
        let load = |descriptor: &abi::UrpAstDescriptor| unsafe { Api::from_descriptor(descriptor) };
        assert!(load(&descriptor).err().unwrap().contains("resolution"));
        descriptor.native_configuration_resolve = Some(resolve_unavailable);
        assert!(load(&descriptor).err().unwrap().contains("visit"));
        descriptor.native_configuration_remote_options = Some(remote_options_unavailable);
        descriptor.native_configuration_visit = Some(visit_unavailable);
        assert!(load(&descriptor).err().unwrap().contains("destruction"));
        descriptor.native_configuration_destroy = Some(destroy_noop);
        assert!(load(&descriptor).is_ok());
        for capability in [c"wrong.product".as_ptr(), ptr::null()] {
            descriptor.capability_name = capability;
            assert!(
                load(&descriptor).is_err(),
                "configuration admission accepted a wrong or null SDK capability"
            );
        }
    }

    #[test]
    fn provider_visit_failures_and_callback_panics_stay_inside_the_c_boundary() {
        let mut resolved = ResolvedRadio {
            owner: NonNull::dangling(),
            api: Api {
                resolve: resolve_unavailable,
                visit: visit_unavailable,
                destroy: destroy_noop,
                remote_options: remote_options_unavailable,
            },
        };
        assert!(
            resolved
                .visit(|_| Ok(()))
                .unwrap_err()
                .contains("visit failed")
        );
        unsafe extern "C" fn invoke(
            _: *const c_void,
            sink: abi::UrpNativeConfigurationSink,
            context: *mut c_void,
        ) -> i32 {
            // SAFETY: this fixture deliberately tests a null request at the synchronous SDK boundary.
            unsafe { sink.unwrap()(context, ptr::null()) }
        }
        resolved.api.visit = invoke;
        assert_eq!(
            resolved
                .visit(|_| panic!("null request reached host"))
                .unwrap_err(),
            "shared radio configuration returned no request"
        );
        resolved.api.visit = visit_request;
        assert_eq!(
            resolved
                .visit(|_| Err("visitor rejected request".into()))
                .unwrap_err(),
            "visitor rejected request"
        );
        assert_eq!(
            resolved
                .visit(|_| panic!("private callback panic"))
                .unwrap_err(),
            "configuration inspection failed"
        );
    }

    #[test]
    fn sdk_owners_are_destroyed_once_after_success_and_rejection() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        unsafe extern "C" fn destroy(owner: *mut c_void) {
            // SAFETY: each fixture transfers exactly one boxed counter into the opaque owner.
            let destroyed = unsafe { Box::from_raw(owner.cast::<Arc<AtomicUsize>>()) };
            destroyed.fetch_add(1, Ordering::Relaxed);
        }
        let destroyed = Arc::new(AtomicUsize::new(0));
        for rejected in [false, true] {
            let resolved = ResolvedRadio {
                owner: NonNull::new(Box::into_raw(Box::new(destroyed.clone())).cast()).unwrap(),
                api: Api {
                    resolve: resolve_unavailable,
                    visit: visit_request,
                    destroy,
                    remote_options: remote_options_unavailable,
                },
            };
            assert_eq!(
                resolved
                    .visit(|_| if rejected {
                        Err("rejected".into())
                    } else {
                        Ok(())
                    })
                    .is_err(),
                rejected
            );
            drop(resolved);
            assert_eq!(destroyed.load(Ordering::Relaxed), usize::from(rejected) + 1);
        }
    }
}
