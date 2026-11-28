//! Each configured private listener has one serialized account owner. Calls retain the
//! provider's native executor and never allocate a signaling thread per audio session.
use super::{Call, IaxClient, Listener, ProviderCall, listener::PendingDial};
use crate::{
    Error, JitterProvider,
    accounts::Route,
    calls::PrivateCall,
    selection::{DialReply, Incoming},
};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

type CallResult = SyncSender<DialReply>;

struct PendingCall {
    dial: PendingDial,
    cancel: Arc<AtomicBool>,
    reply: CallResult,
}

fn adopt_call(
    owned: ProviderCall,
    selected: &Route,
    jitter: &Arc<JitterProvider>,
    delays: [u32; 3],
    calls: &mut Vec<Weak<Call>>,
) -> Result<PrivateCall, Error> {
    let call = Call::new(owned, selected, Arc::clone(jitter), delays)?;
    calls.push(Arc::downgrade(&call));
    Ok(PrivateCall::iax(call))
}

enum Request {
    Refresh,
    Withdraw,
    Policy([u32; 3], SyncSender<Result<(), Error>>),
    Dial(String, Arc<AtomicBool>, CallResult),
}
/// The selected account owns this exact queue and registration-state fence. The frontend
/// retries a full queue on its next control tick, leaving the selection worker free.
pub(crate) struct DialHandoff {
    commands: SyncSender<Request>,
    observed: Arc<AtomicU32>,
    pending: Option<Request>,
}

impl DialHandoff {
    pub(crate) fn submit(&mut self) -> Result<bool, Error> {
        if self.observed.load(Ordering::Acquire) != 2 {
            return Err(Error::Retired);
        }
        let request = self.pending.take().ok_or(Error::Retired)?;
        match self.commands.try_send(request) {
            Ok(()) => Ok(true),
            Err(mpsc::TrySendError::Full(request)) => {
                self.pending = Some(request);
                Ok(false)
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err(Error::Retired),
        }
    }
}

struct Account {
    route: Arc<Route>,
    observed: Arc<AtomicU32>,
    commands: SyncSender<Request>,
    retiring: bool,
    policy_uncertain: bool,
    thread: Option<JoinHandle<Result<(), Error>>>,
}
impl Account {
    fn open(
        client: IaxClient,
        route: Arc<Route>,
        jitter: Arc<JitterProvider>,
        incoming: Incoming,
    ) -> Result<Self, Error> {
        let (commands, receiver) = mpsc::sync_channel(8);
        let observed = Arc::new(AtomicU32::new(1));
        let state = Arc::clone(&observed);
        let selected = Arc::clone(&route);
        let thread = thread::Builder::new()
            .name("private-iax-account".into())
            .spawn(move || {
                let result = (|| {
                    let bind: SocketAddr = if selected.remote.is_ipv4() {
                        "0.0.0.0:0"
                    } else {
                        "[::]:0"
                    }
                    .parse()
                    .map_err(|_| Error::Input)?;
                    let mut listener = Listener::bind(client, Arc::clone(&selected), bind)
                        .map_err(|_| Error::Provider)?;
                    let mut refresh = true;
                    let mut retiring = false;
                    let mut delays = selected.account.jitter_ms();
                    let mut calls = Vec::<Weak<Call>>::new();
                    let mut pending = Vec::<PendingCall>::new();
                    let supports_pending_dial = listener.supports_pending_dial();
                    loop {
                        calls.retain(|call| call.strong_count() != 0);
                        if refresh || (!retiring && listener.refresh_due()) {
                            refresh = false;
                            state.store(1, Ordering::Release);
                            state.store(
                                if listener.register(false, 5000).is_ok() {
                                    2
                                } else {
                                    4
                                },
                                Ordering::Release,
                            );
                        }
                        match receiver.recv_timeout(Duration::from_millis(5)) {
                            Ok(Request::Withdraw) => {
                                retiring = true;
                                state.store(3, Ordering::Release);
                                for attempt in &pending {
                                    attempt.dial.cancel();
                                }
                                // Even a lost prior response may represent a live remote account.
                                // Keep the native listener available to retry a failed release.
                                if listener.register(true, 5000).is_ok() {
                                    listener.withdraw_local().map_err(|_| Error::Provider)?;
                                    return Ok(());
                                }
                                state.store(4, Ordering::Release);
                            }
                            Ok(Request::Refresh) => {
                                if !retiring {
                                    refresh = true;
                                }
                            }
                            Ok(Request::Policy(next, reply)) => {
                                let result = if retiring {
                                    Err(Error::Retired)
                                } else {
                                    let mut touched = Vec::new();
                                    let mut result = Ok(());
                                    // Weak references never keep a retired native peer alive.
                                    for call in calls.iter().filter_map(Weak::upgrade) {
                                        if let Err(error) = call.change_delays(next) {
                                            result = Err(error);
                                            break;
                                        }
                                        touched.push(call);
                                    }
                                    if result.is_ok() {
                                        delays = next;
                                    } else {
                                        for call in touched.into_iter().rev() {
                                            if let Err(error) = call.change_delays(delays) {
                                                result = Err(error);
                                            }
                                        }
                                    }
                                    result
                                };
                                let _ = reply.send(result);
                            }
                            Ok(Request::Dial(destination, cancel, reply)) => {
                                if retiring || cancel.load(Ordering::Acquire) {
                                    let _ = reply.send(DialReply::Final(Err(Error::Retired)));
                                } else if supports_pending_dial {
                                    let attempt = pending
                                        .try_reserve(1)
                                        .map_err(|_| Error::Provider)
                                        .and_then(|()| {
                                            listener
                                                .begin_dial(&destination)
                                                .map_err(|_| Error::Provider)
                                        });
                                    match attempt {
                                        Ok(dial) => pending.push(PendingCall {
                                            dial,
                                            cancel,
                                            reply,
                                        }),
                                        Err(error) => {
                                            let _ = reply.send(DialReply::Final(Err(error)));
                                        }
                                    }
                                } else {
                                    // Older ABI2 providers expose only synchronous origination.
                                    let call = listener
                                        .dial(&destination)
                                        .map_err(|_| Error::Provider)
                                        .and_then(|call| {
                                            adopt_call(call, &selected, &jitter, delays, &mut calls)
                                        });
                                    if !cancel.load(Ordering::Acquire) {
                                        let _ = reply.send(DialReply::Final(call));
                                    }
                                }
                            }
                            Err(RecvTimeoutError::Disconnected) => return Err(Error::Retired),
                            Err(RecvTimeoutError::Timeout) => {}
                        }
                        pending.retain_mut(|attempt| {
                            let retired = retiring || listener.registered().is_none();
                            if retired || attempt.cancel.load(Ordering::Acquire) {
                                attempt.dial.cancel();
                            }
                            let call = match listener.poll_dial(&mut attempt.dial) {
                                Ok(None) => return true,
                                Ok(Some(call)) => {
                                    adopt_call(call, &selected, &jitter, delays, &mut calls)
                                }
                                Err(_) if retired => Err(Error::Retired),
                                Err(_) => Err(Error::Provider),
                            };
                            if !attempt.cancel.load(Ordering::Acquire) {
                                let _ = attempt.reply.send(DialReply::Final(call));
                            }
                            false
                        });
                        if !retiring && listener.registered().is_some() {
                            match listener.poll() {
                                Ok(Some(call)) => {
                                    if let Ok(call) =
                                        adopt_call(call, &selected, &jitter, delays, &mut calls)
                                    {
                                        incoming(call);
                                    }
                                }
                                Ok(None) => {}
                                Err(_) => {
                                    state.store(4, Ordering::Release);
                                    return Err(Error::Provider);
                                }
                            }
                        }
                    }
                })();
                state.store(if result.is_ok() { 0 } else { 4 }, Ordering::Release);
                result
            })
            .map_err(|_| Error::Provider)?;
        Ok(Self {
            route,
            observed,
            commands,
            retiring: false,
            policy_uncertain: false,
            thread: Some(thread),
        })
    }
    fn retain(&mut self, next: &Arc<Route>) -> Result<(), Error> {
        if self.policy_uncertain || self.route.account.jitter_ms() != next.account.jitter_ms() {
            let (reply, result) = mpsc::sync_channel(1);
            self.commands
                .try_send(Request::Policy(next.account.jitter_ms(), reply))
                .map_err(|_| Error::Provider)?;
            self.policy_uncertain = true;
            result.recv().map_err(|_| Error::Provider)??;
            self.policy_uncertain = false;
        }
        self.route = Arc::clone(next);
        Ok(())
    }
    fn withdraw(&mut self) -> Result<(), Error> {
        if !self.retiring || self.observed.load(Ordering::Acquire) == 4 {
            self.commands
                .try_send(Request::Withdraw)
                .map_err(|_| Error::Provider)?;
            self.retiring = true;
        }
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Bank {
    accounts: BTreeMap<String, Account>,
    desired: BTreeMap<String, Arc<Route>>,
    failure: bool,
}
impl Bank {
    pub(crate) fn select(&mut self, next: BTreeMap<String, Arc<Route>>) -> Result<(), Error> {
        for (key, route) in &next {
            if self.accounts.get(key).is_some_and(|a| {
                a.route.generation() == route.generation()
                    && !a.route.account.same_registration(&route.account)
            }) {
                return Err(Error::Input);
            }
        }
        let mut touched = Vec::new();
        for (key, route) in &next {
            let Some(old) = self
                .accounts
                .get_mut(key)
                .filter(|a| !a.retiring && a.route.generation() == route.generation())
            else {
                continue;
            };
            let prior = Arc::clone(&old.route);
            if let Err(error) = old.retain(route) {
                let mut result = Err(error);
                for (id, prior) in touched.into_iter().rev() {
                    if let Err(error) = self
                        .accounts
                        .get_mut(&id)
                        .expect("same native selection")
                        .retain(&prior)
                    {
                        result = Err(error);
                    }
                }
                return result;
            }
            touched.push((key.clone(), prior));
        }
        self.desired = next;
        self.failure = false;
        for (id, a) in &mut self.accounts {
            if !self
                .desired
                .get(id)
                .is_some_and(|n| Arc::ptr_eq(n, &a.route))
                || a.retiring
            {
                a.withdraw()?;
            } else if a.observed.load(Ordering::Acquire) == 4 {
                a.commands
                    .try_send(Request::Refresh)
                    .map_err(|_| Error::Provider)?;
            }
        }
        Ok(())
    }
    pub(crate) fn service(
        &mut self,
        client: Option<&IaxClient>,
        jitter: &Arc<JitterProvider>,
        incoming: &Incoming,
    ) -> Result<(), Error> {
        let completed: Vec<_> = self
            .accounts
            .iter()
            .filter(|(_, a)| a.thread.as_ref().is_some_and(|j| j.is_finished()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in completed {
            let a = self.accounts.get_mut(&id).expect("observed retained owner");
            let job = a.thread.take().expect("completed native owner");
            if !matches!(job.join(), Ok(Ok(()))) {
                self.failure = true;
                continue;
            }
            self.accounts.remove(&id);
        }
        if self.failure {
            return Err(Error::Provider);
        }
        for (id, route) in &self.desired {
            if !self.accounts.contains_key(id) {
                let account = Account::open(
                    client.ok_or(Error::Provider)?.clone(),
                    Arc::clone(route),
                    Arc::clone(jitter),
                    Arc::clone(incoming),
                )?;
                self.accounts.insert(id.clone(), account);
            }
        }
        Ok(())
    }
    pub(crate) fn states(&self) -> (bool, Vec<(String, u64, u32)>) {
        let mut ready = !self.failure && self.accounts.len() == self.desired.len();
        let mut states = Vec::with_capacity(self.accounts.len());
        for (id, a) in &self.accounts {
            let observed = a.observed.load(Ordering::Acquire);
            ready &= !a.retiring
                && observed == 2
                && self
                    .desired
                    .get(id)
                    .is_some_and(|n| Arc::ptr_eq(n, &a.route));
            states.push((id.clone(), a.route.generation(), observed));
        }
        (ready, states)
    }
    pub(crate) fn dial(
        &self,
        port: &str,
        id: &str,
        generation: u64,
        destination: String,
        cancel: Arc<AtomicBool>,
        result: CallResult,
    ) -> Result<DialHandoff, Error> {
        let a = self
            .accounts
            .get(id)
            .filter(|a| {
                !a.retiring
                    && a.route.generation() == generation
                    && a.route.account.port_id().to_string() == port
                    && a.observed.load(Ordering::Acquire) == 2
            })
            .ok_or(Error::Retired)?;
        Ok(DialHandoff {
            commands: a.commands.clone(),
            observed: Arc::clone(&a.observed),
            pending: Some(Request::Dial(destination, cancel, result)),
        })
    }
    pub(crate) fn empty(&self) -> bool {
        self.accounts.is_empty() && self.desired.is_empty() && !self.failure
    }
}

#[cfg(test)]
mod dial_tests {
    use super::*;

    #[test]
    fn full_iax_account_handoff_retains_one_exact_dial_and_rechecks_registration() {
        let (commands, owner) = mpsc::sync_channel(1);
        let observed = Arc::new(AtomicU32::new(2));
        let cancel = Arc::new(AtomicBool::new(false));
        let (result, response) = mpsc::sync_channel(1);
        assert!(commands.try_send(Request::Refresh).is_ok());
        let mut handoff = DialHandoff {
            commands: commands.clone(),
            observed: Arc::clone(&observed),
            pending: Some(Request::Dial("1842".into(), Arc::clone(&cancel), result)),
        };
        assert_eq!(handoff.submit(), Ok(false));
        assert!(matches!(owner.try_recv(), Ok(Request::Refresh)));
        assert_eq!(handoff.submit(), Ok(true));
        let Request::Dial(number, flag, result) = owner.try_recv().unwrap() else {
            panic!("expected dial")
        };
        assert_eq!(number, "1842");
        assert!(Arc::ptr_eq(&flag, &cancel));
        assert!(result.send(DialReply::Final(Err(Error::Provider))).is_ok());
        assert!(matches!(
            response.try_recv(),
            Ok(DialReply::Final(Err(Error::Provider)))
        ));
        let (result, _response) = mpsc::sync_channel(1);
        let mut stale = DialHandoff {
            commands,
            observed: Arc::clone(&observed),
            pending: Some(Request::Dial("1843".into(), flag, result)),
        };
        observed.store(3, Ordering::Release);
        assert_eq!(stale.submit(), Err(Error::Retired));
        assert!(matches!(owner.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}
