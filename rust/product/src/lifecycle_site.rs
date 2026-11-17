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
            let fatal = self.reload_status.load(Ordering::Acquire) == -3;
            match self.site_control(move |host| {
                if fatal {
                    host.site_fatal_shutdown(now)
                } else {
                    host.site_shutdown(now)
                }
            }) {
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
        self.enqueue(move || {
            let mut host = engine.host.lock().unwrap_or_else(|e| e.into_inner());
            let result = catch_unwind(AssertUnwindSafe(|| {
                #[cfg(test)]
                inject_control_panic();
                host.as_mut().map(operation).ok_or(RuntimeError::Rejected)
            }))
            .unwrap_or_else(|_| {
                engine.record_reload_status(-3);
                engine.reloading.store(true, Ordering::Release);
                Err(RuntimeError::Restore)
            });
            let _ = send.send(result);
        })?;
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
            let ready = self.run_inner(true, move |host| {
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
        if self.reload_fenced() {
            return Err(RuntimeError::Busy);
        }
        self.record_reload_status(-1);
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
        // From the first mutating handoff onward, an absent reply cannot prove
        // rejection. Keep admission fenced until a terminal disposition is known.
        self.reloading.store(true, Ordering::Release);
        self.record_reload_status(1);
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.lost_digits.store(true, Ordering::Release);
        let handoff = catch_unwind(AssertUnwindSafe(|| {
            self.run_inner(true, move |host| {
                let mut pair = target.lock().unwrap_or_else(|e| e.into_inner());
                let (acquisition, radio) = &mut *pair;
                Ok(host.begin_site_reload(basis.revision, acquisition, radio, &basis.replaced))
            })
        }))
        .unwrap_or(Err(RuntimeError::Restore));
        match handoff {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.record_reload_status(-1);
                return self.finish_reload(Err(error));
            }
            Err(error) => return self.finish_reload(Err(error)),
        }
        let barrier = self.wait_sites(false);
        if barrier.is_err()
            && !matches!(barrier, Err(RuntimeError::Rejected | RuntimeError::Restore))
        {
            self.record_reload_status(-1);
        }
        let result = barrier.and_then(|()| {
            let clock = self.clock();
            self.record_reload_status(1);
            let engine = Arc::clone(self);
            catch_unwind(AssertUnwindSafe(|| {
                self.run_inner(true, move |host| {
                    let result = host.reload(document, clock);
                    engine.record_reload_status(host.reload_status(result.as_ref().err()));
                    result.map(|()| host.runtime.reload_pending())
                })
            }))
            .unwrap_or(Err(RuntimeError::Preparation))
        });
        if result == Ok(true) {
            return Ok(());
        }
        self.finish_reload(result.map(|_| ()))
    }

    pub(super) fn poll_reload_configuration(self: &Arc<Self>) -> Result<bool, RuntimeError> {
        if !self.reload_fenced() {
            return Ok(true);
        }
        if self.reload_status.load(Ordering::Acquire) == -3 {
            return Err(RuntimeError::Restore);
        }
        let clock = self.clock();
        let engine = Arc::clone(self);
        let result = self.run_inner(true, move |host| {
            let result = host.poll_reload(clock);
            engine.record_reload_status(host.reload_status(result.as_ref().err()));
            result
        });
        if result == Ok(false) {
            return Ok(false);
        }
        self.finish_reload(result.map(|_| ())).map(|()| true)
    }

    /// Fatal uncertainty is absorbing until this entire Engine is shut down.
    pub(super) fn record_reload_status(&self, status: i32) -> i32 {
        let previous = self
            .reload_status
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(if current == -3 { -3 } else { status })
            })
            .expect("terminal update always succeeds");
        if previous == -3 { -3 } else { status }
    }

    fn finish_reload(
        self: &Arc<Self>,
        result: Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        let mut status = self.reload_status.load(Ordering::Acquire);
        if result.is_err() && status == 1 {
            // A lost reply or panic did not confirm either publication or rollback.
            status = -3;
        }
        status = self.record_reload_status(status);
        if status == -3 {
            self.record_reload_status(-3);
            self.reloading.store(true, Ordering::Release);
            return result.and(Err(RuntimeError::Restore));
        }
        let mut result = result;
        // Cleared candidates are already committed. Return retired workers for
        // off-control destruction; never restore an old product document.
        match self.site_control(Host::abort_sites) {
            Ok(retired) => drop(retired),
            Err(error) => {
                eprintln!("rpt-advanced: reload site cleanup failed: {error:?}");
                if result.is_ok() {
                    result = Err(error);
                }
                status = if matches!(status, 0 | -2) { -2 } else { -3 };
            }
        }
        status = self.record_reload_status(status);
        if status == -3 {
            self.record_reload_status(-3);
            self.reloading.store(true, Ordering::Release);
            return result.and(Err(RuntimeError::Restore));
        }
        if self.accepting.load(Ordering::Acquire) {
            if let Err(error) = self.wait_sites(true) {
                eprintln!("rpt-advanced: reload site resume failed: {error:?}");
                if result.is_ok() {
                    result = Err(error);
                }
                status = if matches!(status, 0 | -2) { -2 } else { -3 };
            }
        }
        status = self.record_reload_status(status);
        if status == -3 {
            self.record_reload_status(-3);
            self.reloading.store(true, Ordering::Release);
            return result.and(Err(RuntimeError::Restore));
        }
        self.reloading.store(false, Ordering::Release);
        if result.is_ok() {
            if let Err(error) = self.pump(self.clock(), true) {
                eprintln!("rpt-advanced: reload notification failed: {error:?}");
                result = Err(error);
                status = -2;
            }
        }
        if result.is_err() && status == 0 {
            status = -2;
        }
        status = self.record_reload_status(status);
        #[cfg(test)]
        if FATAL_BEFORE_FINISH_CLEAR.swap(false, Ordering::AcqRel) {
            self.record_reload_status(-3);
            self.reloading.store(true, Ordering::Release);
        }
        self.reloading.store(status == -3, Ordering::Release);
        result
    }
}

#[cfg(test)]
#[path = "lifecycle_site_tests.rs"]
mod tests;
