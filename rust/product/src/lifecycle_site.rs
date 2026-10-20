//! Site ownership barriers wait outside serialized control and audio callbacks.
use super::*;
use crate::site::{acquisition::Acquisition, radio::RadioSite};
use std::time::Duration;

const HANDOFF_TIMEOUT: Duration = Duration::from_secs(5);

impl Engine {
    /// Service safe-output acknowledgments while ordinary command admission is closed.
    pub(super) fn stop_sites(self: &Arc<Self>) -> bool {
        if self
            .host
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
            return true;
        }
        self.stop_sites_until(Instant::now() + HANDOFF_TIMEOUT)
    }

    fn stop_sites_until(self: &Arc<Self>, deadline: Instant) -> bool {
        loop {
            let now = self.clock().now_ms;
            match self.site_control(move |host| host.site_shutdown(now)) {
                Ok(Ok(true)) => return true,
                Ok(Ok(false)) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => return false,
            }
        }
    }

    /// Cleanup must remain admissible after normal command admission closes.
    fn site_control<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Host) -> T + Send + 'static,
    ) -> Result<T, RuntimeError> {
        let engine = Arc::clone(self);
        let (send, receive) = mpsc::sync_channel(1);
        self.executor
            .submit(ControlTask::lifecycle(move || {
                let mut host = engine.host.lock().unwrap_or_else(|e| e.into_inner());
                let result = host.as_mut().map(operation).ok_or(RuntimeError::Rejected);
                let _ = send.send(result);
            }))
            .map_err(|_| RuntimeError::Rejected)?;
        receive.recv().map_err(|_| RuntimeError::Rejected)?
    }

    fn wait_sites(self: &Arc<Self>, resume: bool) -> Result<(), RuntimeError> {
        self.wait_sites_until(resume, Instant::now() + HANDOFF_TIMEOUT)
    }

    fn wait_sites_until(
        self: &Arc<Self>,
        resume: bool,
        deadline: Instant,
    ) -> Result<(), RuntimeError> {
        loop {
            let now = self.clock().now_ms;
            let ready = self.run(move |host| {
                if resume {
                    Ok(host.resume_sites())
                } else {
                    host.site_barrier(now)
                }
            })?;
            if ready {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(RuntimeError::Busy);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Prepare/fence physical owners before briefly gating controller publication.
    pub(super) fn reload_configuration(
        self: &Arc<Self>,
        document: ConfigDocument,
    ) -> Result<(), RuntimeError> {
        self.site_backends.validate(&document)?;
        let settings = crate::site::configured(&document)?;
        let definition = document.clone();
        let basis = self.run(move |host| host.site_basis(&definition))?;
        let acquisition = Acquisition::prepare_reload(
            &settings,
            Arc::clone(&self.site_backends),
            self.origin,
            basis.known,
        )
        .map_err(|_| RuntimeError::Preparation)?;
        let radio = RadioSite::prepare(&settings).map_err(|_| RuntimeError::Preparation)?;
        // A rejected executor task drops only an Arc; its owner is retained here
        // so dormant workers are never joined on the control executor.
        let prepared = Arc::new(Mutex::new((Some(acquisition), Some(radio))));
        let target = Arc::clone(&prepared);
        self.run(move |host| {
            let mut pair = target.lock().unwrap_or_else(|e| e.into_inner());
            let (acquisition, radio) = &mut *pair;
            host.begin_site_reload(basis.revision, acquisition, radio, &basis.replaced)
        })?;
        let result = self.wait_sites(false).and_then(|()| {
            self.reloading.store(true, Ordering::Release);
            self.revision.fetch_add(1, Ordering::AcqRel);
            self.lost_digits.store(true, Ordering::Release);
            let clock = self.clock();
            catch_unwind(AssertUnwindSafe(|| {
                self.run_inner(true, move |host| host.reload(document, clock))
            }))
            .unwrap_or(Err(RuntimeError::Preparation))
        });
        self.reloading.store(false, Ordering::Release);
        // Host has already cleared the candidate when Runtime published it. A later
        // notification error must not roll back physical ownership to old settings.
        let retired = self.site_control(Host::abort_sites)?;
        drop(retired);
        if self.accepting.load(Ordering::Acquire) {
            self.wait_sites(true)?;
        }
        if result.is_ok() {
            let _ = self.pump(self.clock(), true);
        }
        result
    }
}

#[cfg(test)]
#[path = "lifecycle_site_tests.rs"]
mod tests;
