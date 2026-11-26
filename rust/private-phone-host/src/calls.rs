//! Unique SIP leases used by either product frontend.
use crate::{
    Error, JitterProvider, abi,
    accounts::Route,
    sip::{
        call_media::Media,
        media::MediaError,
        registration::{self, calls},
    },
};
use crate::{
    phone::Protocol,
    sip_provider::{Provider, Status},
};
use std::{collections::BTreeMap, ffi::c_void, net::SocketAddr, sync::Arc};

/// Actual Sofia reactor and its most recently requested registration set.
pub struct SipPhones {
    worker: registration::Worker,
    routes: BTreeMap<String, Arc<Route>>,
    jitter: Arc<JitterProvider>,
}

impl SipPhones {
    /// Start one serial reactor for this frontend. No registrar is contacted here.
    pub fn new(provider: Arc<Provider>, jitter: Arc<JitterProvider>) -> Result<Self, Error> {
        if !provider.can_originate() {
            return Err(Error::Provider);
        }
        let worker = registration::Worker::from_provider(provider).map_err(|_| Error::Provider)?;
        Ok(Self {
            worker,
            jitter,
            routes: BTreeMap::new(),
        })
    }

    /// Submit a resolved selection on the frontend's account worker. Confirmation
    /// belongs to observe(), after native REGISTER and all prior withdrawals.
    pub fn select(&mut self, routes: &[Arc<Route>]) -> Result<(), Error> {
        let mut next = BTreeMap::new();
        for route in routes {
            let id = route.account.account_id().to_string();
            if route.account.protocol() != Protocol::Sip {
                return Err(Error::Input);
            }
            let retained = self
                .routes
                .get(&id)
                .filter(|old| old.generation() == route.generation());
            let value = match retained {
                Some(old) if old.account.same_registration(&route.account) => old,
                Some(_) => return Err(Error::Input),
                None => route,
            };
            if next.insert(id, Arc::clone(value)).is_some() {
                return Err(Error::Input);
            }
        }
        self.worker
            .apply(routes.to_vec())
            .map_err(|_| Error::Provider)?;
        self.routes = next;
        Ok(())
    }

    /// Snapshot the native registrar state. Empty withdrawal is confirmed only
    /// once the reactor reports every prior account and callback retired.
    pub fn observe(&self) -> Result<(bool, Vec<(String, Status)>), Error> {
        self.worker.observe().map_err(|_| Error::Provider)
    }

    /// Take a genuine incoming receipt and prepare its media. The reactor
    /// validates the numeric source and live registration again on this turn.
    pub fn incoming(&self) -> Result<Option<PrivateCall>, Error> {
        let Some(receipt) = self.worker.incoming() else {
            return Ok(None);
        };
        let key = receipt.account.account.account_id().to_string();
        if !self
            .routes
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(&receipt.account, current))
        {
            return Err(Error::Retired);
        }
        receipt
            .prepare(Arc::clone(&self.jitter))
            .map(|handle| Some(PrivateCall(Transport::Sip(SipCall { handle }))))
            .map_err(|_| Error::Retired)
    }

    /// Start one outgoing SIP leg against the exact account, bound port and generation.
    pub fn originate(
        &self,
        port: &str,
        account: &str,
        generation: u64,
        destination: &str,
    ) -> Result<Outgoing, Error> {
        self.originate_with_progress(port, account, generation, destination, Arc::default())
    }

    pub(crate) fn originate_with_progress(
        &self,
        port: &str,
        account: &str,
        generation: u64,
        destination: &str,
        progress: Arc<crate::progress::Progress>,
    ) -> Result<Outgoing, Error> {
        let route = self.routes.get(account).ok_or(Error::Retired)?;
        if route.account.port_id().to_string() != port || route.generation() != generation {
            return Err(Error::Retired);
        }
        if destination.is_empty()
            || destination.len() > 64
            || !destination
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'*' | b'#' | b'+'))
        {
            return Err(Error::Input);
        }
        let uri = route.account.sip_uri(Some(destination));
        self.worker
            .client()
            .originate(route, port, uri, Arc::clone(&self.jitter), progress)
            .map(Outgoing)
            .map_err(|_| Error::Provider)
    }

    /// Join the native reactor only after actual callback and account retirement.
    pub fn stop(&mut self) -> Result<(), Error> {
        self.worker.stop().map_err(|_| Error::Provider)?;
        self.routes.clear();
        Ok(())
    }
}

/// A pending outgoing call; dropping the claim cancels just this native attempt.
pub struct Outgoing(calls::Dial);
impl Outgoing {
    /// A final answered call is the only successful handoff.
    pub fn poll(&mut self) -> Result<Option<PrivateCall>, Error> {
        self.0
            .try_complete()
            .map(|result| result.map(|handle| PrivateCall(Transport::Sip(SipCall { handle }))))
            .map_err(|_| Error::Retired)
    }
}

/// One owned private call. Its port, account and native identities never change.
struct SipCall {
    handle: calls::Handle,
}
impl SipCall {
    /// Registered controller port, explicit account UUID and opaque call token.
    pub fn identity(&self) -> (&str, &str, &str) {
        let shared = &self.handle.shared;
        (&shared.local, &shared.account, &shared.remote)
    }
    /// Actual provider registration generation and unique call generation.
    pub fn generations(&self) -> (u64, u64) {
        self.handle.shared.generations()
    }
    /// Numeric transport source observed by Sofia.
    pub fn source(&self) -> SocketAddr {
        self.handle.shared.source
    }
    /// Native PCM frequency of the negotiated codec.
    pub fn rate(&self) -> u32 {
        self.handle.shared.pcm_rate
    }
    /// Current nonterminal provider state, including a pending incoming dialog.
    pub fn live(&self) -> bool {
        self.handle.shared.live()
    }
    /// True only after the final native dialog transition.
    pub fn established(&self) -> bool {
        self.handle.shared.established()
    }

    /// Publish the real product deadline on the existing serialized reactor.
    pub fn set_deadline(&self, after: u64) -> Result<(), Error> {
        self.handle
            .shared
            .set_deadline(after)
            .map_err(|_| Error::Retired)
    }
    /// Capture the right to answer before transferring the handle to the product.
    /// An answer must then follow successful admission on this exact handle.
    pub fn answer_permit(&self) -> Answer {
        Answer(AnswerOwner::Sip(Arc::clone(&self.handle.shared)))
    }

    fn media<T>(&self, work: impl FnOnce(&mut Media) -> Result<T, MediaError>) -> Result<T, Error> {
        let shared = &self.handle.shared;
        if !shared.live() {
            return Err(Error::Retired);
        }
        let result = shared
            .media
            .lock()
            .map_err(|_| MediaError::Owner)
            .and_then(|mut media| work(&mut media));
        result.map_err(|_| {
            shared.end();
            Error::Retired
        })
    }

    /// # Safety
    /// The sink has the advertised readable extent and its callbacks and context
    /// are valid for this synchronous call. Only the serial product owner may read.
    pub unsafe fn read(
        &self,
        completion: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        result: &mut abi::rptadv_peer_source_result_v1,
    ) -> Result<(), Error> {
        if !self.handle.shared.live() {
            result.progress = 0;
            result.activity = 0;
            result.finish_pending = 0;
            result.state = 3;
            result.represented_node_number = 0;
            return Ok(());
        }
        self.media(|media| unsafe { media.read(completion, sink, result) })
    }

    /// Encode on this call's retained native codec context.
    pub fn prepare(
        &self,
        pcm: &[f32],
        bytes: &mut [u8],
        flush: u32,
        result: &mut abi::rptadv_codec_encode_result_v1,
    ) -> Result<i32, Error> {
        self.media(|media| Ok(media.prepare(pcm, bytes, flush, result)))
    }

    /// Send a complete negotiated unit with its original native deadline.
    pub fn send(
        &self,
        bytes: &[u8],
        frames: u32,
        timestamp_ms: u32,
        deadline_ms: u32,
    ) -> Result<i32, Error> {
        self.media(|media| {
            if frames != media.packet_frames() || bytes.len() != media.packet_bytes() {
                return Ok(-1);
            }
            media.send(bytes, timestamp_ms, deadline_ms)
        })
    }

    /// Reset the transmit codec and retire its old output generation.
    pub fn reset_encoder(&self) -> Result<(), Error> {
        self.media(Media::reset)
    }
    /// Actual clock used by the elapsed wire timestamp and deadline.
    pub fn elapsed_ms(&self) -> Result<u32, Error> {
        self.media(|media| Ok(media.elapsed_ms()))
    }
    /// Send an explicitly authorized command digit on its negotiated event payload.
    pub fn send_digit(&self, digit: u8) -> Result<i32, Error> {
        self.media(|media| Ok(media.send_digit(digit)))
    }

    /// # Safety
    /// Context remains live through confirmed detach. The registered callback
    /// obeys the serial product peer step contract and never unwinds across C.
    pub unsafe fn attach(
        &self,
        step: unsafe extern "C" fn(*mut c_void) -> i32,
        context: *mut c_void,
    ) -> Result<Registration, Error> {
        self.handle
            .attach(step, context)
            .map(|owner| Registration(Callback::Sip(owner)))
            .map_err(|_| Error::Retired)
    }
}

/// A product call keeps its actual protocol owner through its last callback.
pub struct PrivateCall(Transport);
enum Transport {
    Sip(SipCall),
    Iax(Arc<crate::iax::Call>),
}
impl PrivateCall {
    pub(crate) fn iax(call: Arc<crate::iax::Call>) -> Self {
        Self(Transport::Iax(call))
    }
    /// Registered port, account and opaque call ID.
    pub fn identity(&self) -> (&str, &str, &str) {
        match &self.0 {
            Transport::Sip(c) => c.identity(),
            Transport::Iax(c) => c.identity(),
        }
    }
    /// Exact registration and call generations observed by the protocol.
    pub fn generations(&self) -> (u64, u64) {
        match &self.0 {
            Transport::Sip(c) => c.generations(),
            Transport::Iax(c) => c.generations(),
        }
    }
    /// Transport-authenticated numeric source.
    pub fn source(&self) -> SocketAddr {
        match &self.0 {
            Transport::Sip(c) => c.source(),
            Transport::Iax(c) => c.source(),
        }
    }
    /// Actual negotiated PCM rate.
    pub fn rate(&self) -> u32 {
        match &self.0 {
            Transport::Sip(c) => c.rate(),
            Transport::Iax(_) => 8000,
        }
    }
    /// Whether the protocol has ended this call.
    pub fn live(&self) -> bool {
        match &self.0 {
            Transport::Sip(c) => c.live(),
            Transport::Iax(c) => c.live(),
        }
    }
    /// Completed native answer. IAX transfers its peer after the provider handshake.
    pub fn established(&self) -> bool {
        match &self.0 {
            Transport::Sip(c) => c.established(),
            Transport::Iax(c) => c.live(),
        }
    }
    /// Native event method negotiated for this dialog.
    pub fn digit_capabilities(&self) -> u32 {
        match &self.0 {
            Transport::Sip(c) => c.handle.shared.digit_capabilities,
            Transport::Iax(_) => 0,
        }
    }
    /// Set the next native serial callback deadline.
    pub fn set_deadline(&self, after: u64) -> Result<(), Error> {
        match &self.0 {
            Transport::Sip(c) => c.set_deadline(after),
            Transport::Iax(c) => c.deadline(after),
        }
    }
    /// Retain the signaling decision across the product's incoming transfer.
    pub fn answer_permit(&self) -> Answer {
        match &self.0 {
            Transport::Sip(c) => c.answer_permit(),
            Transport::Iax(c) => Answer(AnswerOwner::Iax(Arc::downgrade(c))),
        }
    }
    /// # Safety
    /// The sink and its callbacks remain valid throughout this serial turn.
    pub unsafe fn read(
        &self,
        completion: u32,
        sink: *const abi::rptadv_peer_source_sink_v1,
        out: &mut abi::rptadv_peer_source_result_v1,
    ) -> Result<(), Error> {
        match &self.0 {
            Transport::Sip(c) => unsafe { c.read(completion, sink, out) },
            Transport::Iax(c) => unsafe { c.read(completion, sink, out) },
        }
    }
    /// Feed the negotiated provider encoder.
    pub fn prepare(
        &self,
        pcm: &[f32],
        bytes: &mut [u8],
        flush: u32,
        out: &mut abi::rptadv_codec_encode_result_v1,
    ) -> Result<i32, Error> {
        match &self.0 {
            Transport::Sip(c) => c.prepare(pcm, bytes, flush, out),
            Transport::Iax(c) => c.prepare(pcm, bytes, flush, out),
        }
    }
    /// Send only the prepared frame and its original age deadline.
    pub fn send(
        &self,
        bytes: &[u8],
        frames: u32,
        timestamp: u32,
        deadline: u32,
    ) -> Result<i32, Error> {
        match &self.0 {
            Transport::Sip(c) => c.send(bytes, frames, timestamp, deadline),
            Transport::Iax(c) => c.send_prepared(bytes, frames, timestamp, deadline),
        }
    }
    /// Retire the native transmit history.
    pub fn reset_encoder(&self) -> Result<(), Error> {
        match &self.0 {
            Transport::Sip(c) => c.reset_encoder(),
            Transport::Iax(c) => c.reset(),
        }
    }
    /// Read the actual native call clock.
    pub fn elapsed_ms(&self) -> Result<u32, Error> {
        match &self.0 {
            Transport::Sip(c) => c.elapsed_ms(),
            Transport::Iax(c) => c.elapsed(),
        }
    }
    /// Submit an explicit outgoing completed digit.
    pub fn send_digit(&self, digit: u8) -> Result<i32, Error> {
        match &self.0 {
            Transport::Sip(c) => c.send_digit(digit),
            Transport::Iax(c) => c.digit(digit),
        }
    }
    /// # Safety
    /// Context is stable through confirmed native callback detach.
    pub unsafe fn attach(
        &self,
        step: unsafe extern "C" fn(*mut c_void) -> i32,
        context: *mut c_void,
    ) -> Result<Registration, Error> {
        match &self.0 {
            Transport::Sip(c) => unsafe { c.attach(step, context) },
            Transport::Iax(c) => {
                unsafe { c.attach(step, context) }.map(|r| Registration(Callback::Iax(r)))
            }
        }
    }
}
enum AnswerOwner {
    Sip(Arc<calls::Shared>),
    Iax(std::sync::Weak<crate::iax::Call>),
}
/// The exact admitted native signaling owner.
pub struct Answer(AnswerOwner);
impl Answer {
    /// Sofia confirms its pending INVITE here. IAX has already transferred its authenticated answered peer.
    pub fn answer(self) -> Result<(), Error> {
        match self.0 {
            AnswerOwner::Sip(s) => s.answer().map_err(|_| {
                s.end();
                Error::Retired
            }),
            AnswerOwner::Iax(s) => s
                .upgrade()
                .filter(|call| call.live())
                .map(|_| ())
                .ok_or(Error::Retired),
        }
    }
}
enum Callback {
    Sip(calls::Registration),
    Iax(crate::iax::Registration),
}
/// Native callback lease. Drop waits for the final callback.
pub struct Registration(Callback);
impl Registration {
    /// Initiate retirement on this exact serial executor.
    pub fn request_stop(&self) {
        match &self.0 {
            Callback::Sip(r) => r.0.stop(),
            Callback::Iax(r) => r.stop(),
        }
    }
    /// Notify this executor of newly available work.
    pub fn wake(&self) {
        match &self.0 {
            Callback::Sip(r) => r.0.wake(),
            Callback::Iax(r) => r.wake(),
        }
    }
}

#[cfg(test)]
#[path = "calls/active_sip_tests.rs"]
mod active_sip_tests;

#[cfg(test)]
#[path = "calls/source_capture.rs"]
mod source_capture;

#[cfg(test)]
#[path = "calls/active_iax_tests.rs"]
mod active_iax_tests;
