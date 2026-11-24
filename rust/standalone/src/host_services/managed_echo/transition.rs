//! One aggregate attempt retains all displaced owners until the final barrier.
use super::*;
use rptadv_peer_support::echolink::Exchange;

#[derive(Default)]
pub(super) struct Change {
    old: Option<Row>, backup: Option<Row>, next: Option<Row>, live: bool,
    displaced: bool, started: bool, exchanged: bool, restored: bool,
}
pub(super) struct Journal {
    old: RowMap, changes: BTreeMap<Station, Change>, public: Published,
    closing: Arc<AtomicBool>, ticket: u64, started: bool, applied: bool,
    pub(super) published: bool, pub(super) reversed: bool, uncertain: bool,
}
impl Journal {
    pub(super) fn new(old: RowMap, public: Published, closing: Arc<AtomicBool>, ticket:u64) -> Self {
        Self { old, changes: Default::default(), public, closing, ticket, started:false, applied:false,
            published:false, reversed:false, uncertain:false }
    }
    pub(super) fn insert(&mut self, role:u32, row:Row) -> Result<(), EchoError> {
        if self.started { return Err(EchoError::Contract); }
        let old = self.old.get(&row.station);
        let c = self.changes.entry(row.station).or_insert_with(|| Change {old:old.cloned(), ..Default::default()});
        let same = old.is_some_and(|old| old.account == row.account && old.version == row.version && old.label == row.label);
        match role {
            1 if old.is_none() && c.next.is_none() => c.next = Some(row),
            2 if same && c.next.is_none() && c.backup.is_none() => { c.live = true; c.next=Some(row); }
            3 if same && c.backup.is_none() && c.next.is_none() => c.backup=Some(row),
            4 if old.is_some() && !same && c.backup.is_some() && c.next.is_none() => c.next=Some(row),
            _ => return Err(EchoError::Contract),
        }
        Ok(())
    }
    fn exchange(old:&Row, next:&Row, id:NonZeroU64, closing:&AtomicBool) -> Result<Exchange, EchoError> {
        let generation = NonZeroU64::new(old.generation).ok_or(EchoError::Contract)?;
        loop {
            let result = old.native.account().exchange(next.native.account(), id, generation);
            if !matches!(result, Exchange::Pending) { return Ok(result); }
            if closing.load(Ordering::Acquire) {
                // Shutdown owns both sides. Request native cancellation/join off executor.
                let _ = next.native.stop();
                return Ok(Exchange::Uncertain);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    fn exchange_id(&self, inverse:bool) -> Result<NonZeroU64, EchoError> {
        self.ticket.checked_mul(2).and_then(|v| v.checked_add(u64::from(inverse)))
            .and_then(NonZeroU64::new).ok_or(EchoError::Contract)
    }
    fn apply(&mut self) -> Result<(), EchoError> {
        let id = self.exchange_id(false)?;
        self.started=true;
        {
            let mut public = self.public.lock().map_err(|_| EchoError::Control)?;
            for (station,c) in &self.changes {
                if let Some(old)=&c.old { old.native.admit(false); }
                public.remove(station);
            }
        }
        // Release competing binds before starting any replacement on the same socket.
        for c in self.changes.values_mut().filter(|c| c.backup.is_some()) {
            let old = c.old.as_ref().ok_or(EchoError::Contract)?;
            if let Err(error) = old.native.stop() { self.uncertain=true; return Err(error); }
            c.displaced=true;
        }
        for c in self.changes.values_mut() {
            let Some(next)=&mut c.next else { continue };
            if c.live {
                let old=c.old.as_ref().ok_or(EchoError::Contract)?;
                match Self::exchange(old,next,id,&self.closing)? {
                    Exchange::Applied => c.exchanged=true,
                    Exchange::Refused => return Err(EchoError::Product),
                    _ => { self.uncertain=true; return Err(EchoError::Control); }
                }
            } else {
                c.started=true;
                next.activate()?;
            }
        }
        self.applied=true;
        Ok(())
    }
    fn reverse(&mut self) -> Result<(), EchoError> {
        if self.uncertain { return Err(EchoError::Control); }
        let inverse=self.exchange_id(true)?;
        for c in self.changes.values_mut().rev() {
            if c.exchanged {
                let old=c.old.as_ref().ok_or(EchoError::Contract)?;
                let next=c.next.as_ref().ok_or(EchoError::Contract)?;
                if !matches!(Self::exchange(old,next,inverse,&self.closing)?, Exchange::Applied) {
                    self.uncertain=true; return Err(EchoError::Control);
                }
                c.exchanged=false;
            }
            if c.started {
                c.next.as_ref().ok_or(EchoError::Contract)?.native.stop()?;
                c.started=false;
            }
        }
        for c in self.changes.values_mut().rev() {
            if c.displaced {
                let backup=c.backup.as_mut().ok_or(EchoError::Contract)?;
                backup.activate()?;
                c.restored=true;
            } else if let Some(old)=&c.old { old.observe()?; }
        }
        self.reversed=true;
        Ok(())
    }
    pub(super) fn selection(&self) -> RowMap {
        let mut selected=self.old.clone();
        for (station,c) in &self.changes {
            let row=if self.reversed && c.restored { c.backup.as_ref() }
                else if self.published && !self.reversed && !c.live { c.next.as_ref() } else { c.old.as_ref() };
            match row { Some(r)=>{selected.insert(*station,r.clone());}, None=>{selected.remove(station);} }
        }
        selected
    }
    pub(super) fn phase(&mut self, phase:u32) -> Result<(), EchoError> {
        match phase {
            1 => self.apply(),
            2 if self.applied && !self.uncertain => {
                for c in self.changes.values() {
                    if let Some(actual)=if c.live { c.old.as_ref() } else { c.next.as_ref() } { actual.observe()?; }
                }
                self.published=true;
                Ok(())
            }
            3 => self.reverse(),
            4 if !self.uncertain && (!self.started || self.reversed || self.published) => {
                let selection=self.selection();
                for c in self.changes.values() {
                    for row in [&c.next,&c.backup].into_iter().flatten() {
                        if !selection.get(&row.station).is_some_and(|r| Arc::ptr_eq(r.native.account(),row.native.account())) {
                            row.native.stop()?;
                        }
                    }
                }
                if self.started {
                    for station in self.changes.keys() {
                        if let Some(row) = selection.get(station) { row.observe()?; }
                    }
                    let mut public=self.public.lock().map_err(|_|EchoError::Control)?;
                    for station in self.changes.keys() {
                        if let Some(row)=selection.get(station) {
                            public.insert(*station,row.clone());
                            row.native.admit(true);
                        } else { public.remove(station); }
                    }
                }
                // Drop displaced native snapshots on this off-executor lifecycle worker.
                self.old = selection;
                self.changes.clear();
                Ok(())
            }
            _ => Err(EchoError::Contract),
        }
    }
    pub(super) fn stop(&self) -> bool {
        let mut ok=true;
        for c in self.changes.values() {
            // Stop the dormant exchange side before the target to release its native reservation.
            for row in [&c.next,&c.backup,&c.old].into_iter().flatten() { ok &= row.native.stop().is_ok(); }
        }
        for row in self.old.values() { ok &= row.native.stop().is_ok(); }
        ok
    }
}
