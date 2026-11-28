//! Build actual managed graphs and replace only the hardware edge in Product tests.

use crate::{
    abi,
    configuration_radio::RadioPreparationApi,
    management::{PreparedConfiguration, ProfileInputs},
    services::{HostServices, Radio},
};
use rpt_advanced_core::config::resources::{
    PortKey,
    candidate::{
        CapturedResource, ModularCandidate, ResourceBinding, ResourceId, ResourceKind,
        ResourceRegistry, ResourceScope,
    },
};
use serde_json::{Value, json};
use std::{ffi::c_void, fmt::Write as _, num::NonZeroU64, ptr};

pub(crate) const SITE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
pub(crate) const LOCAL: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
pub(crate) const SECOND_LOCAL: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

pub(crate) fn prepare(
    stations: &[(&str, &str, Value)],
    resources: &[(&str, ResourceKind, &str, Value)],
) -> PreparedConfiguration {
    let site = SITE.parse().unwrap();
    let mut rows = vec![(
        ResourceBinding {
            id: site,
            scope: ResourceScope::Site(site),
            kind: ResourceKind::Metadata,
        },
        json!({"id": SITE, "kind": "metadata", "configuration":{"name":"Site","inherit_from":null}}),
    )];
    let mut ports = Vec::new();
    for (id, key, metadata) in stations {
        let owner = id.parse().unwrap();
        ports.push((owner, PortKey::new(*key)));
        let mut fields = metadata.as_object().unwrap().clone();
        fields.entry("name").or_insert_with(|| json!(key));
        fields
            .entry("inherit_from")
            .or_insert_with(|| json!("site"));
        fields.entry("enabled").or_insert_with(|| json!(true));
        rows.push((
            ResourceBinding {
                id: owner,
                scope: ResourceScope::Port(owner),
                kind: ResourceKind::Metadata,
            },
            json!({"id":id,"kind":"metadata","key":key,"configuration":fields}),
        ));
    }
    let mut fixed = Vec::new();
    let mut named = Vec::new();
    for (index, (owner, kind, key, fields)) in resources.iter().enumerate() {
        let owner = owner.parse().unwrap();
        let id = format!("fe000000-0000-4000-8000-{index:012x}");
        let binding = ResourceBinding {
            id: id.parse().unwrap(),
            scope: if owner == site {
                ResourceScope::Site(site)
            } else {
                ResourceScope::Port(owner)
            },
            kind: *kind,
        };
        let mut envelope = if *kind == ResourceKind::Event {
            fields.clone()
        } else {
            json!({"configuration": fields})
        };
        envelope["id"] = json!(id);
        envelope["kind"] = json!(kind.as_str());
        if kind.is_named() {
            named.push((binding, PortKey::new(*key)));
            envelope["key"] = json!(key);
        } else {
            assert!(key.is_empty());
            fixed.push(binding);
        }
        rows.push((binding, envelope));
    }
    let registry = ResourceRegistry::with_members(site, &ports, &fixed, &named).unwrap();
    let encoded: Vec<_> = rows
        .into_iter()
        .map(|(binding, row)| (binding, serde_json::to_vec(&row).unwrap()))
        .collect();
    let captured: Vec<_> = encoded
        .iter()
        .map(|(binding, json)| CapturedResource {
            binding: *binding,
            json,
        })
        .collect();
    let candidate = ModularCandidate::assemble(&registry, &captured).unwrap();
    PreparedConfiguration::prepare(&candidate, Some("UTC"), &ProfileInputs::installed()).unwrap()
}

pub(crate) fn binding(services: HostServices) -> crate::services::StationBinding {
    let id = LOCAL.parse::<ResourceId>().unwrap();
    let configuration = prepare(
        &[(LOCAL, "1000", json!({}))],
        &[(
            LOCAL,
            ResourceKind::Interface,
            "",
            json!({"audio":{"device_identifier":"usb"}}),
        )],
    );
    let station = RadioPreparationApi::open()
        .unwrap()
        .prepare_station(configuration.runtime().radio.port(id).unwrap(), "*6")
        .unwrap();
    services
        .prepare_station_binding(&station, NonZeroU64::MIN)
        .unwrap()
}

pub(crate) fn radio(services: HostServices, maximum_frames: usize) -> Radio {
    binding(services).open_radio(maximum_frames).unwrap()
}

pub(crate) struct Operations(pub(crate) abi::rptadv_station_binding_v1);

// SAFETY: the table is immutable and its capability string is process-lifetime.
unsafe impl Sync for Operations {}

pub(crate) static BINDING: Operations = Operations(abi::rptadv_station_binding_v1 {
    struct_size: size_of::<abi::rptadv_station_binding_v1>() as u32,
    abi_version: 1,
    capability_name: c"rptadv.station-binding1".as_ptr(),
    prepare: Some(prepare_binding),
    open_prepared: Some(open_binding),
    stage_update: Some(stage),
    release_plan: Some(release),
});

unsafe extern "C" fn prepare_binding(
    _: *mut c_void,
    request: *const abi::rptadv_station_request_v1,
    result: *mut *mut c_void,
) -> i32 {
    // SAFETY: the fixture receives a complete stack request and unique output
    // from the real Product station binding client.
    let (Some(request), Some(output)) = (unsafe { request.as_ref() }, unsafe { result.as_mut() })
    else {
        return -1;
    };
    *output = ptr::null_mut();
    if request.abi_version != 1
        || request.struct_size as usize != size_of::<abi::rptadv_station_request_v1>()
        || request.port_uuid == [0; 16]
        || request.candidate_identity == 0
        || request.prepared.is_null()
    {
        return -1;
    }
    // SAFETY: this request borrows its matched product1 descriptor and actual
    // provider preparation with the DSO retained by the calling station.
    let Some(visit) = (unsafe { request.product.as_ref() })
        .and_then(|product| product.native_configuration_visit)
    else {
        return -1;
    };
    let mut saw_station = false;
    // SAFETY: the provider lends its request synchronously and receives a live result flag.
    let status = unsafe {
        visit(
            request.prepared.cast_mut(),
            Some(visit_binding),
            ptr::from_mut(&mut saw_station).cast(),
        )
    };
    if status != 0 || !saw_station {
        return -1;
    }
    let mut name = String::with_capacity(36);
    for (index, byte) in request.port_uuid.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            name.push('-');
        }
        write!(&mut name, "{byte:02x}").unwrap();
    }
    *output = Box::into_raw(Box::new(name)).cast();
    0
}

unsafe extern "C" fn visit_binding(
    context: *mut c_void,
    config: *const abi::UrpNativeStationConfig,
) -> i32 {
    // SAFETY: the provider borrows a complete configuration and passes our live flag.
    let Some(config) = (unsafe { config.as_ref() }) else {
        return -1;
    };
    unsafe { *context.cast::<bool>() = !config.radio.is_null() };
    0
}

unsafe extern "C" fn open_binding(
    _: *mut c_void,
    plan: *const c_void,
    _: usize,
    output: *mut *mut c_void,
) -> i32 {
    // SAFETY: the station owner borrows the original plan until open returns.
    let Some(name) = (unsafe { plan.cast::<String>().as_ref() }) else {
        return -1;
    };
    unsafe { super::open_radio(name, output) }
}

unsafe extern "C" fn stage(_: *mut c_void, _: *mut c_void, _: *const c_void) -> i32 {
    // The deterministic RF fixture supports the ordinary replacement path.
    1
}

pub(crate) unsafe extern "C" fn finish(_: *mut c_void, _: *mut c_void, phase: u32) -> i32 {
    if phase == 2 { 0 } else { -1 }
}

unsafe extern "C" fn release(_: *mut c_void, plan: *mut c_void) {
    // SAFETY: Product releases the unique plan that this fixture allocated.
    drop(unsafe { Box::from_raw(plan.cast::<String>()) });
}
