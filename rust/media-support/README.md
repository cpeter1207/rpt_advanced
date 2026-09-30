# Independent file and speech providers (streaming ABI 2)

This private Rust source set builds two independently replaceable shared objects:

| Capability | SONAME | C descriptor/header |
| --- | --- | --- |
| Local file decode | `librptadv_file_adapter.so.1` | `rptadv_file_adapter_descriptor`, `rptadv_file_adapter.h` |
| Speech synthesis | `librptadv_speech_adapter.so.1` | `rptadv_speech_adapter_descriptor`, `rptadv_speech_adapter.h` |

Each descriptor exposes its own open operation and shared bounded read/close
operations. Common ownership layouts are in `include/rptadv_media_types.h`;
there is no common runtime DSO or provider dependency on the controller core.
The `rlib` targets support tests only. The initial-alpha split replaces the
combined media descriptor rather than retaining compatibility slots (ADR 0040).

The file provider opens one regular local input nonblocking and gives that open
file to FFmpeg on stdin. FFmpeg emits mono normalized F32 WAV at the source
rate; the adapter parses its header and returns samples incrementally from the
child's stdout. It performs no sample-rate conversion and does not invoke
Piper.

The speech provider invokes only Piper with literal arguments and text on
stdin. It reads the configured rate from the model's adjacent `.onnx.json`
file, requests raw mono S16 stdout, converts samples to normalized `f32`, and
applies speech-only gain from −60 to 0 dB. Speed remains 1–1000%, represented
as reciprocal length scale truncated to six fractional digits. It never
invokes FFmpeg. Malformed PCM reports `INVALID_OUTPUT`; an unsuccessful or
empty Piper process reports `PROCESS_FAILED`.

Both providers expose source-rate PCM in bounded reads. They do not decode to a
temporary WAV or retain the complete decoded recording in provider-owned
memory. A stream owns its child until end, cancellation, timeout, or close;
each child has its own monotonic timeout, normally 30 seconds. Cancellation
kills and reaps the owned child before releasing host reaper exclusion. Hosts
with a competing SIGCHLD reaper supply paired concurrency-safe acquire/release
callbacks; the exclusion spans spawn through final reap, including spawn
failure. Standalone hosts without a competing reaper may omit both callbacks.

The product validates both readable descriptor prefixes and every required
slot before creating contexts. A failed second creation destroys the first
context. The product owns conversion from each stream's source rate to native
48 kHz through the released ring3 component; rate conversion does not belong in
these providers. Missing or incompatible selected descriptors are composition
errors. Individual media failures retain the core file → speech → Morse
fallback policy. Runtime packaging installs both versioned providers; their
separate development packages install the corresponding headers.

Run `cargo test -p rptadv-file-adapter -p rptadv-speech-adapter`, Clippy, and
Rustdoc with warnings denied under Rust 1.85. Tests use installed FFmpeg and a
locally compiled Piper fixture, without model downloads or network access.
Compile `tests/fixtures/descriptor.c` twice: with `-DFILE_ADAPTER` and the file
header path, and without that define and with the speech header path; both need
this source set's `include` directory and `-ldl`. Execute each with its
corresponding DSO to test independent C loading and ownership. Debian 13
amd64/arm64 packaging and 100% production-source line/branch coverage remain
the full pull-request gate.
