//! Read mono F32 decoder output and mono S16 synthesizer output.
use crate::{MediaError, result::PreparedAudio};

pub(crate) fn parse(bytes: &[u8]) -> Result<PreparedAudio, MediaError> {
    let invalid = MediaError::InvalidOutput;
    if bytes.len() < 12
        || &bytes[..4] != b"RIFF"
        || &bytes[8..12] != b"WAVE"
        || u32_at(bytes, 4)? as u64 + 8 != bytes.len() as u64
    {
        return Err(invalid);
    }
    let mut offset = 12;
    let mut format = None;
    let mut data = None;
    while offset < bytes.len() {
        let header = bytes.get(offset..offset + 8).ok_or(invalid)?;
        let length = u32_at(header, 4)? as usize;
        offset += 8;
        let end = offset.checked_add(length).ok_or(invalid)?;
        let chunk = bytes.get(offset..end).ok_or(invalid)?;
        match &header[..4] {
            b"fmt " => {
                if format.is_some() || chunk.len() < 16 {
                    return Err(invalid);
                }
                let encoding = u16_at(chunk, 0)?;
                let is_float = encoding == 3
                    || (encoding == 0xfffe
                        && chunk.len() >= 40
                        && u16_at(chunk, 16)? >= 22
                        && u16_at(chunk, 18)? == 32
                        && &chunk[24..40] == b"\x03\0\0\0\0\0\x10\0\x80\0\0\xaa\0\x38\x9b\x71");
                let sample_rate = u32_at(chunk, 4)?;
                let width = if is_float {
                    4
                } else if encoding == 1 {
                    2
                } else {
                    return Err(invalid);
                };
                if u16_at(chunk, 2)? != 1
                    || u16_at(chunk, 12)? != width
                    || u16_at(chunk, 14)? != width * 8
                    || u32_at(chunk, 8)? as u64 != sample_rate as u64 * u64::from(width)
                {
                    return Err(invalid);
                }
                format = Some((sample_rate, usize::from(width)));
            }
            b"data" => {
                if data.replace(chunk).is_some() {
                    return Err(invalid);
                }
            }
            _ => {}
        }
        offset = end.checked_add(length % 2).ok_or(invalid)?;
        if offset > bytes.len() {
            return Err(invalid);
        }
    }
    let data = data.ok_or(invalid)?;
    let (rate, width) = format.ok_or(invalid)?;
    if data.len() % width != 0 {
        return Err(invalid);
    }
    let mut samples = Vec::with_capacity(data.len() / width);
    for sample in data.chunks_exact(width) {
        samples.push(if width == 2 {
            f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0
        } else {
            f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]])
        });
    }
    PreparedAudio::new(rate, samples)
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, MediaError> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or(MediaError::InvalidOutput)?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, MediaError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(MediaError::InvalidOutput)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

#[cfg(test)]
#[path = "wave_tests.rs"]
mod tests;
