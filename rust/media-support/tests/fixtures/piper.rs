//! A deterministic child process; no external model or network is involved.
use std::{fs, io::{self, Read}, time::Duration};
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 7);
    assert_eq!((&*args[1], &*args[3], &*args[5]), ("--model", "--output_file", "--length_scale"));
    let model = &args[2];
    if model == "fail" { std::process::exit(23); }
    if let Some(marker) = model.strip_prefix("wait=") {
        fs::write(marker, std::process::id().to_string()).unwrap();
        loop { std::thread::sleep(Duration::from_secs(1)); }
    }
    if model == "invalid" { fs::write(&args[4], b"bad audio").unwrap(); return; }
    if model == "empty" { return; }
    assert_eq!(args[6], *model);
    let mut text = String::new();
    io::stdin().read_to_string(&mut text).unwrap();
    assert_eq!(text, "Identifier; $(not a command)\n");
    let mut wave = Vec::from(*b"RIFF\x5e\x11\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x22\x56\0\0\x44\xac\0\0\x02\0\x10\0data\x3a\x11\0\0");
    for _ in 0..2205 { wave.extend_from_slice(&1000_i16.to_le_bytes()); }
    fs::write(&args[4], wave).unwrap();
}
