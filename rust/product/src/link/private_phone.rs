//! Private call transport on its native registered executor. Only its exact
//! reply queue reaches this encoder. Authentication and command execution own
//! the other endpoints on ordinary product control.
use super::{dispatcher::PeerWake, outbound::Outbound, private_radio::Radio};
use crate::{Error, phone::PrivatePhoneReply, services::PeerIo};
use rpt_advanced_core::{audio::{AudioSpan, LinkAudioProducer, LinkAudioQueue, PlaybackOutcome}, link::PeerReply};
use rtrb::{Consumer, Producer, RingBuffer};
use std::{ffi::c_void, panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::Instant};

#[path = "private_receive.rs"]
mod receive;
use receive::{Digit, Receive};
use std::sync::atomic::AtomicU64;

pub(crate) struct Control {
    pub(crate) rate:u32,
    pub(crate) redirect:Arc<Mutex<Option<Radio>>>,
    replies: Producer<PrivatePhoneReply>,
    digits: Consumer<Digit>,
    last_processed: u64,
    settled: Arc<AtomicU64>,
    pub(crate) wake: PeerWake,
    stop: Arc<AtomicBool>,
}
impl Control {
    pub(crate) fn send(&mut self, reply: PrivatePhoneReply) -> Result<(), Error> {
        self.replies.push(reply).map_err(|_| Error::Allocation)?;
        self.wake.notify();
        Ok(())
    }
    pub(crate) fn collection_ended(&self) {
        if self.settled.swap(self.last_processed, Ordering::AcqRel) != self.last_processed {
            self.wake.notify();
        }
    }
    pub(crate) fn pending_digits(&self) -> usize { self.digits.slots() }
    pub(crate) fn digit(&mut self) -> Option<(char, u64)> {
        let digit = self.digits.pop().ok()?;
        self.last_processed = digit.revision;
        Some((digit.symbol, digit.received))
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.wake.notify();
        #[cfg(test)]
        if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
            eprintln!("private PHONE retirement: control stop published and owner notified");
        }
    }
}

pub(super) struct Session {
    pub(super) io: PeerIo,
    pub(super) reply: PeerReply,
    pub(crate) wake: PeerWake,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) epoch: Instant,
    redirect:Arc<Mutex<Option<Radio>>>,
    radio:Option<Radio>,
    incoming: Consumer<PrivatePhoneReply>,
    current: Option<PrivatePhoneReply>,
    receive: Receive,
    output: LinkAudioProducer,
    outbound: Outbound,
    samples: [f32; 960],
    next_position: u64,
    next_ns: u64,
}
impl Session {
    pub(super) fn new(io: PeerIo, reply: PeerReply, epoch: Instant, capacity: usize) -> Result<(Self, Control), Error> {
        if io.async_owner().is_none() || !io.source_media() || reply.phone_source().is_none() || capacity == 0 {
            return Err(Error::Admission);
        }
        let (output, input) = LinkAudioQueue::new(960, 1).map_err(|_| Error::Allocation)?.into_endpoints();
        let outbound = Outbound::private(io.rate(), input, reply.clone())?;
        let (replies, incoming) = RingBuffer::new(capacity);
        let (digits, received) = RingBuffer::new(64);
        let settled = Arc::new(AtomicU64::new(0));
        let receive = Receive::new(io.rate(), io.phone_digits().ok_or(Error::Admission)?, digits, Arc::clone(&settled))?;
        let stop = Arc::new(AtomicBool::new(false));
        let wake = PeerWake::default();
        let redirect=Arc::new(Mutex::new(None));
        let control = Control { rate:io.rate(), redirect:Arc::clone(&redirect),replies, digits: received, last_processed: 0, settled, wake: wake.clone(), stop: Arc::clone(&stop) };
        Ok((Self { io, reply, wake, stop, epoch, incoming, redirect, radio:None, current: None, receive, output, outbound,
            samples: [0.; 960], next_position: 0, next_ns: 0 }, control))
    }
    fn step(&mut self) -> Result<(), Error> {
        if self.stop.load(Ordering::Acquire) || self.reply.ended() { return Err(Error::Hangup); }
        let now_ms = self.epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if let Some(new)=self.redirect.lock().map_err(|_|Error::Operation)?.take(){
            new.activate();
            self.radio=Some(new);
        }
        self.receive.settle(self.radio.as_mut());
        let now=self.io.private_now()?;
        let radio=&mut self.radio;
        let receipt=radio.as_mut().map_or(0,|route|route.begin(now));
        let mut failure=None;
        let outgoing = self.reply.outgoing_phone();
        let closed = self.io.read_private(&mut |input| {
            if failure.is_some(){return 0;}
            use crate::services::phone_media::Input;
            let accepted = if outgoing {
                match (radio.as_mut(), input) {
                    (_, Input::Digit(_)) => Ok(1),
                    (Some(radio), input) => radio.input(input, now, receipt),
                    (None, Input::Audio { pcm, .. }) => Ok(pcm.len()),
                    (None, Input::Gap { count, .. }) => Ok(count as usize),
                    (None, _) => Ok(1),
                }
            } else {
                self.receive.input(input, radio.as_mut(), now, receipt, now_ms)
            };
            match accepted {
                Ok(credit) => credit,
                Err(error) => { failure = Some(error); 0 }
            }
        })?;
        if let Some(error)=failure{return Err(error);}
        if closed || self.reply.ended() { return Err(Error::Hangup); }
        if self.current.is_none() { self.current = self.incoming.pop().ok(); }
        let now = self.io.private_now()?;
        if now >= self.next_ns && self.outbound.ready_for_private_input() {
            if let Some(item) = &mut self.current {
                self.outbound.private_stream(None)?;
                let count = item.render(false, &mut self.samples);
                if count != 0 {
                    let source = self.reply.phone_source().ok_or(Error::Admission)?;
                    let span = AudioSpan { generation: source.call_generation(), burst: 0, source_start: self.next_position,
                        stream: source.ingress(), born_ns: Some(self.io.private_now()?) };
                    if self.output.write(span, &self.samples[..count]) != 0 {
                        #[cfg(test)]
                        if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                            eprintln!("private PHONE write rejected: private prompt queue");
                        }
                        return Err(Error::Write);
                    }
                    self.next_position = self.next_position.checked_add(count as u64).ok_or(Error::Write)?;
                    self.next_ns = now.saturating_add((count as u64) * 1_000_000_000 / 48_000);
                } else if item.outcome() == PlaybackOutcome::Active {
                    self.next_ns = now.saturating_add(20_000_000);
                }
                if item.outcome() != PlaybackOutcome::Active { self.current = None; }
            }
        }
        if self.current.is_none() && self.outbound.ready_for_private_input() {
            if let Some(radio)=&mut self.radio {
                if let Some(mut item)=radio.outgoing.read_span(&mut self.samples) {
                    let host_now=u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    // RF birth is measured from the Engine's Instant. Convert its actual age
                    // to this call's checked native clock without renewing a buffered frame.
                    let age=item.span.born_ns.and_then(|born|host_now.checked_sub(born));
                    item.span.born_ns=age.and_then(|age|now.checked_sub(age));
                    if let Some(clock) = radio.clock.as_ref().filter(|clock| clock.stream() == item.span.stream) {
                        if self.reply.phone_media_allowed() && item.span.born_ns.is_some() {
                            self.outbound.private_stream(Some(clock))?;
                            if self.output.write(item.span, &self.samples[..item.frames]) != 0 {
                                #[cfg(test)]
                                if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                                    eprintln!("private PHONE write rejected: attached programme queue");
                                }
                                return Err(Error::Write);
                            }
                        }
                    }
                }
            }
        }
        let send = self.outbound.step(&mut self.io, now_ms)?;
        let render = self.current.as_ref().map(|_| self.next_ns);
        let now = self.io.private_now()?;
        self.io.set_deadline(send.into_iter().chain(render).chain(self.radio.as_ref().and_then(|r|r.deadline(now))).min().map_or(u64::MAX, |at| at.saturating_sub(now)))
    }
}
pub(super) unsafe extern "C" fn step(context: *mut c_void) -> i32 {
    let Some(owner) = (unsafe { context.cast::<Session>().as_mut() }) else { return -1 };
    let result = catch_unwind(AssertUnwindSafe(|| owner.step()));
    if matches!(result, Ok(Ok(()))) { return 0; }
    #[cfg(test)]
    if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
        match result {
            Ok(Err(error)) => eprintln!("private PHONE callback terminal category {error:?}"),
            Err(_) => eprintln!("private PHONE callback terminal category panic"),
            Ok(Ok(())) => unreachable!(),
        }
    }
    owner.reply.retire();
    owner.stop.store(true, Ordering::Release);
    -1
}
