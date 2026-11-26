//! Authored delay changes run on the negotiated ordinary peer's exclusive owner.
use crossbeam_queue::ArrayQueue;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

pub(crate) type Set = unsafe extern "C" fn(*mut c_void, u32, u32, u32) -> i32;

/// Ordered minimum, initial and maximum delay, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delays(u32, u32, u32);

impl Delays {
    /// Accept a tuple already resolved by the product's warning/default policy.
    pub fn new(minimum: u32, initial: u32, maximum: u32) -> Option<Self> {
        (minimum <= initial && initial <= maximum).then_some(Self(minimum, initial, maximum))
    }
    pub(crate) unsafe fn apply(self, set: Set, peer: *mut c_void) -> bool {
        unsafe { set(peer, self.0, self.1, self.2) == 0 }
    }
}

/// Result of one immutable request. Retirement also invalidates an outstanding request.
#[derive(Clone)]
pub struct Receipt {
    result: Arc<AtomicU8>,
    live: Arc<AtomicBool>,
}
impl Receipt {
    /// Pending is None; a refusal or retired peer yields Some(false).
    pub fn applied(&self) -> Option<bool> {
        receipt_status(
            || self.live.load(Ordering::Acquire),
            || self.result.load(Ordering::Acquire),
        )
    }
}

fn receipt_status(live: impl FnOnce() -> bool, result: impl FnOnce() -> u8) -> Option<bool> {
    // Acquiring retirement must precede reading its published completion result.
    let live = live();
    match result() {
        1 => Some(true),
        2 => Some(false),
        _ if !live => Some(false),
        _ => None,
    }
}

struct Command {
    delays: Delays,
    result: Arc<AtomicU8>,
}

/// Management-side handoff for one negotiated peer, with no direct access to native state.
pub struct PlayoutControl {
    commands: Arc<ArrayQueue<Command>>,
    live: Arc<AtomicBool>,
}

impl PlayoutControl {
    /// Queue a change (or the saved tuple for reverse). Wake the existing attached
    /// executor token, then retain and poll this receipt before aggregate confirmation.
    pub fn request(&mut self, delays: Delays) -> Option<Receipt> {
        if !self.live.load(Ordering::Acquire) {
            return None;
        }
        let result = Arc::new(AtomicU8::new(0));
        self.commands
            .push(Command {
                delays,
                result: Arc::clone(&result),
            })
            .ok()?;
        Some(Receipt {
            result,
            live: Arc::clone(&self.live),
        })
    }
}

pub(crate) struct Owner {
    control: PlayoutControl,
    set: Set,
}
impl Owner {
    pub(crate) fn new(set: Set) -> (Self, PlayoutControl) {
        let commands = Arc::new(ArrayQueue::new(1));
        let live = Arc::new(AtomicBool::new(true));
        (
            Self {
                set,
                control: PlayoutControl {
                    commands: Arc::clone(&commands),
                    live: Arc::clone(&live),
                },
            },
            PlayoutControl { commands, live },
        )
    }
    pub(crate) unsafe fn step(&self, peer: *mut c_void) {
        if let Some(command) = self.control.commands.pop() {
            let ok = unsafe { command.delays.apply(self.set, peer) };
            command
                .result
                .store(if ok { 1 } else { 2 }, Ordering::Release);
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.control.live.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observing_retirement_preserves_an_accepted_receipt() {
        // The sole writer published acceptance before retirement. Model the minimum
        // visibility promised after acquiring retirement, without scheduling threads.
        let retirement_observed = std::cell::Cell::new(false);
        let observe = || {
            receipt_status(
                || {
                    retirement_observed.set(true);
                    false
                },
                || u8::from(retirement_observed.get()),
            )
        };
        let first = observe();
        let second = observe();
        assert_eq!((first, second), (Some(true), Some(true)));
    }

    #[test]
    fn completed_and_abandoned_receipts_remain_terminal() {
        unsafe extern "C" fn accept(_: *mut c_void, _: u32, _: u32, _: u32) -> i32 {
            0
        }
        unsafe extern "C" fn refuse(_: *mut c_void, _: u32, _: u32, _: u32) -> i32 {
            -1
        }

        for (setter, step, expected) in [
            (accept as Set, true, true),
            (refuse as Set, true, false),
            (accept as Set, false, false),
        ] {
            let (owner, mut control) = Owner::new(setter);
            let receipt = control.request(Delays::new(20, 40, 100).unwrap()).unwrap();
            let copy = receipt.clone();
            assert_eq!(receipt.applied(), None);
            if step {
                // Both test setters ignore the opaque peer argument.
                unsafe { owner.step(std::ptr::null_mut()) };
                assert_eq!(copy.applied(), Some(expected));
            }
            drop(owner);
            assert_eq!(receipt.applied(), Some(expected));
            assert_eq!(copy.applied(), Some(expected));
        }
    }
}
