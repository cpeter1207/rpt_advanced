//! The real private transport's endpoints in one prepared station audio generation.
use super::{phone_speech::SpeechAnalysis,ring::{InboundProducer,MuteSpan},source_clock::SourceClock};
use crate::{Error,services::phone_media::Input};
use rpt_advanced_core::{audio::LinkAudioConsumer,config::resources::profiles::{PhoneLatencyTiming,PhoneVadSettings},link::PeerReply};
use std::sync::{Arc,atomic::{AtomicBool,Ordering}};

pub(crate) struct Radio {
    pub(super) incoming:InboundProducer,
    pub(super) outgoing:LinkAudioConsumer,
    pub(super) clock:Option<Arc<SourceClock>>,
    pub(crate) installed:Arc<AtomicBool>,
    call:PeerReply,
    confirmed:Arc<AtomicBool>,
    activity:Arc<rpt_advanced_core::link::PeerSignals>,
    speech:Option<SpeechAnalysis>,
    timing:PhoneLatencyTiming,
    hang_ns:u64,
    healthy_until:u64,
    voice_until:u64,
    epoch:u64,
    #[cfg(test)]
    recorded_stall:bool,
    fresh_after:Option<u64>,
    control_muted:bool,
    control_start:Option<u64>,
    // A native media epoch may restart its source positions while this call and
    // its radio ring continue. Retain both origins for translating mute spans.
    native_origin: Option<u64>,
    ring_origin: u64,
    retry:Option<(u64,u64,u64,bool)>,
}
impl Radio {
    pub(crate) fn new(incoming:InboundProducer,outgoing:LinkAudioConsumer,activity:Arc<rpt_advanced_core::link::PeerSignals>,vad:PhoneVadSettings,
        timing:PhoneLatencyTiming,confirmed:Arc<AtomicBool>)->Result<Self,Error> {
        let call=activity.reply_target();
        let speech=if vad.enabled {Some(SpeechAnalysis::new(incoming.rate(),4096)?)}else{None};
        let epoch=incoming.epoch();
        Ok(Self{incoming,outgoing,clock:None,installed:Arc::new(AtomicBool::new(false)),call,confirmed,activity,
            speech,timing,hang_ns:u64::from(vad.hang_ms)*1_000_000,healthy_until:0,voice_until:0,
            #[cfg(test)] recorded_stall:false,
            epoch,fresh_after:None,control_muted:false,control_start:None,native_origin:Some(0),ring_origin:0,retry:None})
    }
    pub(crate) fn bind(&mut self,clock:Arc<SourceClock>){self.clock=Some(clock);}
    pub(super) fn begin(&mut self,now:u64)->u64{
        let epoch=self.incoming.epoch();
        // The first granted RF turn establishes an admission floor in the provider clock.
        if !self.confirmed.load(Ordering::Acquire) {
            self.fresh_after=None;
            self.healthy_until=0;
            self.voice_until=0;
        } else if self.fresh_after.is_none() || self.epoch!=epoch{
            self.epoch=epoch;
            if let Some(speech)=&mut self.speech{speech.revoke();}
            self.fresh_after=Some(now);
            self.healthy_until=0;
            self.voice_until=0;
        }
        self.key(now,epoch);
        epoch
    }
    pub(super) fn control(&mut self, held: bool, start: Option<u64>) {
        self.control_muted = held;
        self.control_start = start;
        self.incoming.signals().set_dtmf_muted(held);
        if held {
            self.healthy_until = 0;
            self.voice_until = 0;
            if let Some(speech)=&mut self.speech{speech.revoke();}
            self.incoming.signals().set_radio_keyed_at(false, self.epoch);
            self.activity.set_active(false);
        }
    }
    fn key(&self,now:u64,receipt:u64){
        let active=self.confirmed.load(Ordering::Acquire) && !self.control_muted && self.call.phone_media_allowed() && now<self.healthy_until &&
            (self.speech.is_none() || now<self.voice_until);
        self.incoming.signals().set_radio_keyed_at(active,receipt);
        self.activity.set_active(active);
    }
    fn ring_position(&mut self, source: u64) -> Result<u64, Error> {
        #[cfg(test)]
        if self.native_origin.is_none() && source == 0 && self.ring_origin != 0
            && crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed)
        {
            eprintln!("private PHONE accepted native discontinuity followed by a zero source origin on a nonempty ring timeline");
        }
        let origin = *self.native_origin.get_or_insert(source);
        source.checked_sub(origin)
            .and_then(|offset| self.ring_origin.checked_add(offset))
            .ok_or(Error::InvalidFrame)
    }
    pub(super) fn input(&mut self,input:Input<'_>,now:u64,receipt:u64)->Result<usize,Error>{
        let generation=self.call.phone_source().ok_or(Error::Admission)?.call_generation();
        match input {
            Input::Audio{start,pcm,born_ns,concealed}=>{
                let end=start.checked_add(pcm.len() as u64).ok_or(Error::InvalidFrame)?;
                // A declined suffix keeps the receipt of its first offer across provider reads.
                let (credit_epoch,offer_end,first_allowed)=self.retry.filter(|(next,end,_,_)|*next==start && *end>=start)
                    .map_or((receipt,end,true),|(_,end,old,allowed)|(old,end,allowed));
                // This is the per-turn maximum at the checked private source boundary.
                // Retain and credit the exact prefix even for a larger future offer.
                static SILENCE: [f32; 4096] = [0.; 4096];
                let presented=pcm.len().min((offer_end-start) as usize).min(SILENCE.len());
                if presented==0{return Ok(0);}
                let age_limit=u64::from(self.timing.maximum_latency_ms)*1_000_000;
                let live_birth=born_ns.is_some_and(|born| self.fresh_after.is_some_and(|earliest|born>=earliest) && now.saturating_sub(born)<=age_limit);
                let allowed=first_allowed && !self.control_muted && live_birth && self.call.phone_media_allowed() && credit_epoch==self.epoch && credit_epoch&1==0;
                let ring_start = self.ring_position(start)?;
                let ring_end = ring_start.checked_add(presented as u64).ok_or(Error::InvalidFrame)?;
                // A detector lookback cannot mark samples belonging to an earlier native epoch.
                let lookback = self.control_start.map_or(0, |from| start.saturating_sub(from));
                let mute = [MuteSpan {
                    start: ring_start.saturating_sub(lookback).max(self.ring_origin),
                    end: ring_end,
                }];
                // Source-time mute spans fence emitted coordinates. Keep denied samples
                // out of the RF resampler's interpolation history as well, otherwise
                // the first authorized silent frame can contain pre-grant speech.
                // Native decode, jitter and the speech detector still receive the original.
                let programme = if allowed { &pcm[..presented] } else { &SILENCE[..presented] };
                let accepted=self.incoming.write_received_tagged(programme,generation,ring_start,
                    if allowed {&[]}else{&mute},credit_epoch).map_err(|_|Error::Write)?;
                #[cfg(test)]
                self.record_credit("audio", start, presented, accepted);
                #[cfg(test)]
                if !concealed && crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                    use crate::fixture::{PHONE_ACCEPTED, PHONE_FRESH, PHONE_ORIGINAL, PHONE_PERMITTED};
                    let original = pcm[..presented].iter().filter(|value| value.abs() > 0.02).count();
                    PHONE_ORIGINAL.fetch_add(original, Ordering::Relaxed);
                    if live_birth { PHONE_FRESH.fetch_add(original, Ordering::Relaxed); }
                    if allowed {
                        crate::fixture::PHONE_LIVE_SAMPLES.fetch_add(accepted, Ordering::Relaxed);
                        PHONE_PERMITTED.fetch_add(original, Ordering::Relaxed);
                        PHONE_ACCEPTED.fetch_add(pcm[..accepted].iter().filter(|v| v.abs() > 0.02).count(), Ordering::Relaxed);
                    }
                }
                let next=start+accepted as u64;
                self.retry=(next<offer_end).then_some((next,offer_end,credit_epoch,allowed));
                if accepted!=0{
                    let voice=if concealed{None}else if let Some(detector)=&mut self.speech {
                        detector.admitted_voice(start,&pcm[..accepted],born_ns.filter(|_|allowed))?
                    }else{None};
                    if allowed && !concealed {
                        let born=born_ns.ok_or(Error::InvalidFrame)?;
                        self.healthy_until=born.saturating_add(age_limit);
                        if let Some(voice_birth)=voice {
                            self.voice_until=voice_birth
                                .saturating_add(self.hang_ns).saturating_add(u64::from(self.timing.target_latency_ms)*1_000_000);
                        }
                    }
                    self.key(now,credit_epoch);
                }
                Ok(accepted)
            }
            Input::Gap{start,count}=>{
                start.checked_add(u64::from(count)).ok_or(Error::InvalidFrame)?;
                self.ring_position(start)?.checked_add(u64::from(count)).ok_or(Error::InvalidFrame)?;
                let credit=self.incoming.gap(u64::from(count));
                #[cfg(test)]
                if credit.is_err() && crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                    eprintln!("private PHONE write rejected: native gap");
                }
                let accepted=credit.map_err(|_|Error::Write)? as usize;
                #[cfg(test)]
                if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                    crate::fixture::PHONE_GAP_CREDIT.fetch_add(accepted,Ordering::Relaxed);
                    self.record_credit("gap",start,count as usize,accepted);
                }
                Ok(accepted)
            }
            Input::Adaptation{start,count}=>{
                start.checked_add(u64::from(count)).ok_or(Error::InvalidFrame)?;
                self.retry=None;
                Ok(1)
            }
            Input::Discontinuity=>{
                self.retry=None;
                self.voice_until=0;
                self.healthy_until=0;
                self.key(now,receipt);
                let Some(next) = self.incoming.retire_phone_media(generation, self.ring_origin)
                    .map_err(|_| Error::Write)? else { return Ok(0); };
                if let Some(speech)=&mut self.speech{speech.reset()?;}
                self.native_origin = None;
                self.ring_origin = next;
                Ok(1)
            }
            Input::Digit(_)=>Err(Error::InvalidFrame),
        }
    }
    pub(super) fn deadline(&self,now:u64)->Option<u64>{
        let at=if self.speech.is_some(){self.healthy_until.min(self.voice_until)}else{self.healthy_until};
        (at>now).then_some(at)
    }
    #[cfg(test)]
    fn record_credit(&mut self, kind:&str, start:u64, offered:usize, accepted:usize) {
        if !self.recorded_stall && offered>0 && accepted==0 && crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
            self.recorded_stall=true;
            let available=self.incoming.observer().observe().ok().map(|observation| observation.available_samples);
            eprintln!("private ring first uncredited {kind} at {start}, offered {offered}, epoch {}, available {available:?}",
                self.incoming.signals().pcm_epoch());
        }
    }
    pub(super) fn activate(&self){
        #[cfg(test)]
        if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed){
            crate::fixture::PHONE_ROUTE_INSTALLS.fetch_add(1,Ordering::Relaxed);
        }
        self.installed.store(true,Ordering::Release);
    }
}
pub(crate) struct Redirect {
    call:PeerReply,
    slot:Arc<std::sync::Mutex<Option<Radio>>>,
    wake:super::dispatcher::PeerWake,
    radio:Option<Radio>,
    installed:Arc<AtomicBool>,
}
impl Redirect {
    pub(crate) fn new(call:PeerReply,control:&super::private_phone::Control,radio:Radio)->Self{
        Self{call,slot:Arc::clone(&control.redirect),wake:control.wake.clone(),
            installed:Arc::clone(&radio.installed),radio:Some(radio)}
    }
    pub(crate) fn grant(&self)->&PeerReply{&self.call}
    pub(crate) fn submit(&mut self,clock:&Arc<SourceClock>)->bool{
        if !self.call.phone_media_allowed(){return true;}
        if let Some(mut candidate)=self.radio.take(){
            candidate.bind(Arc::clone(clock));
            *self.slot.lock().unwrap_or_else(|e|e.into_inner())=Some(candidate);
            self.wake.notify();
        }
        self.installed.load(Ordering::Acquire)
    }
}
impl Drop for Radio {fn drop(&mut self){self.incoming.signals().end();if self.installed.load(Ordering::Acquire){self.activity.set_active(false);}}}

#[cfg(test)]
mod tests{
    use super::*;
    use crate::link::ring::{InboundPolicy,InboundRing};
    use crate::services::ProcessedPeer;
    use rpt_advanced_core::{audio::LinkAudioQueue,config::resources::profiles::VadMode,
        controller::{ControllerSettings,CourtesySettings,NodeController},
        link::{AudioPeer,LinkAudio,Mode,PeerSignals,PhoneAccountId,PhoneCallId,PhoneSource,SourcePosition}};
    #[test]
    fn timed_private_ingress_preserves_continuous_silence_vad_qualification_and_retirement(){
        for enabled in [false,true] {
            let source=PhoneSource::new(PhoneAccountId::new("native-account").unwrap(),3,PhoneCallId::new([7;16]).unwrap(),5,9).unwrap();
            let activity=Arc::new(PeerSignals::for_phone(source.clone()));
            assert!(activity.authenticate_phone());
            let reply=activity.reply_target();
            let (writer,reader)=InboundRing::redirected(8000,InboundPolicy::Phone,&reply).unwrap();
            let (output,egress)=LinkAudioQueue::new(4096,8).unwrap().into_endpoints();
            let permit=Arc::new(AtomicBool::new(false));
            let slot=AudioPeer::for_phone(&source,&reply,Mode::phone(false),ProcessedPeer::new(reader,None),output,960).unwrap().with_phone_policy(false,true,Arc::clone(&permit));
            let (mut graph,_)=LinkAudio::new(vec![slot],960).unwrap();
            let (mut controller,_)=NodeController::new(ControllerSettings{hang_ms:0,..Default::default()},
                vec![],vec![],CourtesySettings::default()).unwrap();
            let timing=PhoneLatencyTiming{target_latency_ms:250, maximum_latency_ms:500};
            let mut radio=Radio::new(writer,egress,activity,PhoneVadSettings{enabled,mode:VadMode::Continuous,hang_ms:100},timing,Arc::clone(&permit)).unwrap();
            let mut rf=[0.;960];
            let mut now=1_000_000_000;
            radio.activate();
            let pending=radio.begin(now);
            assert_eq!(radio.input(Input::Audio{start:0,pcm:&[0.5;160],born_ns:Some(now),concealed:false},now,pending).unwrap(),160);
            assert!(!radio.incoming.signals().output_radio_keyed(pending));
            assert!(!graph.process(&mut controller,false,&mut rf,SourcePosition::default()).unwrap());
            assert!(rf.iter().all(|sample|*sample==0.0));
            now += 20_000_000;
            permit.store(true, Ordering::Release);
            let receipt = radio.begin(now);
            let before_grant = Input::Audio {
                start: 160, pcm: &[0.5; 160], born_ns: Some(now - 1_000_000), concealed: false,
            };
            assert_eq!(radio.input(before_grant, now, receipt).unwrap(), 160);
            assert!(!graph.process(&mut controller, false, &mut rf, SourcePosition::default()).unwrap(),
                "media queued before private RF attachment must not key even within the latency budget");
            assert!(rf.iter().all(|sample| sample.abs() < 0.001));
            for i in 0..10 {
                let receipt=radio.begin(now);
                let input=Input::Audio{start:160*(i+2),pcm:&[0.;160],born_ns:Some(now),concealed:false};
                assert_eq!(radio.input(input,now,receipt).unwrap(),160);
                now+=20_000_000;
            }
            assert_eq!(graph.process(&mut controller,false,&mut rf,SourcePosition::default()).unwrap(),!enabled);
            let peak = rf.iter().copied().map(f32::abs).fold(0.0, f32::max);
            assert!(peak < 0.001, "pre-grant speech reached later silent playout: peak {peak}");
            assert_eq!(graph.status().last_keyed(),None);
            now+=600_000_000;
            radio.begin(now);
            assert!(!graph.process(&mut controller,false,&mut rf,SourcePosition::default()).unwrap());
            let receipt=radio.begin(now);
            let stale=Input::Audio{start:1920,pcm:&[0.5;160],born_ns:Some(now-600_000_000),concealed:false};
            assert_eq!(radio.input(stale,now,receipt).unwrap(),160);
            assert!(!graph.process(&mut controller,false,&mut rf,SourcePosition::default()).unwrap());
            if !enabled {
                let mut heard = false;
                for frame in 0..20 {
                    now += 20_000_000;
                    let receipt = radio.begin(now);
                    let fresh = Input::Audio { start: 2080 + frame * 160, pcm: &[0.25;160],
                        born_ns: Some(now), concealed: false };
                    assert_eq!(radio.input(fresh, now, receipt).unwrap(), 160);
                    let keyed = graph.process(&mut controller, false, &mut rf, SourcePosition::default()).unwrap();
                    heard |= keyed && rf.iter().any(|sample| sample.abs() > 0.02);
                }
                assert!(heard, "fresh authorized speech never reached RF after the denied prefix");

                // The native IAX source restarts its sample cursor at zero after an
                // acknowledged media discontinuity on the same authenticated call.
                radio.control(true, None);
                let receipt = radio.begin(now);
                assert_eq!(radio.input(Input::Gap { start: 5280, count: 160 }, now, receipt), Ok(160));
                assert_eq!(radio.input(Input::Discontinuity, now, receipt), Ok(1));
                let first = Input::Audio {
                    start: 0, pcm: &[0.5; 160], born_ns: Some(now), concealed: false,
                };
                assert_eq!(radio.input(first, now, receipt), Ok(160),
                    "acknowledged native media restart must accept the marked prefix on the existing private call");
                assert!(!radio.incoming.signals().output_radio_keyed(receipt));

                radio.control(false, None);
                // Retired and held PCM was nonzero. Fresh silence may key under
                // the continuous policy, but must not replay either old signal.
                for frame in 1..=16 {
                    now += 20_000_000;
                    let receipt = radio.begin(now);
                    let silence = Input::Audio {
                        start: frame * 160, pcm: &[0.; 160], born_ns: Some(now), concealed: false,
                    };
                    assert_eq!(radio.input(silence, now, receipt), Ok(160));
                    graph.process(&mut controller, false, &mut rf, SourcePosition::default()).unwrap();
                    assert!(rf.iter().all(|sample| sample.abs() < 0.001),
                        "retired private audio leaked into fresh silent playout");
                }
                let mut resumed = false;
                for frame in 17..=36 {
                    now += 20_000_000;
                    let receipt = radio.begin(now);
                    let first_fresh = Input::Audio {
                        start: frame * 160, pcm: &[0.25; 160], born_ns: Some(now), concealed: false,
                    };
                    assert_eq!(radio.input(first_fresh, now, receipt), Ok(160));
                    let keyed = graph.process(&mut controller, false, &mut rf, SourcePosition::default()).unwrap();
                    resumed |= keyed && rf.iter().any(|sample| sample.abs() > 0.02);
                }
                assert!(resumed, "the same private route must play fresh authorized audio after restart");
            }
            reply.retire();
            assert!(!graph.process(&mut controller,false,&mut rf,SourcePosition::default()).unwrap());
        }
    }
}
