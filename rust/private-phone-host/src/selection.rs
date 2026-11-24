//! One bounded private account worker and the exact native transition it owns.
use crate::{Error, JitterProvider, accounts::Route, calls::{PrivateCall, SipPhones},
    phone::{PreparedAccount, Protocol}, sip_provider::Provider};
use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::{Arc, Mutex, mpsc::{self, Receiver, SyncSender, TryRecvError}},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Private adapter paths selected by the frontend's existing installed-provider owner.
pub struct NativePaths { pub sip: PathBuf, pub jitter: PathBuf, pub iax: PathBuf, pub g711: PathBuf }
#[derive(Default,Clone)]
struct Providers { sip:Option<Arc<Provider>>,iax:Option<crate::iax::IaxClient> }
impl Providers {
    fn require(&mut self,paths:&NativePaths,rows:&Rows)->Result<(),Error>{
        for row in rows.values(){
            match row.protocol(){
                Protocol::Sip if self.sip.is_none()=>{
                    let provider=unsafe{Provider::load(&paths.sip)}.map_err(|_|Error::Provider)?;
                    if !provider.can_originate(){return Err(Error::Provider);}
                    self.sip=Some(provider);
                }
                Protocol::Iax2=>{
                    if self.iax.is_none(){self.iax=Some(crate::iax::IaxClient::open(&paths.iax,&paths.g711).map_err(|_|Error::Provider)?);}
                    if !self.iax.as_ref().is_some_and(|c|c.supports(row.jitter_ms())){return Err(Error::Provider);}
                }
                _=>{}
            }
        }
        Ok(())
    }
}
type Rows = BTreeMap<String, Arc<PreparedAccount>>;

/// The signaling owner delivers a uniquely authenticated transport to its frontend.
/// Failure to transfer drops the call and initiates native retirement.
pub type Incoming = Arc<dyn Fn(PrivateCall) + Send + Sync>;

enum Request {
    Select { rows: Rows, providers:Providers, result: SyncSender<Result<(), Error>> },
    Dial { port: String, account: String, generation: u64, destination: String,
        cancel: Arc<std::sync::atomic::AtomicBool>, result: SyncSender<Result<PrivateCall, Error>> },
}
struct PendingSelection {
    started: Instant,
    rows: Rows,
    result: SyncSender<Result<(), Error>>,
}
struct PendingDial {
    native: crate::calls::Outgoing, cancel: Arc<std::sync::atomic::AtomicBool>,
    result: SyncSender<Result<PrivateCall, Error>>,
}
struct Worker {
    requests: SyncSender<Request>,
    providers:Providers,
    finished: Option<JoinHandle<Result<(), Error>>>,
    departed: Receiver<()>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    retired_uncertain: bool,
    live_jitter: bool,
    observed: Arc<Mutex<BTreeMap<String, (u64,u32)>>>,
}

impl Worker {
    fn open(paths: &NativePaths, incoming: Incoming) -> Result<Self, Error> {
        let jitter = unsafe { JitterProvider::load(&paths.jitter) }?;

        let live_jitter = jitter.live_update;
        let (requests, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let halted = Arc::clone(&stop);
        let observed = Arc::new(Mutex::new(BTreeMap::new()));
        let snapshot = Arc::clone(&observed);
        // The last local drops on every worker return or unwind, after native owners.
        let (departure, departed) = mpsc::channel::<()>();
        let finished = thread::Builder::new().name("rpt-private-accounts".into()).spawn(move || {
            let _departure = departure;
            let mut pending: Option<PendingSelection> = None;
            let mut routes = BTreeMap::<String, Arc<Route>>::new();
            let mut outgoing = Vec::<PendingDial>::new();
            let mut sip:Option<SipPhones>=None;
            let mut iax=crate::iax::Bank::default();
            let mut iax_client=None;
            while !halted.load(std::sync::atomic::Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_millis(10)) {
                    Ok(Request::Select { rows, providers, result }) => {
                        if pending.is_some() {
                            let _ = result.send(Err(Error::Provider));
                            continue;
                        }
                        // This worker is off control and off BOTH transport executors.
                        // No other selection can run until its actual resolver returns.
                        let next = resolve(&rows, &routes).and_then(|next| {
                            if sip.is_none() && let Some(provider)=providers.sip {sip=Some(SipPhones::new(provider,Arc::clone(&jitter))?);}
                            if let Some(provider)=providers.iax{iax_client=Some(provider);}
                            let mut sip_routes=Vec::new();
                            let mut iax_routes=BTreeMap::new();
                            for (key,route) in &next {
                                match route.account.protocol(){
                                    Protocol::Sip=>sip_routes.push(Arc::clone(route)),
                                    Protocol::Iax2=>{iax_routes.insert(key.clone(),Arc::clone(route));}
                                }
                            }
                            if let Some(sip)=&mut sip {sip.select(&sip_routes)?;}
                            iax.select(iax_routes)?;
                            Ok(next)
                        });
                        match next {
                            Ok(next) => {
                                routes = next;
                                pending = Some(PendingSelection { started: Instant::now(), rows, result });
                            }
                            Err(error) => { let _ = result.send(Err(error)); }
                        }
                    }
                    Ok(Request::Dial {port, account, generation, destination, cancel, result}) => {
                        // Sofia admits at most eight pending/established calls per account.
                        // Bound submissions that are still waiting for a native mailbox slot too.
                        if routes.get(&account).is_some_and(|r|r.account.protocol()==Protocol::Iax2){
                            if let Err(error)=iax.dial(&port,&account,generation,destination,cancel,result.clone()){
                                let _=result.send(Err(error));
                            }
                        } else if outgoing.len() >= routes.len().saturating_mul(8)
                            || cancel.load(std::sync::atomic::Ordering::Acquire) {
                            let _ = result.send(Err(Error::Retired));
                        } else {
                            match sip.as_ref().ok_or(Error::Provider).and_then(|s|s.originate(&port, &account, generation, &destination)) {
                                Ok(native) => outgoing.push(PendingDial {native,cancel,result}),
                                Err(error) => { let _ = result.send(Err(error)); }
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                let serviced=iax.service(iax_client.as_ref(),&jitter,&incoming);
                let (iax_ready,iax_states)=iax.states();
                let sip_state=sip.as_ref().map_or_else(||Ok((true,Vec::new())),SipPhones::observe);
                let mut table=snapshot.lock().map_err(|_|Error::Provider)?;
                table.clear();
                table.extend(iax_states.into_iter().map(|(account,generation,status)|(account,(generation,status))));
                if let Ok((_,registrations))=&sip_state{
                    for (account,status) in registrations {
                        table.insert(account.clone(),(status.generation,match status.state{
                            0|1=>1,2=>2,3|6=>3,4|7=>0,_=>4,
                        }));
                    }
                }
                if let Some(PendingSelection { started: start, rows, .. }) = &pending {
                    let complete=iax_ready && matches!(&sip_state,Ok((true,_))) && rows.len()==table.len() &&
                        rows.iter().all(|(id,row)|table.get(id)==Some(&(row.generation(),2)));
                    let resolution=if complete{Some(Ok(()))}
                        else if serviced.is_err() || sip_state.is_err() || table.values().any(|(_,v)|*v==4) ||
                            start.elapsed()>=Duration::from_secs(70){Some(Err(Error::Provider))}
                        else{None};
                    if let Some(outcome) = resolution {
                        let sender=pending.take().expect("observed outstanding selection").result;
                        let _=sender.send(outcome);
                    }
                }
                drop(table);
                outgoing.retain_mut(|dial| {
                    if dial.cancel.load(std::sync::atomic::Ordering::Acquire) {return false;}
                    match dial.native.poll() {
                        Ok(None) => true,
                        result => {
                            let _ = dial.result.send(result.and_then(|call| call.ok_or(Error::Retired)));
                            false
                        }
                    }
                });
                // The product independently authorizes this exact installed account
                // before any prompt or mailbox lookup. Call only after dropping all maps.
                if let Some(sip)=&sip && let Ok(Some(call)) = sip.incoming() { incoming(call); }
            }
            if !iax.empty(){return Err(Error::Provider);}
            sip.as_mut().map_or(Ok(()),SipPhones::stop)
        }).map_err(|_| Error::Provider)?;
        Ok(Self { requests, live_jitter, providers:Providers::default(), finished: Some(finished), departed, stop, observed, retired_uncertain:false })
    }
    fn select(&self, rows: Rows)->Result<Receiver<Result<(),Error>>, Error> {
        let (result, rx)=mpsc::sync_channel(1);
        self.requests.try_send(Request::Select{rows,result,providers:self.providers.clone()}).map_err(|_|Error::Provider)?;
        Ok(rx)
    }
    fn stop(&mut self, wait: Duration)->Result<bool,Error>{
        self.stop.store(true,std::sync::atomic::Ordering::Release);
        if self.finished.as_ref().is_some_and(|job| !job.is_finished())
            && (wait.is_zero() || matches!(self.departed.recv_timeout(wait), Err(mpsc::RecvTimeoutError::Timeout)))
        {
            return Ok(false);
        }
        if let Some(job)=self.finished.take() {
            self.retired_uncertain = !matches!(job.join(), Ok(Ok(())));
        }
        if self.retired_uncertain {Err(Error::Provider)} else {Ok(true)}
    }
}

fn resolve(rows:&Rows, old:&BTreeMap<String, Arc<Route>>)->Result<BTreeMap<String, Arc<Route>>, Error> {
    let mut next=BTreeMap::new();
    for (id,row) in rows {
        let retained=old.get(id).filter(|route|route.generation()==row.generation());
        let route=if let Some(previous)=retained {
            if !previous.account.same_registration(row) {return Err(Error::Input);}
            if previous.account.jitter_ms() == row.jitter_ms() {
                Arc::clone(previous)
            } else {
                // Policy changes reuse the resolved registrar and the existing native account.
                Arc::new(Route::new(Arc::clone(row), previous.remote()).ok_or(Error::Input)?)
            }
        } else {
            let (host,port)=row.endpoint();
            let host=host.strip_prefix('[').and_then(|s|s.strip_suffix(']')).unwrap_or(host);
            let address=match host.parse::<IpAddr>() {
                Ok(ip)=>SocketAddr::new(ip,port),
                Err(_)=>(host,port).to_socket_addrs().map_err(|_|Error::Provider)?
                    .find(|s|!s.ip().is_unspecified()&&!s.ip().is_multicast()).ok_or(Error::Provider)?,
            };
            Arc::new(Route::new(Arc::clone(row),address).ok_or(Error::Input)?)
        };
        next.insert(id.clone(),route);
    }
    Ok(next)
}

#[derive(Clone,Copy,Eq,PartialEq)]
enum Phase { Dormant, Applying, Applied, Reversing, Reversed, FailedApply, FailedReverse }
struct Ticket {
    id: usize, next: Rows, phase: Phase, submitted: Instant, pending: Option<Receiver<Result<(), Error>>>,
}
struct State { worker: Option<Worker>, ticket: Option<Ticket>, selected: Rows, stopping: bool }

/// Per-frontend participant. A ticket identity is a checked opaque integer and is never dereferenced.
pub struct PhoneHost { paths: NativePaths, incoming: Incoming, state: Mutex<State> }
impl PhoneHost {
    /// Inert until a complete native private selection is prepared.
    pub fn new(paths: NativePaths,incoming:Incoming)->Self {
        Self{paths,incoming,state:Mutex::new(State{worker:None,ticket:None,selected:Rows::new(),stopping:false})}
    }
    /// Copy checks are complete before allocating a new native owner. No registration starts here.
    pub fn prepare(&self, rows:Vec<PreparedAccount>)->Result<usize,Error> {
        static LAST: std::sync::atomic::AtomicUsize=std::sync::atomic::AtomicUsize::new(0);
        let mut state=self.state.lock().map_err(|_|Error::Provider)?;
        if state.stopping || state.ticket.is_some(){return Err(Error::Retired);}
        let mut next=Rows::new();
        for record in rows {
            let id=record.account_id().to_string();
            let row=if let Some(old)=state.selected.get(&id).filter(|r|r.generation()==record.generation()){
                if !old.same_registration(&record){return Err(Error::Input);}
                if old.jitter_ms() == record.jitter_ms() {
                    Arc::clone(old)
                } else {
                    let owner = state.worker.as_ref().ok_or(Error::Retired)?;
                    if !owner.live_jitter || (record.protocol() == Protocol::Iax2
                        && !owner.providers.iax.as_ref().is_some_and(|client| client.supports_live_policy())) {
                        return Err(Error::Provider);
                    }
                    Arc::new(record)
                }
            }else{Arc::new(record)};
            if next.insert(id,row).is_some(){return Err(Error::Input);}
        }
        if state.worker.is_none() { state.worker=Some(Worker::open(&self.paths,Arc::clone(&self.incoming))?); }
        state.worker.as_mut().ok_or(Error::Provider)?.providers.require(&self.paths,&next)?;
        let id=LAST.fetch_update(std::sync::atomic::Ordering::Relaxed,std::sync::atomic::Ordering::Relaxed,|n|n.checked_add(1)).map_err(|_|Error::Provider)?+1;
        state.ticket=Some(Ticket{id,next,phase:Phase::Dormant,submitted:Instant::now(),pending:None});
        Ok(id)
    }
    /// Begin or poll the exact selection. A reversal never races a still-running native apply.
    pub fn drive(&self,id:usize,reverse:bool)->Result<bool,Error>{
        let mut state=self.state.lock().map_err(|_|Error::Provider)?;
        let State{worker,ticket,selected,..} = &mut *state;
        let plan=ticket.as_mut().filter(|p|p.id==id).ok_or(Error::Retired)?;
        if let Some(result)=&plan.pending {
            let terminal=match result.try_recv(){
                Ok(result)=>result,
                Err(TryRecvError::Empty) if plan.submitted.elapsed() >= Duration::from_secs(75) =>
                    return Err(Error::Provider),
                Err(TryRecvError::Empty)=>return Ok(false),
                Err(TryRecvError::Disconnected)=>Err(Error::Provider),
            };
            plan.pending=None;
            plan.phase=match (plan.phase,terminal.is_ok()) {
                (Phase::Applying,true)=>Phase::Applied,(Phase::Applying,false)=>Phase::FailedApply,
                (Phase::Reversing,true)=>Phase::Reversed,(Phase::Reversing,false)=>Phase::FailedReverse,
                _=>return Err(Error::Retired),
            };
        }
        let rows=match (reverse,plan.phase){
            (false,Phase::Applied)|(true,Phase::Reversed)=>return Ok(true),
            (true,Phase::Dormant)=>{plan.phase=Phase::Reversed;return Ok(true);}
            (false,Phase::Dormant)=>plan.next.clone(),
            (true,Phase::Applied|Phase::FailedApply)=>selected.clone(),
            _=>return Err(Error::Provider),
        };
        plan.pending=Some(worker.as_ref().ok_or(Error::Provider)?.select(rows)?);
        plan.submitted=Instant::now();
        plan.phase=if reverse{Phase::Reversing}else{Phase::Applying};
        Ok(false)
    }
    /// Observe only the exact registered resource within an actually selected native table.
    pub fn observe(&self, port:&[u8;16], account:&[u8;16], generation:u64)->Result<u32,Error>{
        let state=self.state.lock().map_err(|_|Error::Provider)?;
        let id=uuid::Uuid::from_bytes(*account).to_string();
        let rows=match &state.ticket {Some(plan) if plan.phase==Phase::Applied=>&plan.next,_=>&state.selected};
        if !rows.get(&id).is_some_and(|r|r.port_id().as_bytes()==port && r.generation()==generation) {
            return Err(Error::Retired);
        }
        let observed=state.worker.as_ref().ok_or(Error::Provider)?.observed.lock().map_err(|_|Error::Provider)?;
        observed.get(&id).filter(|(actual,_)|*actual==generation).map(|(_,s)|*s).ok_or(Error::Retired)
    }
    /// Originate on the retained registration and transfer the selected answered call
    /// only while the product's exact authority remains current. This runs on a
    /// product control job, outside the account worker and native provider reactor.
    pub fn originate(&self, port:[u8;16], account:[u8;16], generation:u64,
        destination:&str, mut current:impl FnMut()->bool)->Result<PrivateCall,Error> {
        if !current() || self.observe(&port,&account,generation)? != 2 {return Err(Error::Retired);}
        let (result, rx)=mpsc::sync_channel(1);
        let cancel=Arc::new(std::sync::atomic::AtomicBool::new(false));
        let command=Request::Dial {
            port:uuid::Uuid::from_bytes(port).to_string(),account:uuid::Uuid::from_bytes(account).to_string(),
            generation,destination:destination.to_owned(),cancel:Arc::clone(&cancel),result,
        };
        self.state.lock().map_err(|_|Error::Provider)?.worker.as_ref().ok_or(Error::Provider)?
            .requests.try_send(command).map_err(|_|Error::Provider)?;
        let start=Instant::now();
        loop {
            if !current() || start.elapsed()>=Duration::from_secs(35) {
                cancel.store(true,std::sync::atomic::Ordering::Release);
                return Err(Error::Retired);
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(result)=>{
                    let call=result?;
                    if !current() || self.observe(&port,&account,generation)?!=2 {return Err(Error::Retired);}
                    return Ok(call);
                }
                Err(mpsc::RecvTimeoutError::Timeout)=>{}
                Err(mpsc::RecvTimeoutError::Disconnected)=>return Err(Error::Provider),
            }
        }
    }
    /// Consume the ticket only after the native forward or reverse transition has finished.
    pub fn release(&self,id:usize)->Result<(),Error>{
        let mut state=self.state.lock().map_err(|_|Error::Provider)?;
        let plan=state.ticket.as_ref().filter(|p|p.id==id).ok_or(Error::Retired)?;
        if !matches!(plan.phase,Phase::Applied|Phase::Dormant|Phase::Reversed){return Err(Error::Provider);}
        let plan=state.ticket.take().expect("verified current ticket");
        if plan.phase==Phase::Applied {state.selected=plan.next;}
        Ok(())
    }
    /// Stop only after the product has confirmed withdrawal of every private row.
    pub fn stop(&self)->Result<bool,Error>{
        self.stop_within(Duration::ZERO)
    }

    /// Wait at most the supplied interval for a withdrawn account worker to leave
    /// its native service loop, then join it. An active selection or an unresolved
    /// withdrawal retains the worker and returns pending immediately.
    pub fn stop_within(&self, wait: Duration)->Result<bool,Error>{
        let mut state=self.state.lock().map_err(|_|Error::Provider)?;
        if state.ticket.is_some()||!state.selected.is_empty(){return Ok(false);}
        state.stopping=true;
        if let Some(worker)=&mut state.worker && !worker.stop(wait)?{return Ok(false);}
        state.worker=None;Ok(true)
    }
}
