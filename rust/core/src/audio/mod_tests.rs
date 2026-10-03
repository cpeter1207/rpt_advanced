use super::{PcmRead, PcmStreamReader};

struct MinimalReader;

impl PcmStreamReader for MinimalReader {
    fn render(&mut self, _: &mut [f32]) -> PcmRead {
        PcmRead::Pending
    }
}

#[test]
fn optional_stream_controls_default_to_no_fallback_and_nonblocking_noop() {
    let mut reader = MinimalReader;
    reader.start();
    assert!(!reader.select_morse_fallback());
    reader.cancel();
}
