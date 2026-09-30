//! Deterministic streaming Piper stand-in; no model or network is involved.
use std::{io::{self, Read, Write}, time::Duration};

fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 6);
    assert_eq!((&*args[1], &*args[3], &*args[4]), ("--model", "--output_raw", "--length_scale"));
    let model = std::path::Path::new(&args[2]);
    assert_eq!(args[5], model.file_stem().unwrap().to_string_lossy());
    let mut text = String::new();
    io::stdin().read_to_string(&mut text).unwrap();
    if text == "fail" { std::process::exit(23); }
    if text == "invalid" { io::stdout().write_all(&[0]).unwrap(); return; }
    if text == "empty" { return; }
    if let Some(marker) = text.strip_prefix("wait=") {
        std::fs::write(marker, std::process::id().to_string()).unwrap();
        loop { std::thread::sleep(Duration::from_secs(1)); }
    }
    assert_eq!(text, "Identifier; $(not a command)\n");
    let mut output = io::stdout().lock();
    for _ in 0..2205 { output.write_all(&1000_i16.to_le_bytes()).unwrap(); }
}
