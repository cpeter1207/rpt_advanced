//! Streaming 48 kHz Morse renderer.

use super::AudioSource;

/// Fixed native audio rate used by Morse rendering.
const SAMPLE_RATE_HZ: u64 = 48_000;

/// A rejected Morse configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MorseError;

#[derive(Clone, Copy)]
enum Segment {
    Tone(u64),
    Gap(u64),
}

/// Prevalidated, allocation-free-at-render-time Morse audio source.
pub struct MorseRenderer {
    segments: Vec<Segment>,
    segment: usize,
    remaining: u64,
    keyed: bool,
    phase: f32,
    step: f32,
    amplitude: f32,
}

impl MorseRenderer {
    /// Validate and prepare one ASCII Morse message for fixed 48 kHz rendering.
    pub fn new(
        text: &str,
        speed_wpm: u32,
        frequency_hz: f32,
        level_db: i8,
    ) -> Result<Self, MorseError> {
        if !(1..=100).contains(&speed_wpm)
            || !frequency_hz.is_finite()
            || frequency_hz <= 0.0
            || frequency_hz >= 24_000.0
            || !(-60..=0).contains(&level_db)
        {
            return Err(MorseError);
        }
        let words: Vec<_> = text
            .split(is_separator)
            .filter(|word| !word.is_empty())
            .collect();
        if words
            .iter()
            .any(|word| word.chars().any(|character| pattern(character).is_none()))
        {
            return Err(MorseError);
        }
        let denominator = u64::from(5 * speed_wpm);
        let mut fraction = 0;
        let mut segments = Vec::new();
        for (word_index, word) in words.iter().enumerate() {
            for (character_index, character) in word.chars().enumerate() {
                let code = pattern(character).expect("validated Morse character");
                for (symbol_index, symbol) in code.bytes().enumerate() {
                    segments.push(Segment::Tone(samples(
                        symbol == b'.',
                        denominator,
                        &mut fraction,
                    )));
                    if symbol_index + 1 != code.len() {
                        segments.push(Segment::Gap(samples_units(1, denominator, &mut fraction)));
                    }
                }
                if character_index + 1 != word.len() {
                    segments.push(Segment::Gap(samples_units(3, denominator, &mut fraction)));
                }
            }
            if word_index + 1 != words.len() {
                segments.push(Segment::Gap(samples_units(7, denominator, &mut fraction)));
            }
        }
        Ok(Self {
            segments,
            segment: 0,
            remaining: 0,
            keyed: false,
            phase: 0.0,
            step: frequency_hz / SAMPLE_RATE_HZ as f32,
            amplitude: 10_f32.powf(f32::from(level_db) / 20.0),
        })
    }

    /// Render as many samples as fit, returning the number written.
    pub fn render(&mut self, output: &mut [f32]) -> usize {
        let mut written = 0;
        while written != output.len() {
            if self.remaining == 0 {
                let Some(segment) = self.segments.get(self.segment).copied() else {
                    break;
                };
                self.segment += 1;
                match segment {
                    Segment::Tone(samples) => {
                        self.keyed = true;
                        self.phase = 0.0;
                        self.remaining = samples;
                    }
                    Segment::Gap(samples) => {
                        self.keyed = false;
                        self.remaining = samples;
                    }
                }
            }
            output[written] = if self.keyed {
                let sample = self.amplitude * (core::f32::consts::TAU * self.phase).sin();
                self.phase = (self.phase + self.step).fract();
                sample
            } else {
                0.0
            };
            self.remaining -= 1;
            written += 1;
        }
        written
    }

    /// Reset this validated renderer to the start without allocating.
    pub fn restart(&mut self) {
        self.segment = 0;
        self.remaining = 0;
        self.keyed = false;
        self.phase = 0.0;
    }
}

impl AudioSource for MorseRenderer {
    fn render(&mut self, output: &mut [f32]) -> usize {
        self.render(output)
    }
}

fn is_separator(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

fn samples(dot: bool, denominator: u64, fraction: &mut u64) -> u64 {
    samples_units(if dot { 1 } else { 3 }, denominator, fraction)
}

fn samples_units(units: u64, denominator: u64, fraction: &mut u64) -> u64 {
    let numerator = units * SAMPLE_RATE_HZ * 6 + *fraction;
    *fraction = numerator % denominator;
    numerator / denominator
}

fn pattern(character: char) -> Option<&'static str> {
    match character.to_ascii_uppercase() {
        'A' => Some(".-"),
        'B' => Some("-..."),
        'C' => Some("-.-."),
        'D' => Some("-.."),
        'E' => Some("."),
        'F' => Some("..-."),
        'G' => Some("--."),
        'H' => Some("...."),
        'I' => Some(".."),
        'J' => Some(".---"),
        'K' => Some("-.-"),
        'L' => Some(".-.."),
        'M' => Some("--"),
        'N' => Some("-."),
        'O' => Some("---"),
        'P' => Some(".--."),
        'Q' => Some("--.-"),
        'R' => Some(".-."),
        'S' => Some("..."),
        'T' => Some("-"),
        'U' => Some("..-"),
        'V' => Some("...-"),
        'W' => Some(".--"),
        'X' => Some("-..-"),
        'Y' => Some("-.--"),
        'Z' => Some("--.."),
        '0' => Some("-----"),
        '1' => Some(".----"),
        '2' => Some("..---"),
        '3' => Some("...--"),
        '4' => Some("....-"),
        '5' => Some("....."),
        '6' => Some("-...."),
        '7' => Some("--..."),
        '8' => Some("---.."),
        '9' => Some("----."),
        '/' => Some("-..-."),
        '.' => Some(".-.-.-"),
        ',' => Some("--..--"),
        '?' => Some("..--.."),
        '-' => Some("-....-"),
        '=' => Some("-...-"),
        '+' => Some(".-.-."),
        '@' => Some(".--.-."),
        '(' => Some("-.--."),
        ')' => Some("-.--.-"),
        '\'' => Some(".----."),
        '!' => Some("-.-.--"),
        '"' => Some(".-..-."),
        ':' => Some("---..."),
        ';' => Some("-.-.-."),
        '_' => Some("..--.-"),
        '$' => Some("...-..-"),
        '&' => Some(".-..."),
        _ => None,
    }
}

#[cfg(test)]
#[path = "morse_tests.rs"]
mod tests;
