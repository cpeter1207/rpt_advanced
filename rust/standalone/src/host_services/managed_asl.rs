//! Managed ordinary IAX transactions. Only the frontend thread owns a listener.
use super::*;
use crate::{
    iax::{
        managed::{Authority, Incoming, Listeners},
        playout::Delays,
    },
    registration::{ManagedRegistrar, RegistrarIdentity, RegistrarState},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU16, NonZeroU64},
    sync::{Condvar, Mutex},
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) mod peers;
use peers::{Admission, PeerRegistry};
fn frontend<'a>(raw: *mut c_void) -> Option<&'a Frontend> {
    if raw.is_null() {
        return None;
    }
    // The frontend field is installed before product start and remains immutable through stop.
    unsafe { (&*ptr::addr_of!((*raw.cast::<StandaloneHostContext>()).asl)).as_ref() }
}

pub(super) static OPERATIONS: abi::rptadv_asl_provider_v2 = abi::rptadv_asl_provider_v2 {
    struct_size: size_of::<abi::rptadv_asl_provider_v2>() as u32,
    abi_version: 2,
    media_kind: 1,
    reserved: 0,
    prepare: Some(prepare),
    step: Some(step),
    observe: Some(observe),
    originate: Some(peers::originate),
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, sync::mpsc};

    thread_local! {
        static OBSERVING: RefCell<Option<mpsc::Sender<()>>> = const { RefCell::new(None) };
    }

    pub(super) fn before_observing_worker() {
        OBSERVING.with(|slot| {
            if let Some(send) = slot.take() {
                let _ = send.send(());
            }
        });
    }

    #[test]
    fn registrar_observation_leaves_publication_available_to_reversal() {
        let mut host = HostServicesOwner::new(None).unwrap();
        let shared = Arc::new(Shared {
            state: Default::default(),
            wake: Condvar::new(),
            network: host.network_access().unwrap(),
        });
        host.context.asl = Some(Frontend {
            shared: Arc::clone(&shared),
            thread: None,
        });
        let local = Authority {
            station: [1; 16],
            account: [2; 16],
            port: NonZeroU16::new(4569).unwrap(),
            wire: NonZeroU64::new(716701).unwrap(),
            generation: NonZeroU64::MIN,
        };
        let registration = Registration {
            version: [3; 16],
            url: "https://127.0.0.1/".into(),
            seconds: 60,
        };
        let id = RegistrarIdentity {
            station: local.station,
            account: local.account,
            record_version: registration.version,
            attempt: NonZeroU64::MIN,
        };
        let mut old = ManagedRegistrar::prepare(
            id,
            local.wire,
            Some(&registration.url),
            local.port,
            registration.seconds,
            Some("fixture"),
        )
        .unwrap();
        assert!(old.withdraw());
        let old = Arc::new(Mutex::new(old));
        {
            let mut state = shared.lock();
            state.registrar_serial = id.attempt.get();
            state.published.insert(
                local.station,
                Row {
                    authority: local,
                    delays: Delays::new(20, 40, 100).unwrap(),
                    worker: Some(old.clone()),
                    registration: Some(registration),
                },
            );
        }

        let (ready, locked) = mpsc::channel();
        let (enter, observe_started) = mpsc::channel();
        let replacement = std::thread::spawn(move || {
            let old = old.lock().unwrap();
            ready.send(()).unwrap();
            observe_started
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            // The reverse owner already holds the preceding worker. Fail promptly if
            // publication is unavailable, releasing the worker so the observer can exit.
            let Ok(mut state) = shared.state.try_lock() else {
                return false;
            };
            let next = serial(&mut state.registrar_serial).unwrap();
            state.published.get_mut(&local.station).unwrap().worker =
                Some(Arc::new(Mutex::new(old.replacement(next))));
            true
        });
        locked.recv_timeout(Duration::from_secs(5)).unwrap();
        OBSERVING.with(|slot| {
            *slot.borrow_mut() = Some(enter);
        });
        let mut status = 0;
        let result = unsafe { observe(host.descriptor().context, &identity(local), &mut status) };
        assert!(
            replacement.join().unwrap(),
            "observer retained publication while waiting for the selected worker"
        );
        assert!(
            matches!((result, status), (0, 1) | (-1, 0)),
            "observer exposed the withdrawn registrar"
        );
    }
}

const MEDIA_KIND: u32 = 1;
const POLL_DELAY: Duration = Duration::from_millis(5);
fn idle_admissions(_: &PeerRegistry) -> bool {
    true
}
#[cfg(test)]
use tests::before_observing_worker;
fn apply_transport(listeners: &mut Listeners, shared: &Shared, job: &Selection) -> Result<(), ()> {
    let ingress = shared.network.open().map_err(|_| ())?;
    listeners
        .apply(job.attempt, &ingress.client, |server| {
            server.enable_async(&ingress.executor)
        })
        .map_err(|_| ())
}
fn poll_transport(
    listeners: &mut Listeners,
    shared: &Arc<Shared>,
    callbacks: abi::rptadv_asl_admission_v1,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32);
    let _ = listeners.poll(now, &mut Admission::new(shared, callbacks));
}
include!("../../../peer-support/frontend/managed_asl.rs");
