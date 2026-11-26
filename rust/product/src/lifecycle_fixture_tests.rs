//! Start a unit-test Engine with real JSON owners and explicit fake host edges.
use super::super::*;
use crate::{management::PreparedConfiguration, site::backends::SiteBackends};

/// Create the checked lifecycle owner for internal ownership and command tests.
///
/// # Safety
/// Both complete descriptor allocations and callbacks remain live until successful stop.
pub(crate) unsafe fn fixture_engine(
    configuration: PreparedConfiguration,
    table: *const abi::rptadv_host_services_v11,
    control: *const ControlDescriptor,
) -> Result<Arc<Engine>, RuntimeError> {
    // SAFETY: the caller retains the descriptor storage for the complete lifetime.
    unsafe { fixture_engine_with_site(configuration, table, control, None, None) }
}

/// Use an existing backend and, when supplied, the actual test bootstrap clock.
///
/// # Safety
/// The descriptors and their code remain live through successful lifecycle stop.
pub(crate) unsafe fn fixture_engine_with_site(
    configuration: PreparedConfiguration,
    table: *const abi::rptadv_host_services_v11,
    control: *const ControlDescriptor,
    backends: Option<Arc<SiteBackends>>,
    initial_clock: Option<RuntimeClock>,
) -> Result<Arc<Engine>, RuntimeError> {
    let mut selected = ENGINE.lock().unwrap_or_else(|e| e.into_inner());
    if selected.is_some() {
        return Err(RuntimeError::Busy);
    }
    // SAFETY: the caller and the linked test providers retain all descriptor code.
    let (services, executor, media) = unsafe {
        host_components(
            table,
            control,
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
        )
    }?;
    let backends = match backends {
        Some(backends) => backends,
        None => Arc::new(SiteBackends::load_scopes(
            configuration.runtime().acquisition.values(),
        )?),
    };
    let origin = Instant::now();
    let clock = initial_clock.unwrap_or_else(|| capture(origin, services));
    let inputs = configuration.inputs().clone();
    let mut host = Host::fixture_start_with_site(
        configuration,
        media,
        services,
        origin,
        clock,
        Some(Arc::clone(&backends)),
    )?;
    let revision = format!("fixture-{}", host.runtime.revision());
    let observed = host.observed_configuration(&revision, &inputs)?;
    let gate = host.runtime.configuration_gate();
    let engine = Arc::new(Engine::new(services, executor, origin));
    engine.management_state.install_gate(gate);
    engine.management_state.fixture_observed(observed);
    *engine.host.lock().unwrap() = Some(host);
    if let Err(error) = engine.start_ticker() {
        if !engine.stop() {
            *selected = Some(engine);
        }
        return Err(error);
    }
    *selected = Some(Arc::clone(&engine));
    Ok(engine)
}

/// Drive the public pending-stop ABI until actual shutdown or a bounded refusal.
pub(crate) fn fixture_stop() -> i32 {
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    // SAFETY: the linked getter returns the immutable complete Product table.
    let descriptor = unsafe { &*rptadv_product_descriptor_v1() };
    let stop = descriptor.stop.expect("Product stop capability");
    loop {
        // SAFETY: the selected linked provider and its getter remain live.
        let result = unsafe { stop() };
        if result == 0 || Instant::now() >= deadline {
            return result;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Observe the existing asynchronous jobs without removing or fabricating their completion.
pub(crate) fn fixture_jobs_finished(engine: &Engine) -> bool {
    engine
        .jobs
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .all(|job| job.is_finished())
}
