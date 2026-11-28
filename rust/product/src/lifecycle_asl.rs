//! Product-owned directory proofs retained across a native call handoff.
use super::*;
use crate::{directory::VerifiedRoute, host::asl::Account};
use rpt_advanced_core::link::ProtocolId;
use std::{net::IpAddr, ptr};

pub(super) struct Outbound {
    station: rpt_advanced_core::config::resources::candidate::ResourceId,
    route: VerifiedRoute,
    endpoint: std::net::SocketAddr,
}

impl Outbound {
    pub(super) fn current(&self, host: &Host, remote: &str) -> bool {
        host.asl_account(self.station).is_ok_and(|account| {
            account.permits(remote)
                && self
                    .route
                    .destination(&account.directory, remote, None)
                    .is_some()
        })
    }

    pub(super) fn originate(
        engine: &Arc<Engine>,
        account: Account,
        remote: &str,
        current: impl Fn() -> bool,
    ) -> Result<(crate::services::PeerIo, Self), RuntimeError> {
        let route = account
            .directory
            .resolve(engine.services, remote, None)
            .map_err(|_| RuntimeError::Rejected)?;
        let endpoint = route
            .outbound_endpoint(&engine.services)
            .map_err(|_| RuntimeError::Rejected)?;
        let proof = Self {
            station: account.station,
            route,
            endpoint,
        };
        let name = remote.to_owned();
        let proof = engine.run(move |host| {
            if proof.current(host, &name) {
                Ok(proof)
            } else {
                Err(RuntimeError::Rejected)
            }
        })?;
        if !current() {
            return Err(RuntimeError::Rejected);
        }
        let peer = engine
            .services
            .originate_asl(&account.identity(), remote, proof.endpoint, current)
            .map_err(|_| RuntimeError::Rejected)?;
        if peer.identity().protocol() != ProtocolId::AllStarLink
            || peer.local() != account.wire.to_string()
            || peer.identity().wire_identity() != remote
            || (engine
                .services
                .asl_provider()
                .map_err(|_| RuntimeError::Rejected)?
                .media_kind
                == 1)
                != peer.async_owner().is_some()
        {
            return Err(RuntimeError::Rejected);
        }
        Ok((peer, proof))
    }
}

pub(crate) static ADMISSION: abi::rptadv_asl_admission_v1 = abi::rptadv_asl_admission_v1 {
    struct_size: size_of::<abi::rptadv_asl_admission_v1>() as u32,
    abi_version: 1,
    authorize: Some(authorize),
    complete: Some(complete),
};

struct Proof {
    call: Call,
    account: Account,
    remote: String,
    source: IpAddr,
    route: VerifiedRoute,
}

unsafe fn string(span: abi::rptadv_peer_bytes_v1) -> Result<String, RuntimeError> {
    if span.length == 0 || span.length > 63 || span.data.is_null() {
        return Err(RuntimeError::Rejected);
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(span.data, span.length) })
        .map(str::to_owned)
        .map_err(|_| RuntimeError::Rejected)
}

impl Proof {
    fn capture(
        id: abi::rptadv_asl_identity_v1,
        remote: String,
        source: IpAddr,
    ) -> Result<Self, RuntimeError> {
        let (engine, call) = selected_reload(true)?;
        let account = engine.run_inner(true, move |host| host.asl_identity(&id))?;
        let route = account
            .directory
            .resolve(engine.services, &remote, Some(source))
            .map_err(|_| RuntimeError::Rejected)?;
        let proof = Self {
            call,
            account,
            remote,
            source,
            route,
        };
        engine.run_inner(true, move |host| {
            proof.check(host)?;
            Ok(proof)
        })
    }

    fn check(&self, host: &mut Host) -> Result<(), RuntimeError> {
        let selected = host.asl_account(self.account.station)?;
        self.route
            .destination(&selected.directory, &self.remote, Some(self.source))
            .ok_or(RuntimeError::Rejected)?;
        host.runtime
            .authorize_incoming(&selected.station.to_string(), &self.remote, true)
    }
}

unsafe extern "C" fn authorize(
    id: *const abi::rptadv_asl_identity_v1,
    remote: abi::rptadv_peer_bytes_v1,
    source: abi::rptadv_peer_bytes_v1,
    out: *mut *mut c_void,
) -> i32 {
    let Some(out) = (unsafe { out.as_mut() }) else {
        return -1;
    };
    *out = ptr::null_mut();
    catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        if id.is_null() || !id.is_aligned() {
            return Err(RuntimeError::Rejected);
        }
        let source = unsafe { string(source)? }
            .parse()
            .map_err(|_| RuntimeError::Rejected)?;
        let proof = Proof::capture(unsafe { id.read() }, unsafe { string(remote)? }, source)?;
        *out = Box::into_raw(Box::new(proof)).cast();
        Ok(())
    }))
    .ok()
    .and_then(Result::ok)
    .map_or(-1, |()| 0)
}

unsafe extern "C" fn complete(proof: *mut c_void, peer: *mut c_void) -> i32 {
    if proof.is_null() {
        return 1;
    }
    let consumed = std::cell::Cell::new(false);
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), RuntimeError> {
        let proof = unsafe { Box::from_raw(proof.cast::<Proof>()) };
        if peer.is_null() {
            return Ok(());
        }
        // The activity guard covers both control visits. No global ENGINE acquisition
        // is required even if a concurrent stop has closed admission.
        let engine = Arc::clone(&proof.call.0);
        let io = unsafe { engine.services.peer(peer) }.map_err(|_| RuntimeError::Rejected)?;
        consumed.set(true);
        if io.identity().protocol() != ProtocolId::AllStarLink
            || io.local() != proof.account.wire.to_string()
            || io.identity().wire_identity() != proof.remote
            || (engine
                .services
                .asl_provider()
                .map_err(|_| RuntimeError::Rejected)?
                .media_kind
                == 1)
                != io.async_owner().is_some()
        {
            return Err(RuntimeError::Rejected);
        }
        let clock = engine.clock();
        engine.run_inner(true, move |host| {
            proof.check(host)?;
            host.accept_verified_peer(&proof.account.station.to_string(), &proof.remote, io, clock)
        })
    }));
    match result {
        Ok(Ok(())) => 0,
        _ if consumed.get() => -1,
        _ => 1,
    }
}
