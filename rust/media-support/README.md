# Independent file and speech providers (ABI 1)

This private Rust source set builds two independently replaceable shared objects:

| Capability | SONAME | C descriptor/header |
| --- | --- | --- |
| Local file decode | `librptadv_file_adapter.so.1` | `rptadv_file_adapter_descriptor`, `rptadv_file_adapter.h` |
| Speech synthesis | `librptadv_speech_adapter.so.1` | `rptadv_speech_adapter_descriptor`, `rptadv_speech_adapter.h` |

Each table exposes only its own preparation operation. Common ownership layouts
are in `include/rptadv_media_types.h`; there is no common runtime DSO or provider
dependency on the controller core. The `rlib` targets support tests only.
The initial-alpha split replaces the combined media descriptor rather than
retaining compatibility slots (ADR 0040).

The file provider opens one regular local input nonblocking, rejects FIFOs, and
passes the owned file as FFmpeg stdin. FFmpeg outputs mono normalized F32 WAV at
the source rate with a `file,pipe` protocol whitelist. It performs no sample-rate
conversion and does not invoke Piper.

The speech provider invokes only Piper with literal arguments and file-backed
text stdin. Speed remains 1–1000%, with reciprocal length scale truncated to six
fractional digits. It reads Piper's mono S16 WAV directly and applies speech-only
gain from −60 to 0 dB. It never invokes FFmpeg. Malformed or empty Piper output
retains the former decoder-stage `PROCESS_FAILED` result.
Neither provider downloads models, fetches network media, invokes a shell, or
implements fallback policy. The WAV reader accepts ordinary RIFF PCM16/F32 and
extensible F32; RF64 beyond the RIFF size limit is not supported.

Each call owns its children and private temporary directory. Each child has its
own monotonic timeout, normally 30 seconds. Cancellation kills and reaps the
owned child before releasing files or host reaper exclusion. Hosts with a
competing SIGCHLD reaper supply paired concurrency-safe acquire/release callbacks;
the exclusion runs from before spawn through final reap, including spawn failure.
Standalone hosts without a competing reaper may omit both callbacks.

The product-owned `NativeMediaPreparer` validates both readable descriptor
prefixes and every required slot before creating contexts. A failed second
creation destroys the first context. Foreign PCM is copied through the released
ring3 converter into core-owned 48 kHz audio, then released by its originating
provider. Finite conversion uses bounded zero context beyond the fastest-sinc
support, zero occupancy target, and exactly ceil(source_frames × 48000/source_rate)
output frames. Padding/concealment never extends playable duration.

The loader retains both selected DSOs and host callbacks until all calls,
contexts, and result handles finish. Replacement is controlled restart/module
reload, not active callback hot swap. Missing/incompatible selected descriptors
are composition errors. Individual media failures follow core file → speech →
Morse policy unchanged. Runtime packaging installs both versioned providers;
their separate development packages install the corresponding headers.

Run `cargo test -p rptadv-file-adapter -p rptadv-speech-adapter`, Clippy and Rustdoc
with warnings denied under Rust 1.85. Tests use installed FFmpeg and a local
compiled Piper fixture, without a model download or network access. Compile
`tests/fixtures/descriptor.c` twice: `-DFILE_ADAPTER` plus the file header path,
and without that define plus the speech header path; both need this source set's
`include` directory and `-ldl`. Execute each with its corresponding DSO to test
independent C loading and ownership. Debian 13 amd64/arm64 packaging and 100%
production source line/branch coverage remain the full pull-request gate.
