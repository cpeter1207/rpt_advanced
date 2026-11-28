//! Detect caller digits on the native call owner and retain accepted-prefix authority.
use super::super::{
    private_radio::Radio,
    session::source::{Decoded, SourceIngress},
};
use crate::{
    Error,
    services::{PhoneDigits, phone_media::Input},
};
use rtrb::Producer;
use std::{
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use zeroize::Zeroize;

pub(crate) struct Digit {
    pub(crate) symbol: char,
    pub(crate) received: u64,
    pub(crate) revision: u64,
}

pub(super) struct Receive {
    ingress: SourceIngress,
    mode: PhoneDigits,
    scratch: [f32; 4096],
    digits: Producer<Digit>,
    revision: Cell<Option<u64>>,
    released: Arc<AtomicU64>,
}

fn advance(revision: &Cell<Option<u64>>) -> Result<u64, Error> {
    let next = revision.get().and_then(|v| v.checked_add(1));
    revision.set(next);
    next.ok_or(Error::Admission)
}

impl Receive {
    pub(super) fn new(
        rate: u32,
        mode: PhoneDigits,
        digits: Producer<Digit>,
        released: Arc<AtomicU64>,
    ) -> Result<Self, Error> {
        Ok(Self {
            ingress: SourceIngress::phone(rate)?,
            mode,
            scratch: [0.; 4096],
            digits,
            revision: Cell::new(Some(0)),
            released,
        })
    }

    pub(super) fn input(
        &mut self,
        input: Input<'_>,
        mut radio: Option<&mut Radio>,
        now: u64,
        receipt: u64,
        received: u64,
    ) -> Result<usize, Error> {
        let in_band = self.mode == PhoneDigits::InBand;
        let revision = &self.revision;
        let digits = &mut self.digits;
        let available = digits.slots();
        let failure = Cell::new(false);
        let mut emit = |symbol| {
            let result = advance(revision).and_then(|revision| {
                digits
                    .push(Digit {
                        symbol,
                        revision,
                        received,
                    })
                    .map_err(|_| Error::Allocation)
            });
            #[cfg(test)]
            if result.is_ok() && crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                crate::fixture::PHONE_COMMAND_DIGITS.fetch_add(1, Ordering::Relaxed);
            }
            if result.is_err() {
                failure.set(true);
            }
        };
        let mut completed = |symbol| {
            if in_band {
                emit(symbol);
            }
        };
        let credit = match input {
            Input::Digit(symbol) => {
                #[cfg(test)]
                if crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed) {
                    crate::fixture::PHONE_EVENT_DIGITS.fetch_add(1, Ordering::Relaxed);
                }
                // Some providers still report native events on a PCM-only route.
                // Their receipt earns credit but cannot dispatch an extra command.
                if in_band {
                    return Ok(1);
                }
                if available == 0 {
                    return Ok(0);
                }
                emit(symbol);
                self.ingress.hold();
                if let Some(radio) = radio {
                    radio.control(true, None);
                }
                1
            }
            Input::Audio {
                start,
                pcm,
                born_ns,
                concealed,
            } => {
                #[cfg(test)]
                if !concealed
                    && born_ns.is_some()
                    && crate::fixture::WATCH_PHONE_PCM.load(Ordering::Relaxed)
                {
                    crate::fixture::PHONE_WIRE_ORIGINAL.fetch_add(
                        pcm.iter().filter(|v| v.abs() > 0.02).count(),
                        Ordering::Relaxed,
                    );
                }
                let count = self.ingress.limit(pcm.len());
                if count == 0 || (in_band && available < self.ingress.digit_capacity(count)) {
                    return Ok(0);
                }
                let pcm = &pcm[..count];
                let analyzed = self.ingress.prepare(
                    radio
                        .as_ref()
                        .map(|route| -> &rpt_advanced_core::link::PeerSignals {
                            route.incoming.signals()
                        }),
                    &mut self.scratch,
                    &Decoded {
                        start,
                        pcm,
                        epoch: receipt,
                        concealed,
                    },
                    &mut completed,
                    || {
                        if in_band && advance(revision).is_err() {
                            failure.set(true);
                        }
                    },
                );
                // The provider retains raw retries. Only mute/cursor metadata crosses this read.
                self.scratch[..count].zeroize();
                analyzed?;
                let accepted = if let Some(radio) = radio {
                    radio.control(self.ingress.muted(), self.ingress.earliest_mute());
                    radio.input(
                        Input::Audio {
                            start,
                            pcm,
                            born_ns,
                            concealed,
                        },
                        now,
                        receipt,
                    )?
                } else {
                    count
                };
                self.ingress.accepted(accepted)?;
                accepted
            }
            Input::Gap { .. } | Input::Discontinuity => {
                if in_band && available == 0 {
                    return Ok(0);
                }
                if matches!(input, Input::Discontinuity) {
                    self.ingress.abandon(&mut completed)?;
                } else {
                    self.ingress.discontinuity(&mut completed)?;
                }
                if let Some(radio) = radio.as_mut() {
                    radio.control(self.ingress.muted(), None);
                    radio.input(input, now, receipt)?
                } else if let Input::Gap { count, .. } = input {
                    count as usize
                } else {
                    1
                }
            }
            Input::Adaptation { .. } => {
                if let Some(radio) = radio {
                    radio.input(input, now, receipt)?
                } else {
                    1
                }
            }
        };
        if failure.get() {
            Err(Error::Allocation)
        } else {
            Ok(credit)
        }
    }

    pub(super) fn revision(&self) -> Option<u64> {
        self.revision.get()
    }

    pub(super) fn settle(&mut self, radio: Option<&mut Radio>) {
        if Some(self.released.load(Ordering::Acquire)) == self.revision.get()
            && self.ingress.release()
        {
            if let Some(radio) = radio {
                radio.control(false, None);
            }
        }
    }
}
