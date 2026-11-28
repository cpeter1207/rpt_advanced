//! Consume only fields belonging to the selected variant. Errors carry no input.
use super::ActionError;
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub(super) type Result<T> = std::result::Result<T, ActionError>;
pub(super) struct Fields<'a> {
    values: &'a Map<String, Value>,
    used: BTreeSet<&'static str>,
}
impl<'a> Fields<'a> {
    pub fn new(value: &'a Value) -> Result<Self> {
        Ok(Self {
            values: value.as_object().ok_or(ActionError::Invalid)?,
            used: BTreeSet::new(),
        })
    }
    pub fn take(&mut self, key: &'static str) -> Option<&'a Value> {
        self.used.insert(key);
        self.values.get(key)
    }
    pub fn get(&mut self, key: &'static str) -> Result<&'a Value> {
        self.take(key).ok_or(ActionError::Field(key))
    }
    pub fn text(&mut self, key: &'static str) -> Result<&'a str> {
        self.get(key)?.as_str().ok_or(ActionError::Field(key))
    }
    pub fn flag(&mut self, key: &'static str) -> Result<bool> {
        self.get(key)?.as_bool().ok_or(ActionError::Field(key))
    }
    pub fn uint(&mut self, key: &'static str, min: u64, max: u64) -> Result<u64> {
        self.get(key)?
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or(ActionError::Field(key))
    }
    pub fn choice(&mut self, key: &'static str, options: &[&str]) -> Result<usize> {
        let value = self.text(key)?;
        options
            .iter()
            .position(|s| *s == value)
            .ok_or(ActionError::Field(key))
    }
    pub fn finish(self) -> Result<()> {
        if self.values.keys().all(|k| self.used.contains(k.as_str())) {
            Ok(())
        } else {
            Err(ActionError::Extra)
        }
    }
    pub fn scaled(&mut self, key: &'static str, places: i32, min: i128, max: i128) -> Result<i128> {
        let number = self.get(key)?.as_number().ok_or(ActionError::Field(key))?;
        scaled(number.as_str(), places)
            .filter(|n| (min..=max).contains(n))
            .ok_or(ActionError::Field(key))
    }
    pub fn digit(&mut self, key: &'static str) -> Result<char> {
        let text = self.text(key)?;
        text.chars()
            .next()
            .filter(|c| text.len() == 1 && crate::link::valid_digit(*c))
            .ok_or(ActionError::Field(key))
    }
    pub fn duration(&mut self, min: u64, max: u64) -> Result<u64> {
        let mut timing = Self::new(self.get("timing")?)?;
        let duration = timing.uint("duration_ms", min, max)?;
        timing.finish()?;
        Ok(duration)
    }
}

/// Convert the original JSON numeric spelling exactly, including exponent notation.
/// Convert an exact JSON decimal spelling to an integer at the requested decimal scale.
pub fn scaled(text: &str, places: i32) -> Option<i128> {
    let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
    let negative = mantissa.starts_with('-');
    let (whole, fraction) = mantissa
        .trim_start_matches('-')
        .split_once('.')
        .unwrap_or((mantissa.trim_start_matches('-'), ""));
    let digits = format!("{whole}{fraction}");
    let significant = digits.trim_start_matches('0').trim_end_matches('0');
    if significant.is_empty() {
        return Some(0);
    }
    let shift = exponent
        .parse::<i32>()
        .ok()?
        .checked_add(places)?
        .checked_sub(i32::try_from(fraction.len()).ok()?)?
        .checked_add(i32::try_from(digits.len() - digits.trim_end_matches('0').len()).ok()?)?;
    let n = significant
        .parse::<i128>()
        .ok()?
        .checked_mul(10_i128.checked_pow(shift.try_into().ok()?)?)?;
    Some(if negative { -n } else { n })
}
