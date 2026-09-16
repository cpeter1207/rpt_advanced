//! Fixed-state 48 kHz DTMF detection.

const INTERVAL_SAMPLES: usize = 612;
const ROWS: [f32; 4] = [697.0, 770.0, 852.0, 941.0];
const COLUMNS: [f32; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
const MINIMUM_POWER: f64 = 8.0e7 * 36.0 / (32_767.0 * 32_767.0);
const TOTAL_ENERGY_RATIO: f64 = 42.0 * 6.0;
const REVERSE_TWIST: f64 = 2.51;
const NORMAL_TWIST: f64 = 6.31;
const DOMINANCE: f64 = 6.3;

/// A recognized standard DTMF digit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DtmfDigit {
    /// Digit 1.
    One,
    /// Digit 2.
    Two,
    /// Digit 3.
    Three,
    /// Digit A.
    A,
    /// Digit 4.
    Four,
    /// Digit 5.
    Five,
    /// Digit 6.
    Six,
    /// Digit B.
    B,
    /// Digit 7.
    Seven,
    /// Digit 8.
    Eight,
    /// Digit 9.
    Nine,
    /// Digit C.
    C,
    /// Star.
    Star,
    /// Digit 0.
    Zero,
    /// Hash.
    Hash,
    /// Digit D.
    D,
}

impl DtmfDigit {
    /// Return this digit's low- and high-group frequencies in hertz.
    #[must_use]
    pub fn frequencies(self) -> (f32, f32) {
        let index = match self {
            Self::One => 0,
            Self::Two => 1,
            Self::Three => 2,
            Self::A => 3,
            Self::Four => 4,
            Self::Five => 5,
            Self::Six => 6,
            Self::B => 7,
            Self::Seven => 8,
            Self::Eight => 9,
            Self::Nine => 10,
            Self::C => 11,
            Self::Star => 12,
            Self::Zero => 13,
            Self::Hash => 14,
            Self::D => 15,
        };
        (ROWS[index / 4], COLUMNS[index % 4])
    }
}

#[derive(Clone, Copy)]
struct Filter {
    coefficient: f64,
    previous: f64,
    before: f64,
}

/// Fixed-storage DTMF detector for canonical 48 kHz PCM.
pub struct DtmfDetector {
    filters: [Filter; 8],
    samples: usize,
    energy: f64,
    active: Option<DtmfDigit>,
    last_hit: Option<DtmfDigit>,
    hits: u8,
    misses: u8,
    muting: bool,
    suppressing: bool,
}

impl DtmfDetector {
    /// Construct a detector, selecting whether qualified DTMF audio is muted.
    #[must_use]
    pub fn new(muting: bool) -> Self {
        let frequencies = [
            ROWS[0], ROWS[1], ROWS[2], ROWS[3], COLUMNS[0], COLUMNS[1], COLUMNS[2], COLUMNS[3],
        ];
        Self {
            filters: frequencies.map(|frequency| Filter {
                coefficient: 2.0 * (core::f64::consts::TAU * f64::from(frequency) / 48_000.0).cos(),
                previous: 0.0,
                before: 0.0,
            }),
            samples: 0,
            energy: 0.0,
            active: None,
            last_hit: None,
            hits: 0,
            misses: 0,
            muting,
            suppressing: false,
        }
    }

    /// Select whether qualified DTMF audio is muted.
    pub fn set_muting(&mut self, enabled: bool) {
        self.muting = enabled;
    }

    /// Process one arbitrary PCM partition, emitting every completed digit in order.
    ///
    /// Work and callback count are bounded by the supplied sample count. The callback
    /// must remain allocation-free, lock-free and nonblocking on an audio worker.
    /// Muting starts after two matching analysis intervals qualify a digit and ends
    /// after the first unlike interval. It adds no lookback delay, so the qualifying
    /// prefix can pass and at most 612 post-tone samples (12.75 ms) can be silenced;
    /// later completion audio is never muted.
    pub fn process(&mut self, receiving: bool, audio: &mut [f32], mut emit: impl FnMut(DtmfDigit)) {
        for sample in &mut *audio {
            let input = if receiving { f64::from(*sample) } else { 0.0 };
            if receiving && self.muting && self.suppressing {
                *sample = 0.0;
            }
            self.energy += input * input;
            for filter in &mut self.filters {
                let current = filter.coefficient * filter.previous - filter.before + input;
                filter.before = filter.previous;
                filter.previous = current;
            }
            self.samples += 1;
            if self.samples == INTERVAL_SAMPLES {
                if let Some(digit) = self.finish_interval() {
                    emit(digit);
                }
            }
        }
    }

    fn finish_interval(&mut self) -> Option<DtmfDigit> {
        let hit = self.classify();
        let mut completed = None;
        if self.active == hit {
            self.misses = 0;
        } else if self.active.is_some() {
            self.misses += 1;
            if self.misses == 3 {
                completed = self.active;
                self.active = None;
            }
        }
        if hit != self.last_hit {
            self.last_hit = hit;
            self.hits = 0;
        }
        if hit.is_some() && hit != self.active {
            self.hits += 1;
            if self.hits == 2 {
                if self.active.is_some() {
                    completed = self.active;
                }
                self.active = hit;
                self.misses = 0;
            }
        }
        self.suppressing = hit.is_some() && hit == self.active;
        self.samples = 0;
        self.energy = 0.0;
        for filter in &mut self.filters {
            filter.previous = 0.0;
            filter.before = 0.0;
        }
        completed
    }

    fn classify(&self) -> Option<DtmfDigit> {
        let powers = self.filters.map(|filter| {
            filter.previous * filter.previous + filter.before * filter.before
                - filter.coefficient * filter.previous * filter.before
        });
        let row = strongest(&powers[..4]);
        let column = 4 + strongest(&powers[4..]);
        let row_power = powers[row];
        let column_power = powers[column];
        let valid = row_power >= MINIMUM_POWER
            && column_power >= MINIMUM_POWER
            && column_power < row_power * REVERSE_TWIST
            && row_power < column_power * NORMAL_TWIST
            && row_power + column_power > TOTAL_ENERGY_RATIO * self.energy
            && powers[..4]
                .iter()
                .enumerate()
                .all(|(index, power)| index == row || power * DOMINANCE <= row_power)
            && powers[4..]
                .iter()
                .enumerate()
                .all(|(index, power)| index + 4 == column || power * DOMINANCE <= column_power);
        valid.then_some(digit_at(row, column - 4))
    }
}

fn strongest(powers: &[f64]) -> usize {
    powers
        .iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| left.total_cmp(right))
        .map_or(0, |(index, _)| index)
}

fn digit_at(row: usize, column: usize) -> DtmfDigit {
    const DIGITS: [[DtmfDigit; 4]; 4] = [
        [
            DtmfDigit::One,
            DtmfDigit::Two,
            DtmfDigit::Three,
            DtmfDigit::A,
        ],
        [
            DtmfDigit::Four,
            DtmfDigit::Five,
            DtmfDigit::Six,
            DtmfDigit::B,
        ],
        [
            DtmfDigit::Seven,
            DtmfDigit::Eight,
            DtmfDigit::Nine,
            DtmfDigit::C,
        ],
        [
            DtmfDigit::Star,
            DtmfDigit::Zero,
            DtmfDigit::Hash,
            DtmfDigit::D,
        ],
    ];
    DIGITS[row][column]
}

#[cfg(test)]
#[path = "dtmf_tests.rs"]
mod tests;
