//! Owned product1 prepared stations, with synchronous visits and semantic equality.
use super::{
    PreparationError, PreparedPeer, Product, RadioPreparationApi, Result, abi, diagnostic,
    equivalent, owned, template,
};
use crate::configuration::radio::ResolvedRadio;
use rpt_advanced_core::{
    config::resources::{candidate::ResourceId, profiles::radio::RadioPortProfile},
    remote_base_command::RemoteCommandConfiguration,
};
use std::{ffi::c_void, ptr, sync::Arc};

/// Stable projection copied during a hardware-free provider visit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StationInspection {
    /// Exact requested sound-card identity when selected.
    pub device_identifier: Option<String>,
    /// Exact requested USB serial when selected.
    pub usb_serial: Option<String>,
    /// Number of 20-ms carrier-on admission blocks.
    pub receive_on_delay_blocks: u32,
    /// Key-up settling time in milliseconds.
    pub settle_ms: i32,
    /// Full-duplex qualification selected.
    pub full_duplex: bool,
}
/// One resolved station owned by the matched provider. Its accepted definition outlives every borrowed input span.
pub struct PreparedStation {
    // Release the opaque configuration before unloading its provider.
    pub(super) resolved: ResolvedRadio,
    pub(super) product: Arc<Product>,
    id: ResourceId,
    controller: super::ControllerRadioPolicy,
}
impl RadioPreparationApi {
    /// Synchronously admit a resolved modular station, without opening a radio, audio stream, or GPIO.
    pub fn prepare_station(
        &self,
        p: &RadioPortProfile,
        remote_prefix: &str,
    ) -> Result<PreparedStation> {
        template::with_request(p, remote_prefix, |request| {
            let mut raw = ptr::null_mut();
            let mut field = None;
            // SAFETY: the typed builder retains the nested radio request, settings, strings and result storage.
            let status = unsafe {
                (self.product.stations().prepare.expect("validated"))(
                    request,
                    Some(diagnostic),
                    ptr::from_mut(&mut field).cast(),
                    &mut raw,
                )
            };
            let handle = owned(status, raw, field)?;
            // SAFETY: the unique handle and its validated descriptor come from this same retained DSO.
            let resolved =
                unsafe { ResolvedRadio::from_prepared(handle, self.product.product.as_ptr()) }
                    .map_err(|_| {
                        // SAFETY: adoption failed; the still-unique handle must be returned to the same provider.
                        unsafe {
                            (self
                                .product
                                .ordinary()
                                .native_configuration_destroy
                                .expect("validated"))(handle.as_ptr())
                        };
                        PreparationError::Incompatible
                    })?;
            Ok(PreparedStation {
                resolved,
                product: self.product.clone(),
                id: p.id,
                controller: super::ControllerRadioPolicy::from_profile(p),
            })
        })
    }
}
impl PreparedStation {
    /// Borrow this exact provider and prepared owner through the synchronous host call.
    pub(crate) fn binding_request(
        &self,
        candidate_identity: std::num::NonZeroU64,
    ) -> (abi::rptadv_station_request_v1, RadioPreparationApi) {
        let request = abi::rptadv_station_request_v1 {
            struct_size: size_of::<abi::rptadv_station_request_v1>() as u32,
            abi_version: 1,
            port_uuid: *self.port_id().uuid().as_bytes(),
            candidate_identity: candidate_identity.get(),
            product: self.product.product.as_ptr(),
            prepared: self.resolved.raw().as_ptr(),
        };
        (
            request,
            RadioPreparationApi {
                product: Arc::clone(&self.product),
            },
        )
    }

    /// Stable registry UUID of the physical owner.
    pub fn port_id(&self) -> ResourceId {
        self.id
    }
    /// Local RX/TX programme admission and activity settings owned by the controller.
    pub fn controller_policy(&self) -> &super::ControllerRadioPolicy {
        &self.controller
    }
    /// Compare controller and provider semantic identity across preparations made by the same product instance.
    pub fn equivalent(&self, other: &Self) -> Result<bool> {
        self.product.same(&other.product)?;
        if self.controller != other.controller {
            return Ok(false);
        }
        let mut flag = 2;
        // SAFETY: the immutable handles are live and checked against their common owner.
        let status = unsafe {
            (self.product.stations().equal.expect("validated"))(
                self.resolved.raw().as_ptr(),
                other.resolved.raw().as_ptr(),
                &mut flag,
            )
        };
        equivalent(status, flag)
    }
    /// Copy provider-derived remote command admission options.
    pub fn remote_commands(&self) -> Result<Option<RemoteCommandConfiguration>> {
        self.resolved
            .remote_commands()
            .map_err(|_| PreparationError::Incompatible)
    }
    /// Visit the complete native request synchronously. All nested provider pointers expire at callback return.
    pub fn visit(&self, mut visitor: impl FnMut(&abi::UrpNativeStationConfig)) -> Result<()> {
        self.resolved
            .visit(|raw| {
                // SAFETY: the product SDK lends a complete station request for this synchronous visitor.
                let value =
                    unsafe { raw.as_ref() }.ok_or("provider returned an empty configuration")?;
                visitor(value);
                Ok(())
            })
            .map_err(|_| PreparationError::Incompatible)
    }
    /// Copy physical identity and timing from the accepted SDK object.
    pub fn inspect(&self) -> Result<StationInspection> {
        let mut result = None;
        self.visit(|s| {
            // SAFETY: nested radio and byte spans are supplied by the matched SDK for this visit only.
            let Some(r) = (unsafe { s.radio.as_ref() }) else {
                return;
            };
            let bytes = |data: *const u8, n: u32| -> Option<String> {
                if n == 0 || data.is_null() {
                    return None;
                }
                // SAFETY: provider visitor lends exactly n bytes; copy them before return.
                std::str::from_utf8(unsafe { std::slice::from_raw_parts(data, n as usize) })
                    .ok()
                    .map(str::to_owned)
            };
            result = Some(StationInspection {
                device_identifier: bytes(s.device_identifier, s.device_identifier_length),
                usb_serial: bytes(s.usb_serial, s.usb_serial_length),
                receive_on_delay_blocks: r.qualification.rx_on_delay_blocks,
                settle_ms: r.transmit.tx_settle_time_ms,
                full_duplex: r.qualification.radio_duplex != 0,
            });
        })?;
        result.ok_or(PreparationError::Incompatible)
    }
    /// Create an independent negotiated-rate graph from the retained station's selected peer profile.
    pub fn prepare_peer(&self, rate: u32, frames: u32) -> Result<PreparedPeer> {
        PreparedPeer::create(self.product.clone(), rate, |ffmpeg, out| {
            // SAFETY: the graph factory retains the FFmpeg DSO; all inputs remain live for the call.
            unsafe {
                (self.product.stations().link_prepare.expect("validated"))(
                    self.resolved.raw().as_ptr() as *const c_void,
                    ffmpeg,
                    rate,
                    frames,
                    out,
                )
            }
        })
    }
}
