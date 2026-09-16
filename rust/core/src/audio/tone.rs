//! Streaming 48 kHz courtesy-tone sequence renderer.

use super::AudioSource;

/// Fixed native audio rate used by tone rendering.
const SAMPLE_RATE_HZ: usize = 48_000;
const MAX_SEGMENTS: usize = 256;
const MAX_DURATION_MS: usize = 60_000;
const MAX_SAMPLES: usize = 5_760_000;

/// A rejected tone sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToneError;

struct Segment {
    first_hz: f32,
    second_hz: f32,
    samples: usize,
    level_db: i8,
}

/// A fully prepared tone sequence whose renderer performs no allocation.
pub struct ToneSequence {
    audio: Vec<f32>,
    offset: usize,
}

impl ToneSequence {
    /// Parse and prepare one bounded tone sequence at the fixed native rate.
    pub fn new(text: &str, default_level_db: i8) -> Result<Self, ToneError> {
        if !(-60..=0).contains(&default_level_db) || text.trim().is_empty() {
            return Err(ToneError);
        }
        let mut segments = Vec::new();
        for part in text.split(',') {
            if part.trim().is_empty() || segments.len() == MAX_SEGMENTS {
                return Err(ToneError);
            }
            segments.push(parse_segment(part, default_level_db)?);
        }
        let total = segments
            .iter()
            .try_fold(0_usize, |total, segment| {
                total
                    .checked_add(segment.samples)
                    .filter(|total| *total <= MAX_SAMPLES)
            })
            .ok_or(ToneError)?;
        let mut audio = Vec::with_capacity(total);
        let mut first_phase = 0.0;
        let mut second_phase = 0.0;
        for segment in segments {
            let amplitude = 10_f32.powf(f32::from(segment.level_db) / 20.0)
                / if segment.second_hz == 0.0 { 1.0 } else { 2.0 };
            for _ in 0..segment.samples {
                let mut sample = 0.0;
                if segment.first_hz != 0.0 {
                    sample += amplitude * (core::f32::consts::TAU * first_phase).sin();
                    first_phase = (first_phase + segment.first_hz / SAMPLE_RATE_HZ as f32).fract();
                }
                if segment.second_hz != 0.0 {
                    sample += amplitude * (core::f32::consts::TAU * second_phase).sin();
                    second_phase =
                        (second_phase + segment.second_hz / SAMPLE_RATE_HZ as f32).fract();
                }
                audio.push(sample);
            }
        }
        Ok(Self { audio, offset: 0 })
    }

    /// Return the fixed prepared sample count.
    pub fn rendered_samples(&self) -> usize {
        self.audio.len()
    }

    /// Render as many prepared samples as fit, returning the number written.
    pub fn render(&mut self, output: &mut [f32]) -> usize {
        let count = output.len().min(self.audio.len() - self.offset);
        output[..count].copy_from_slice(&self.audio[self.offset..self.offset + count]);
        self.offset += count;
        count
    }
}

impl AudioSource for ToneSequence {
    fn render(&mut self, output: &mut [f32]) -> usize {
        self.render(output)
    }
}

fn parse_segment(text: &str, default_level_db: i8) -> Result<Segment, ToneError> {
    let fields: Vec<_> = text.split('/').map(str::trim).collect();
    if !(2..=3).contains(&fields.len()) || fields.iter().any(|field| field.is_empty()) {
        return Err(ToneError);
    }
    let (frequencies, compact_level) = match fields[0].split_once('@') {
        Some((frequencies, level)) if !level.contains('@') => {
            (frequencies.trim(), Some(parse_level(level)?))
        }
        Some(_) => return Err(ToneError),
        None => (fields[0], None),
    };
    if compact_level.is_some() && fields.len() == 3 {
        return Err(ToneError);
    }
    let (first_hz, second_hz) = match frequencies.split_once('+') {
        Some((first, second)) if !second.contains('+') => {
            let first_hz = parse_frequency(first)?;
            let second_hz = parse_frequency(second)?;
            if first_hz == 0.0 || second_hz == 0.0 {
                return Err(ToneError);
            }
            (first_hz, second_hz)
        }
        Some(_) => return Err(ToneError),
        None => (parse_frequency(frequencies)?, 0.0),
    };
    if first_hz >= 24_000.0 || second_hz >= 24_000.0 {
        return Err(ToneError);
    }
    let duration_ms = parse_duration(fields[1])?;
    let level_db = compact_level
        .or_else(|| fields.get(2).and_then(|level| parse_level(level).ok()))
        .unwrap_or(default_level_db);
    if fields.len() == 3 && parse_level(fields[2]).is_err() {
        return Err(ToneError);
    }
    Ok(Segment {
        first_hz,
        second_hz,
        samples: duration_ms * 48,
        level_db,
    })
}

fn parse_frequency(text: &str) -> Result<f32, ToneError> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("silence") {
        return Ok(0.0);
    }
    let text = strip_suffix_case(text, "hz").unwrap_or(text).trim();
    if text.is_empty()
        || text.len() > 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || text.bytes().filter(|byte| *byte == b'.').count() > 1
    {
        return Err(ToneError);
    }
    let frequency: f32 = text.parse().map_err(|_| ToneError)?;
    frequency.is_finite().then_some(frequency).ok_or(ToneError)
}

fn parse_duration(text: &str) -> Result<usize, ToneError> {
    let text = strip_suffix_case(text.trim(), "ms")
        .unwrap_or(text.trim())
        .trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ToneError);
    }
    let duration = text.parse().map_err(|_| ToneError)?;
    (1..=MAX_DURATION_MS)
        .contains(&duration)
        .then_some(duration)
        .ok_or(ToneError)
}

fn parse_level(text: &str) -> Result<i8, ToneError> {
    let text = strip_suffix_case(text.trim(), "dbfs")
        .or_else(|| strip_suffix_case(text.trim(), "db"))
        .unwrap_or(text.trim())
        .trim();
    let level: i8 = text.parse().map_err(|_| ToneError)?;
    (-60..=0).contains(&level).then_some(level).ok_or(ToneError)
}

fn strip_suffix_case<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let split = text.len().checked_sub(suffix.len())?;
    let (prefix, found) = text.split_at_checked(split)?;
    found.eq_ignore_ascii_case(suffix).then_some(prefix)
}

#[cfg(test)]
#[path = "tone_tests.rs"]
mod tests;
