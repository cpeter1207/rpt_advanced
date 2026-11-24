//! One uniquely owned private IAX call, with callback and native jitter lifetime fences.
use super::{IaxError,IaxPeer,CallReceipt,ProviderCall,Route,abi,timed::{Event,Kind}};
use crate::{JitterProvider,Error};
use std::sync::mpsc;
use std::{ffi::c_void,mem::size_of,ptr,sync::{Arc,Mutex,atomic::{AtomicBool,AtomicU64,Ordering}}};
const DIGIT_TAIL_SIZE: usize = std::mem::offset_of!(abi::rptadv_peer_source_sink_v1, digit)
    + size_of::<abi::rptadv_peer_source_digit_v1>();
const ADAPTATION_TAIL_SIZE: usize =
    std::mem::offset_of!(abi::rptadv_peer_source_sink_v1, source_adaptation)
        + size_of::<abi::rptadv_peer_source_adaptation_v1>();

pub(super) struct Phone {
    pending: Option<Event>,
    offset: usize,
    acknowledged: u64,
    host_epoch: Option<u64>,
    egress_epoch: Option<u64>,
    closed: bool,
}


include!("media.rs");
struct Media { peer:IaxPeer, source:Phone, samples:[f32;160], delays:[u32;3] }
struct PolicyEdit { delays:[u32;3], result:mpsc::SyncSender<Result<(),Error>> }
struct Callback { call:Arc<Call>, step:super::executor::Step, context:*mut c_void }

// A live IAX peer can migrate between threads. All media operations have one serialized owner.
unsafe impl Send for Media {}
pub(crate) struct Call {
    local:String, account:String, token:String, receipt:CallReceipt,
    media:Mutex<Media>, attached:AtomicBool, live:AtomicBool,
    registration:Mutex<Option<usize>>, policy:Mutex<Option<PolicyEdit>>,
    // Keep jitter code loaded until after the peer's context is destroyed.
    _jitter:Arc<JitterProvider>,
}
impl Call {
    pub(super) fn new(mut owned:ProviderCall,route:&Route,jitter:Arc<JitterProvider>,delays:[u32;3])->Result<Arc<Self>,Error>{
        let receipt=owned.receipt;
        if receipt.account_generation!=route.generation() || receipt.call_generation==0 ||
            receipt.remote!=route.remote || !matches!(receipt.format,4|8) || owned.peer.sample_rate_hz()!=8000 ||
            !owned.peer.supports_timed_source(){return Err(Error::Retired);}
        let local=route.account.port_id().to_string();
        let account=route.account.account_id().to_string();
        let mut source=Phone{pending:None,offset:0,acknowledged:0,
            host_epoch:None,egress_epoch:None,closed:false};
        let client=&owned.peer._owner.0;
        unsafe{client.executor.configure_private(owned.peer.raw_handle(),jitter.descriptor(),delays)}.map_err(|_|Error::Provider)?;
        source.retire_egress(&mut owned.peer).map_err(|_|Error::Provider)?;
        // The immutable product registration generations are unique across protocols.
        // Provider-local call generations alone cannot distinguish listener lifetimes.
        static NEXT:AtomicU64=AtomicU64::new(0);
        let next=NEXT.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|v|v.checked_add(1)).map_err(|_|Error::Provider)?+1;
        let token=format!("{:016x}{:016x}",receipt.account_generation,next);
        Ok(Arc::new(Self{local,account,token,receipt,media:Mutex::new(Media{peer:owned.peer,source,samples:[0.;160],delays}),
            registration:Mutex::new(None),policy:Mutex::new(None),attached:AtomicBool::new(false),live:AtomicBool::new(true),_jitter:jitter}))
    }
    pub(crate) fn identity(&self)->(&str,&str,&str){(&self.local,&self.account,&self.token)}
    pub(crate) fn generations(&self)->(u64,u64){(self.receipt.account_generation,self.receipt.call_generation)}
    pub(crate) fn source(&self)->std::net::SocketAddr{self.receipt.remote}
    pub(crate) fn live(&self)->bool{self.live.load(Ordering::Acquire)}
    fn with<T>(&self,work:impl FnOnce(&mut Media)->Result<T,IaxError>)->Result<T,Error>{
        if !self.live(){return Err(Error::Retired);}
        let mut media=self.media.lock().map_err(|_|Error::Provider)?;
        let result=work(&mut media);
        if result.is_err() || !media.source.live(){self.live.store(false,Ordering::Release);}
        result.map_err(|_|Error::Provider)
    }
    pub(crate) unsafe fn read(&self,completion:u32,sink:*const abi::rptadv_peer_source_sink_v1,
        output:&mut abi::rptadv_peer_source_result_v1)->Result<(),Error>{
        self.with(|media|unsafe{media.source.read(&mut media.peer,&mut media.samples,completion,sink,output)})
    }
    pub(crate) fn prepare(&self,pcm:&[f32],bytes:&mut[u8],flush:u32,result:&mut abi::rptadv_codec_encode_result_v1)->Result<i32,Error>{
        self.with(|m|if m.source.encoding_ready(){Ok(m.peer.prepare_audio(pcm,bytes,flush,result))}else{Err(IaxError::Operation)})
    }
    pub(crate) fn send_prepared(&self,bytes:&[u8],frames:u32,timestamp:u32,deadline:u32)->Result<i32,Error>{
        self.with(|m|m.source.send_prepared(&mut m.peer,bytes,frames,timestamp,deadline))
    }
    pub(crate) fn reset(&self)->Result<(),Error>{self.with(|m|m.source.retire_egress(&mut m.peer))}
    pub(crate) fn elapsed(&self)->Result<u32,Error>{self.with(|m|Ok(m.peer.elapsed_ms()))}
    pub(crate) fn digit(&self,digit:u8)->Result<i32,Error>{self.with(|m|m.peer.send_digit(digit).map(|()|0))}
    pub(crate) fn deadline(&self,after:u64)->Result<(),Error>{
        self.with(|m|if unsafe{m.peer._owner.0.executor.set_deadline(m.peer.raw_handle(),after)}==0{Ok(())}else{Err(IaxError::Operation)})
    }
    fn apply_delays(&self, delays:[u32;3]) -> Result<(),Error> {
        if !self.live() { return Ok(()); }
        let mut media = self.media.lock().map_err(|_|Error::Provider)?;
        if media.delays == delays { return Ok(()); }
        let peer=&media.peer;
        unsafe { peer._owner.0.executor.set_delays(peer.raw_handle(),delays) }.map_err(|_|Error::Provider)?;
        media.delays=delays;
        Ok(())
    }
    pub(super) fn change_delays(&self,delays:[u32;3]) -> Result<(),Error> {
        let registration=self.registration.lock().map_err(|_|Error::Provider)?;
        let Some(token)=*registration else { return self.apply_delays(delays) };
        if !self.live() { return Ok(()); }
        let (result,receive)=mpsc::sync_channel(1);
        {
            let mut pending=self.policy.lock().map_err(|_|Error::Provider)?;
            if pending.is_some() {return Err(Error::Provider);}
            *pending=Some(PolicyEdit{delays,result});
        }
        // Wake is thread-safe; this lock keeps the exact token attached through wake.
        let client=self.media.lock().map_err(|_|Error::Provider)?.peer._owner.clone();
        unsafe {client.0.executor.wake(token as *mut c_void)};
        drop(registration);
        if let Ok(result)=receive.recv_timeout(std::time::Duration::from_secs(3)) { return result; }
        // The callback holds the mailbox through the native setter and result send.
        // Removing a still-pending edit guarantees it cannot execute after reversal.
        self.policy.lock().map_err(|_|Error::Provider)?.take();
        receive.try_recv().unwrap_or(Err(Error::Provider))
    }
    pub(crate) unsafe fn attach(self:&Arc<Self>,step:super::executor::Step,context:*mut c_void)->Result<Registration,Error>{
        if !self.live() || self.attached.swap(true,Ordering::AcqRel){return Err(Error::Retired);}
        let mut registration=self.registration.lock().map_err(|_|Error::Provider)?;
        let (client,raw)=self.with(|m|Ok((m.peer._owner.clone(),m.peer.raw_handle())))?;
        let mut callback=Box::new(Callback{call:self.clone(),step,context});
        // A native step can begin before attach returns. It does not borrow registration.
        let token=unsafe{client.0.executor.attach(raw,dispatch,std::ptr::from_mut(callback.as_mut()).cast())}
            .map_err(|_|{self.attached.store(false,Ordering::Release);Error::Provider})?;
        *registration=Some(token as usize);
        Ok(Registration{client,token,callback})
    }
}
unsafe extern "C" fn dispatch(context:*mut c_void)->i32 {
    let Some(callback)=(unsafe {context.cast::<Callback>().as_ref()}) else{return -1};
    let call=&callback.call;
    let Ok(mut pending)=call.policy.lock() else{return -1};
    if let Some(edit)=pending.take() {
        let _=edit.result.try_send(call.apply_delays(edit.delays));
    }
    drop(pending);
    let result=unsafe{(callback.step)(callback.context)};
    if result!=0 {call.live.store(false,Ordering::Release);}
    result
}
pub(crate) struct Registration {client:super::IaxClient,token:*mut c_void,callback:Box<Callback>}
// Native stop and wake are explicitly thread-safe; detach waits outside any callback.
unsafe impl Send for Registration {}
unsafe impl Sync for Registration {}
impl Registration {
    pub(crate) fn stop(&self){unsafe{self.client.0.executor.stop(self.token)};}
    pub(crate) fn wake(&self){unsafe{self.client.0.executor.wake(self.token)};}
}
impl Drop for Registration {
    fn drop(&mut self){
        let call=&self.callback.call;
        let mut registration=call.registration.lock().unwrap_or_else(|e|e.into_inner());
        unsafe{self.client.0.executor.detach(self.token)};
        *registration=None;
        let mut pending=call.policy.lock().unwrap_or_else(|e|e.into_inner());
        if let Some(edit)=pending.take() {
            // After the real native join this thread again has exclusive access.
            let _=edit.result.try_send(call.apply_delays(edit.delays));
        }
        call.attached.store(false,Ordering::Release);
    }
}

#[cfg(test)]
impl Call {
    /// Read genuine observations only while this fixture holds the native callback turn.
    pub(crate) fn test_playout(&self) -> Result<(abi::rptadv_iax2_peer_observation_v1,u64,std::net::SocketAddr), Error> {
        self.with(|media| {
            let mut observation: abi::rptadv_iax2_peer_observation_v1 = unsafe { std::mem::zeroed() };
            observation.abi_version = 1;
            observation.struct_size = size_of::<abi::rptadv_iax2_peer_observation_v1>() as u32;
            if unsafe { media.peer._owner.0.executor.observe(media.peer.raw_handle(),
                        std::ptr::from_mut(&mut observation).cast()) } != 0 { return Err(IaxError::Operation); }
            let epoch = media.source.egress_epoch.ok_or(IaxError::Operation)?;
            Ok((observation,epoch,self.receipt.local))
        })
    }
}
