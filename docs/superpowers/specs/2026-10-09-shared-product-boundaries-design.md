# Shared product boundaries

Approved direction: 2026-10-09 user request and subsequent approval.

## Goal

Make USBRadioPlus and app_rpt_advanced thin integration adapters. Share their
existing implementation with standalone through versioned dynamic libraries,
without changing required behavior or implementing wishlist features.

## Ownership

- `librptadv_product` remains the sole controller implementation: configuration
  semantics, directory policy, node generations, linking, schedules, and telemetry.
- Extract `libusbradioplus_product` from USBRadioPlus's existing neutral driver
  descriptor and station implementation. This controller-free product owns
  radio configuration normalization, processing preparation, station/device
  lifecycle, tuning, and shared native radio composition. Build/package it in
  the USBRadioPlus repository, like the controller's existing product library.
- Keep `librptadvradio` portable and unchanged where its existing ABI suffices.
  It must not acquire configuration parsing, controller policy, or hardware/DSP
  implementation dependencies.
- Reuse the existing radio, ring, FFmpeg, RNNoise, conversion, audio, GPIO,
  control, speech, file, and IAX provider libraries. Do not merge their external
  APIs into a hardware facade or vendor their implementations.
- Asterisk adapters own registration, external objects/callbacks, CLI transport,
  channel/frame conversion, and Asterisk-specific resource management. ASL3's
  8 kHz compatibility behavior remains at that edge; native paths remain 48 kHz.
- Standalone retains process/service and backend-I/O integration, but does not
  embed a second copy of controller configuration policy or radio processing.

## Preservation constraints

Preserve configuration filenames, schemas, inheritance, defaults, gains, filter
responses, signaling, device selection, telemetry routing, and live-reload
semantics. The differing current standalone and USBRadioPlus processing inputs
are not permission to silently standardize their sound. Share graph preparation
while retaining each frontend's effective values and supported operations.

Use stable C-compatible descriptors, opaque ownership handles, explicit buffer
lengths, and callback lifetime contracts. No Rust-owned type or external native
object crosses a public product boundary. Validate incompatible artifacts before
calling function tables; retain provider code through callback quiescence.

Preserve bounded, allocation-free, lock-free native callbacks and existing ring
ownership. Extraction must not add a PCM queue, conversion, or processing delay.
Device access stays in the selected audio/GPIO adapters; the product composes
them without performing direct ALSA, PortAudio, or HID calls.

Support independent installation of standalone alone, USBRadioPlus alone, and
USBRadioPlus with app_rpt_advanced. A radio-library installation must not require
the controller, Asterisk, or ASL3. Preserve current secrets-file protections.

## Scope and approval

Preserve all existing uncommitted standalone/IAX repairs. No unrelated cleanup,
new controller features, new frontend tuning features, or wishlist extractions.
Developer documentation and affected packaging accompany implementation;
operator documentation is deferred until pull-request preparation.

Local refactoring is approved. Node deployment or switching runtime compositions
requires a separate explicit approval. Completion requires artifact-boundary,
behavior, reload, and independent-installation evidence, not just moved files.

Governing ADRs: 0005, 0012, 0018, 0020, 0021, 0022, 0025, 0026, 0027, 0028,
0029, 0035, 0036, 0038, and 0040.
