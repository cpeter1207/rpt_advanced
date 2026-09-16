use super::*;

fn chunk(name: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::from(*name);
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    if data.len() % 2 != 0 {
        bytes.push(0);
    }
    bytes
}

fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::from(*b"RIFF\0\0\0\0WAVE");
    for chunk in chunks {
        bytes.extend_from_slice(chunk);
    }
    let length = bytes.len() as u32 - 8;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes
}

fn wave(data: &[u8]) -> Vec<u8> {
    riff(&[
        chunk(
            b"fmt ",
            b"\x03\0\x01\0\x22\x56\0\0\x88\x58\x01\0\x04\0\x20\0",
        ),
        chunk(b"data", data),
    ])
}
#[test]
fn rejects_truncated_partial_empty_nonfinite_and_mismatched_format_output() {
    let valid = wave(&[0, 0, 128, 191, 0, 254, 127, 63]);
    assert_eq!(parse(&valid).unwrap().samples(), &[-1.0, 0.9999695]);
    for length in 0..valid.len() {
        assert_eq!(parse(&valid[..length]), Err(MediaError::InvalidOutput));
    }
    for data in [
        vec![],
        vec![1],
        vec![1, 2],
        vec![1, 2, 3],
        f32::NAN.to_le_bytes().to_vec(),
        f32::INFINITY.to_le_bytes().to_vec(),
    ] {
        assert_eq!(parse(&wave(&data)), Err(MediaError::InvalidOutput));
    }
    for (offset, replacement) in [(20, 1), (22, 2), (24, 0), (28, 0), (32, 2), (34, 16)] {
        let mut invalid = valid.clone();
        invalid[offset] = replacement;
        assert_eq!(parse(&invalid), Err(MediaError::InvalidOutput));
    }
}

#[test]
fn reads_piper_pcm16_endpoints_and_rejects_partial_samples() {
    let format = chunk(b"fmt ", b"\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0");
    let audio = parse(&riff(&[
        format.clone(),
        chunk(b"data", &[0, 128, 0, 0, 255, 127]),
    ]))
    .unwrap();
    assert_eq!(audio.sample_rate_hz(), 22050);
    assert_eq!(audio.samples(), &[-1.0, 0.0, 32767.0 / 32768.0]);
    assert_eq!(
        parse(&riff(&[format, chunk(b"data", &[1])])),
        Err(MediaError::InvalidOutput)
    );
}

#[test]
fn accepts_extensible_float_and_ignores_unknown_chunks() {
    let format = b"\xfe\xff\x01\0\x80\xbb\0\0\0\xee\x02\0\x04\0\x20\0\x16\0\x20\0\0\0\0\0\x03\0\0\0\0\0\x10\0\x80\0\0\xaa\0\x38\x9b\x71";
    let bytes = riff(&[
        chunk(b"JUNK", b"x"),
        chunk(b"fmt ", format),
        chunk(b"data", &0.25_f32.to_le_bytes()),
    ]);
    let audio = parse(&bytes).unwrap();
    assert_eq!(audio.sample_rate_hz(), 48_000);
    assert_eq!(audio.samples(), &[0.25]);
}

#[test]
fn rejects_duplicate_chunks_and_missing_odd_padding() {
    let format = chunk(
        b"fmt ",
        b"\x03\0\x01\0\x22\x56\0\0\x88\x58\x01\0\x04\0\x20\0",
    );
    let data = chunk(b"data", &0.0_f32.to_le_bytes());
    for chunks in [
        vec![format.clone(), format.clone(), data.clone()],
        vec![format.clone(), data.clone(), data.clone()],
    ] {
        assert_eq!(parse(&riff(&chunks)), Err(MediaError::InvalidOutput));
    }

    let mut missing_padding = riff(&[format, chunk(b"JUNK", b"x")]);
    missing_padding.pop();
    let length = missing_padding.len() as u32 - 8;
    missing_padding[4..8].copy_from_slice(&length.to_le_bytes());
    assert_eq!(parse(&missing_padding), Err(MediaError::InvalidOutput));
}

#[test]
fn rejects_invalid_headers_and_extensible_float_variants() {
    let mut invalid_riff = wave(&0.0_f32.to_le_bytes());
    invalid_riff[..4].copy_from_slice(b"NOPE");
    assert_eq!(parse(&invalid_riff), Err(MediaError::InvalidOutput));

    let mut invalid_wave = wave(&0.0_f32.to_le_bytes());
    invalid_wave[8..12].copy_from_slice(b"NOPE");
    assert_eq!(parse(&invalid_wave), Err(MediaError::InvalidOutput));

    assert_eq!(
        parse(&riff(&[
            chunk(b"fmt ", b"short"),
            chunk(b"data", &0.0_f32.to_le_bytes()),
        ])),
        Err(MediaError::InvalidOutput)
    );

    let valid = b"\xfe\xff\x01\0\x80\xbb\0\0\0\xee\x02\0\x04\0\x20\0\x16\0\x20\0\0\0\0\0\x03\0\0\0\0\0\x10\0\x80\0\0\xaa\0\x38\x9b\x71";
    for format in [
        valid[..39].to_vec(),
        {
            let mut value = valid.to_vec();
            value[16] = 21;
            value
        },
        {
            let mut value = valid.to_vec();
            value[18] = 31;
            value
        },
    ] {
        assert_eq!(
            parse(&riff(&[
                chunk(b"fmt ", &format),
                chunk(b"data", &0.0_f32.to_le_bytes()),
            ])),
            Err(MediaError::InvalidOutput)
        );
    }
}
