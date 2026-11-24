//! Dormant and reversible membership for the standalone ASL listener owner.

use super::{IaxClient, IaxError, IaxPeer, IaxServer};
use crate::foreground::{callback_text, drain_iax_datagrams, report_iax_poll};
use std::ffi::c_void;
use std::{collections::{BTreeMap, BTreeSet}, net::{Ipv4Addr, SocketAddr}, num::{NonZeroU16, NonZeroU64}};

/// Exact local identity supplied by the authenticated product selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Authority {
    /// Registered local port UUID, independent of the operator's display key.
    pub station: [u8; 16],
    /// Registered account UUID. Deleting and recreating an account changes this.
    pub account: [u8; 16],
    /// Generation of this account authority, stable for retained peer media.
    pub generation: NonZeroU64,
    /// Numeric IAX called/calling number on the wire.
    pub wire: NonZeroU64,
    /// UDP port shared by accounts at this local endpoint.
    pub port: NonZeroU16,
}

type Groups = BTreeMap<NonZeroU16, BTreeMap<NonZeroU64, Authority>>;

enum Change {
    Added(NonZeroU16),
    Membership(NonZeroU16),
}

struct Transition {
    ticket: NonZeroU64,
    desired: Groups,
    started: bool,
    applied: bool,
    published: bool,
    touched: Vec<Change>,
}

/// Single foreground owner of server handles. Existing sockets remain owned through reversal.
#[derive(Default)]
pub struct Listeners {
    serial: Option<NonZeroU64>,
    last_ticket: Option<NonZeroU64>,
    active: Groups,
    sockets: BTreeMap<NonZeroU16, IaxServer>,
    pending: Option<Transition>,
}

impl Listeners {
    /// Current confirmed serial, independent of a pending preparation.
    pub fn basis(&self) -> Option<NonZeroU64> { self.serial }

    /// Stage only selected, enabled accounts; reserve no socket or external resource.
    pub fn prepare(&mut self, basis: Option<NonZeroU64>, ticket: NonZeroU64, accounts: &[Authority]) -> Result<(), IaxError> {
        if self.pending.is_some() || basis != self.serial || self.last_ticket.is_some_and(|old| ticket <= old) {
            return Err(IaxError::Operation);
        }
        let desired = groups(accounts)?;
        self.pending = Some(Transition { ticket, desired, started: false, applied: false, published: false, touched: Vec::new() });
        self.last_ticket = Some(ticket);
        Ok(())
    }

    /// Check duplicate wire numbers and registry bindings while the caller retains its stage.
    pub(crate) fn validate(accounts: &[Authority]) -> Result<(), IaxError> {
        groups(accounts).map(|_| ())
    }
}

fn groups(accounts: &[Authority]) -> Result<Groups, IaxError> {
        let mut desired = Groups::new();
        let mut wires = BTreeSet::new();
        let mut stations = BTreeSet::new();
        let mut bindings = BTreeSet::new();
        for &account in accounts {
            if account.station == [0; 16] || account.account == [0; 16]
                || !wires.insert(account.wire) || !stations.insert(account.station) || !bindings.insert(account.account) {
                return Err(IaxError::InvalidParameters);
            }
            desired.entry(account.port).or_default().insert(account.wire, account);
        }
        Ok(desired)
}

impl Listeners {

    /// Bind new ports and change retained memberships. Keep the previous membership
    /// and all fallible updates until the aggregate releases or reverses this attempt.
    pub fn apply(&mut self, ticket: NonZeroU64, client: &IaxClient,
        mut enable: impl FnMut(&mut IaxServer) -> Result<(), IaxError>) -> Result<(), IaxError> {
        let stage = self.pending.as_mut().filter(|s| s.ticket == ticket).ok_or(IaxError::Operation)?;
        if stage.applied { return Ok(()); }
        if stage.started { return Err(IaxError::Operation); }
        stage.started = true;
        for (&port, members) in &stage.desired {
            let nodes: Vec<_> = members.keys().map(ToString::to_string).collect();
            if let Some(server) = self.sockets.get_mut(&port) {
                if !self.active[&port].keys().eq(members.keys()) {
                    // A failed provider update can leave its result uncertain. Reverse it too.
                    stage.touched.push(Change::Membership(port));
                    server.set_local_nodes(&nodes)?;
                }
            } else {
                let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port.get()));
                let mut server = IaxServer::bind(client, addr, &nodes)?;
                enable(&mut server)?;
                self.sockets.insert(port, server);
                stage.touched.push(Change::Added(port));
            }
        }
        stage.applied = true;
        Ok(())
    }

    /// Expose the candidate only at the aggregate confirmation barrier.
    pub fn publish(&mut self, ticket: NonZeroU64) -> Result<(), IaxError> {
        let stage = self.pending.as_mut().filter(|s| s.ticket == ticket && s.applied).ok_or(IaxError::Operation)?;
        stage.published = true;
        Ok(())
    }

    /// Retire withdrawn listeners once the aggregate commits and affected peer owners have drained.
    pub fn finish(&mut self, ticket: NonZeroU64) -> Result<(), IaxError> {
        if self.pending.is_none() && self.serial == Some(ticket) { return Ok(()); }
        if !self.pending.as_ref().is_some_and(|s| s.ticket == ticket && s.published) {
            return Err(IaxError::Operation);
        }
        let stage = self.pending.take().expect("checked attempt");
        self.sockets.retain(|port, _| stage.desired.contains_key(port));
        self.active = stage.desired;
        self.serial = Some(ticket);
        Ok(())
    }

    /// Abort dormant selection or reverse actual mutations last-first. A failed
    /// reverse keeps its remaining journal, handles and admission fence for retry.
    pub fn reverse(&mut self, ticket: NonZeroU64) -> Result<(), IaxError> {
        if self.pending.is_none() && self.last_ticket == Some(ticket) && self.serial != Some(ticket) { return Ok(()); }
        let stage = self.pending.as_mut().filter(|s| s.ticket == ticket).ok_or(IaxError::Operation)?;
        stage.published = false;
        stage.applied = false;
        while let Some(change) = stage.touched.last() {
            match change {
                Change::Added(port) => { self.sockets.remove(port); }
                Change::Membership(port) => {
                    let nodes: Vec<_> = self.active[port].keys().map(ToString::to_string).collect();
                    self.sockets.get_mut(port).ok_or(IaxError::Operation)?.set_local_nodes(&nodes)?;
                }
            }
            stage.touched.pop();
        }
        self.pending = None;
        Ok(())
    }

    /// Acquire the exact authority for a callback. Between apply and publish only
    /// accounts retained identically in both selections can admit new peers.
    pub fn authority(&self, port: NonZeroU16, wire: NonZeroU64) -> Option<Authority> {
        visible(&self.active, self.pending.as_ref(), port, wire)
    }

    /// Recheck the captured UUIDs and generation after directory work or an outbound dial.
    pub fn current(&self, captured: Authority) -> bool {
        self.authority(captured.port, captured.wire) == Some(captured)
    }

    /// Poll bounded NEW work on the frontend owner. Directory resolution runs on
    /// this non-media caller; the product rechecks the captured authority and ACL at admission.
    pub fn poll<A: Incoming>(&mut self, now_seconds: u32, admission: &mut A) -> Result<(), IaxError> {
        let mut first_error = None;
        for (&port, server) in &mut self.sockets {
            if let Err(error) = drain_iax_datagrams(|| admission.individual(|admission| {
                let mut context = Poll {
                    active: &self.active, stage: self.pending.as_ref(), port,
                    client: server._owner.clone(), admission,
                };
                unsafe { server.poll(now_seconds, Some(authorize::<A>), Some(accept::<A>), std::ptr::from_mut(&mut context).cast()) }
            })) {
                report_iax_poll(port.get(), Err(error));
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn visible(active: &Groups, stage: Option<&Transition>, port: NonZeroU16, wire: NonZeroU64) -> Option<Authority> {
    let old = active.get(&port).and_then(|group| group.get(&wire)).copied();
    if let Some(s) = stage.filter(|s| s.started) {
        let new = s.desired.get(&port).and_then(|group| group.get(&wire)).copied();
        if s.published || old == new { new } else { None }
    } else { old }
}

/// Admission operations on the non-media foreground owner. Both calls receive the
/// exact authority selected for this listener, independent of the native wire labels.
pub trait Incoming {
    /// Scope the callback state to the processing of one provider datagram.
    fn individual(&mut self, poll: impl FnOnce(&mut Self) -> Result<bool, IaxError>) -> Result<bool, IaxError>
    where Self: Sized { poll(self) }
    /// Resolve the numeric peer and authenticate its source against current admission.
    fn authorize(&mut self, local: Authority, remote: &str, source: &str) -> bool;
    /// Recheck captured ownership and attach the uniquely transferred peer, or drop it.
    fn accept(&mut self, local: Authority, remote: &str, source: &str, peer: IaxPeer);
}

/// A completed outgoing call still requires the admitting owner's current identity.
pub struct DialedPeer {
    local: Authority,
    peer: IaxPeer,
}

impl DialedPeer {
    /// Dial an already verified destination off the control owner. Public numeric
    /// destinations have no authority to use the configured HTTPS registrar secret.
    pub fn dial(client: IaxClient, local: Authority, remote: &str, endpoint: SocketAddr,
                timeout_ms: u32) -> Result<Self, IaxError> {
        let peer = client.dial(endpoint, &local.wire.to_string(), remote, "", timeout_ms)?;
        Ok(Self { local, peer })
    }

    /// Recheck the captured identity and apply the actual negotiated policy before handing
    /// this completed peer to the product's normal asynchronous media attachment.
    pub fn take(mut self, listeners: &Listeners, host: &crate::host_services::HostServicesOwner,
                delays: super::playout::Delays) -> Result<(IaxPeer, super::playout::PlayoutControl), IaxError> {
        if !listeners.current(self.local) { return Err(IaxError::Operation); }
        let control = host.prepare_managed_playout(&mut self.peer, delays)?;
        Ok((self.peer, control))
    }
}

struct Poll<'a, A> {
    active: &'a Groups,
    stage: Option<&'a Transition>,
    port: NonZeroU16,
    client: IaxClient,
    admission: &'a mut A,
}

impl<A> Poll<'_, A> {
    unsafe fn identity<'a>(&self, local: *const u8, llen: usize, remote: *const u8, rlen: usize,
                           source: *const u8, slen: usize) -> Option<(Authority, &'a str, &'a str)> {
        // SAFETY: the matched server lends all three spans for this synchronous callback.
        let (local, remote, source) = unsafe {
            (callback_text(local, llen)?, callback_text(remote, rlen)?, callback_text(source, slen)?)
        };
        let wire: NonZeroU64 = local.parse().ok()?;
        if wire.to_string() != local || remote.is_empty() || !remote.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some((visible(self.active, self.stage, self.port, wire)?, remote, source))
    }
}

unsafe extern "C" fn authorize<A: Incoming>(ctx: *mut c_void, l: *const u8, ll: usize,
    r: *const u8, rl: usize, s: *const u8, sl: usize) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: poll installs a live exclusive context and provider-borrowed spans.
        let ctx = unsafe { &mut *ctx.cast::<Poll<'_, A>>() };
        let Some((a, remote, source)) = (unsafe { ctx.identity(l,ll,r,rl,s,sl) }) else { return -1 };
        i32::from(!ctx.admission.authorize(a, remote, source))
    })).unwrap_or(-1)
}

unsafe extern "C" fn accept<A: Incoming>(ctx: *mut c_void, l: *const u8, ll: usize,
    r: *const u8, rl: usize, s: *const u8, sl: usize, raw: *mut c_void) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: this server transfers its matched, uniquely owned peer into the callback.
        let ctx = unsafe { &mut *ctx.cast::<Poll<'_, A>>() };
        let Some((a, remote, source)) = (unsafe { ctx.identity(l,ll,r,rl,s,sl) }) else { return -1 };
        let Ok(peer) = (unsafe { ctx.client.clone().adopt_inbound(raw) }) else { return -1 };
        // The C contract requires zero after we take the raw peer, including
        // on product refusal or unwind: its Rust owner will release it once.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ctx.admission.accept(a, remote, source, peer)
        }));
        0
    })).unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::UdpSocket, path::Path};

    fn number(value: u64) -> NonZeroU64 { value.try_into().unwrap() }
    fn account(value: u8, port: NonZeroU16) -> Authority {
        Authority { station: [value; 16], account: [value + 10; 16], generation: number(1),
                    wire: number(716700 + u64::from(value)), port }
    }

    #[test]
    fn polling_advances_an_independent_listener_after_a_group_error() {
        use super::super::{ServerAccept, ServerAuthorize, tests::{client_with_destroy, server_descriptor}};

        unsafe extern "C" fn destroy(_: *mut c_void) {}
        unsafe extern "C" fn first(_: *mut c_void, _: u32, _: ServerAuthorize,
                                   _: ServerAccept, _: *mut c_void) -> i32 { -1 }
        unsafe extern "C" fn second(_: *mut c_void, _: u32, authorize: ServerAuthorize,
                                    accept: ServerAccept, ctx: *mut c_void) -> i32 {
            let (Some(authorize), Some(accept)) = (authorize, accept) else { return -1 };
            let (local, remote, source) = (b"716702", b"123", b"127.0.0.1");
            // Static spans and an ignored fake peer token live through these synchronous calls.
            if unsafe { authorize(ctx, local.as_ptr(), local.len(), remote.as_ptr(), remote.len(),
                                  source.as_ptr(), source.len()) } != 0 { return -1; }
            unsafe { accept(ctx, local.as_ptr(), local.len(), remote.as_ptr(), remote.len(),
                            source.as_ptr(), source.len(), std::ptr::dangling_mut::<c_void>()) }
        }
        #[derive(Default)]
        struct Probe { authorized: Vec<Authority>, accepted: Vec<Authority> }
        impl Incoming for Probe {
            fn authorize(&mut self, local: Authority, _: &str, _: &str) -> bool {
                self.authorized.push(local);
                true
            }
            fn accept(&mut self, local: Authority, _: &str, _: &str, _: IaxPeer) {
                self.accepted.push(local);
            }
        }

        let low = account(1, NonZeroU16::new(1001).unwrap());
        let high = account(2, NonZeroU16::new(1002).unwrap());
        let mut listeners = Listeners::default();
        for local in [low, high] {
            let mut api = server_descriptor();
            api.poll = if local == low { Some(first) } else { Some(second) };
            listeners.sockets.insert(local.port, IaxServer {
                _owner: client_with_destroy(destroy),
                api,
                handle: std::ptr::NonNull::dangling(),
            });
            listeners.active.entry(local.port).or_default().insert(local.wire, local);
        }

        let mut probe = Probe::default();
        for _ in 0..2 {
            assert_eq!(listeners.poll(0, &mut probe), Err(IaxError::Operation));
        }
        assert_eq!(probe.authorized, [high, high]);
        assert_eq!(probe.accepted, [high, high]);
    }

    fn check_wire(listeners: &mut Listeners, client: IaxClient, local: Authority) {
        struct Probe { expected: Authority, accepted: bool }
        impl Incoming for Probe {
            fn authorize(&mut self, local: Authority, remote: &str, source: &str) -> bool {
                local == self.expected && remote == "716799" && source == "127.0.0.1"
            }
            fn accept(&mut self, local: Authority, remote: &str, source: &str, _peer: IaxPeer) {
                self.accepted = self.authorize(local, remote, source);
            }
        }
        let mut probe = Probe { expected: local, accepted: false };
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, local.port.get()));
        let dial = std::thread::spawn(move || {
            client.dial(address, "716799", &local.wire.to_string(), "", 4000).is_ok()
        });
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !probe.accepted && std::time::Instant::now() < until {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
            listeners.poll(now, &mut probe).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(dial.join().unwrap() && probe.accepted);
    }

    #[test]
    fn real_grouped_socket_restores_membership_after_second_bind_refusal() {
        let client = IaxClient::load(Path::new("librptadviax2.so.2")).unwrap();
        let reserve = || UdpSocket::bind("0.0.0.0:0").unwrap();
        let mut sockets = [reserve(), reserve()];
        sockets.sort_by_key(|socket| socket.local_addr().unwrap().port());
        let [first, occupied] = sockets;
        let address = first.local_addr().unwrap();
        let port = address.port().try_into().unwrap();
        let a = account(1, port);
        let b = account(2, port);
        let c = account(3, occupied.local_addr().unwrap().port().try_into().unwrap());
        let mut listeners = Listeners::default();
        listeners.prepare(None, number(1), &[a, b]).unwrap();
        listeners.reverse(number(1)).unwrap();
        assert!(listeners.sockets.is_empty());
        drop(first);
        listeners.prepare(None, number(2), &[a, b]).unwrap();
        listeners.apply(number(2), &client, |_| Ok(())).unwrap();
        assert!(listeners.authority(port, a.wire).is_none());
        listeners.publish(number(2)).unwrap();
        listeners.finish(number(2)).unwrap();
        assert_eq!(listeners.sockets.len(), 1);
        assert!(UdpSocket::bind(address).is_err());

        listeners.prepare(Some(number(2)), number(3), &[a, c]).unwrap();
        assert!(listeners.apply(number(3), &client, |_| Ok(())).is_err());
        assert!(listeners.current(a));
        assert!(!listeners.current(b));
        listeners.reverse(number(3)).unwrap();
        listeners.reverse(number(3)).unwrap();
        assert!(listeners.current(b));
        assert_eq!(listeners.basis(), Some(number(2)));
        check_wire(&mut listeners, client.clone(), b);
        let changed = Authority { account: [99; 16], generation: number(4), ..a };
        listeners.prepare(Some(number(2)), number(4), &[changed]).unwrap();
        listeners.apply(number(4), &client, |_| Ok(())).unwrap();
        listeners.publish(number(4)).unwrap();
        listeners.finish(number(4)).unwrap();
        assert!(!listeners.current(a));
        assert!(listeners.current(changed));
        assert!(UdpSocket::bind(address).is_err());
        listeners.prepare(Some(number(4)), number(5), &[]).unwrap();
        listeners.apply(number(5), &client, |_| Ok(())).unwrap();
        listeners.publish(number(5)).unwrap();
        listeners.finish(number(5)).unwrap();
        drop(UdpSocket::bind(address).unwrap());
    }

    #[test]
    fn negotiated_managed_peer_applies_and_reverses_policy_while_exchanging_audio() {
        use crate::{host_services::HostServicesOwner, iax::{IaxEvent, playout::Delays, tests::send_pcm}};
        use std::{sync::mpsc, thread, time::{Duration, Instant}};
        struct Capture(Authority, Option<IaxPeer>);
        impl Incoming for Capture {
            fn authorize(&mut self, a: Authority, remote: &str, source: &str) -> bool {
                a == self.0 && remote == "716799" && source == "127.0.0.1"
            }
            fn accept(&mut self, _: Authority, _: &str, _: &str, peer: IaxPeer) { self.1 = Some(peer); }
        }
        fn audio(peer: &mut IaxPeer) -> bool {
            let mut buffer = [0.0; 8192];
            matches!(peer.poll(&mut buffer, &mut [0; 1024]), Ok(IaxEvent::Audio(n)) if buffer[..n].iter().any(|s| s.abs() > 0.005))
        }
        fn send(peer: &mut IaxPeer) {
            // One 100 ms source segment, packetized by the negotiated encoder and flushed once.
            let count = (peer.sample_rate_hz() / 10) as usize;
            let speech: Vec<_> = (0..count).map(|i| if i % 32 < 16 { 0.35 } else { -0.35 }).collect();
            send_pcm(peer, &speech);
        }
        let host = HostServicesOwner::new(Vec::new(), None, Default::default()).unwrap();
        let client = host.managed_iax_client().unwrap();
        let temporary = UdpSocket::bind("0.0.0.0:0").unwrap();
        let port = temporary.local_addr().unwrap().port().try_into().unwrap();
        let a = account(1, port);
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port.get()));
        let mut listeners = Listeners::default();
        listeners.prepare(None, number(1), &[a]).unwrap();
        drop(temporary);
        listeners.apply(number(1), &client, |s| host.enable_listener(s)).unwrap();
        listeners.publish(number(1)).unwrap();
        listeners.finish(number(1)).unwrap();
        let (send_gate, start) = mpsc::sync_channel(1);
        let remote = thread::spawn(move || {
            let mut peer = client.dial(address, "716799", &a.wire.to_string(), "", 4000).unwrap();
            start.recv_timeout(Duration::from_secs(5)).unwrap();
            send(&mut peer);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !audio(&mut peer) {
                assert!(Instant::now() < deadline, "no controller-to-peer samples");
                thread::sleep(Duration::from_millis(2));
            }
        });
        let mut capture = Capture(a, None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while capture.1.is_none() {
            assert!(Instant::now() < deadline, "numeric incoming peer not delivered");
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as u32;
            listeners.poll(now, &mut capture).unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        let mut peer = capture.1.take().unwrap();
        let original = Delays::new(20, 40, 100).unwrap();
        let mut edits = host.prepare_managed_playout(&mut peer, original).unwrap();
        send_gate.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !audio(&mut peer) {
            assert!(Instant::now() < deadline, "no peer-to-controller samples");
            thread::sleep(Duration::from_millis(2));
        }
        for tuple in [Delays::new(25, 50, 110).unwrap(), original] {
            let request = edits.request(tuple).unwrap();
            assert_eq!(request.applied(), None);
            let _ = audio(&mut peer);
            assert_eq!(request.applied(), Some(true));
        }
        send(&mut peer);
        remote.join().unwrap();
    }
}
