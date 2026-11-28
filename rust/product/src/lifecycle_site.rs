//! Site ownership barriers wait outside serialized control and audio callbacks.
use super::*;
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
    pub(super) fn site_control<T: Send + 'static>(
        self: &Arc<Self>,
        operation: impl FnOnce(&mut Host) -> T + Send + 'static,
    ) -> Result<T, RuntimeError> {
        let engine = Arc::clone(self);
        let (send, receive) = mpsc::sync_channel(1);
        self.enqueue(move || {
            let mut host = engine.host.lock().unwrap_or_else(|e| e.into_inner());
            let result = host.as_mut().map(operation).ok_or(RuntimeError::Rejected);
            let _ = send.send(result);
        })?;
        receive.recv().map_err(|_| RuntimeError::Rejected)?
    }

    pub(super) fn wait_sites(self: &Arc<Self>, resume: bool) -> Result<(), RuntimeError> {
        #[cfg(test)]
        crate::fixture::SITE_RESUME_VISIT.set(resume);
        let result = self.wait_sites_until(resume, Instant::now() + HANDOFF_TIMEOUT);
        #[cfg(test)]
        crate::fixture::SITE_RESUME_VISIT.set(false);
        result
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

    pub(super) fn finish_reload_sites(self: &Arc<Self>, applied: bool) -> Result<(), RuntimeError> {
        self.reloading.store(false, Ordering::Release);
        // Host has already cleared the candidate when Runtime published it. A later
        // notification error must not roll back physical ownership to old settings.
        let retired = self.site_control(Host::abort_sites)?;
        drop(retired);
        if self.accepting.load(Ordering::Acquire) {
            self.wait_sites(true)?;
        }
        if applied {
            let _ = self.pump(self.clock(), true);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "lifecycle_site_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::{
    install_delayed_site_bank, site_bank_delay, site_bank_electrical, site_bank_sample,
};
