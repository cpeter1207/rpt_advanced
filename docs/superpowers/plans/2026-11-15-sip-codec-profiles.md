# SIP negotiated codec profiles

The owner's codec-parity request reuses the existing released codec providers.
No new external dependency, codec implementation, deployment or public push is
part of this local checkpoint. Baseline: `1285f8f56acfc7e6a84ed95741f2885e23fde69a`.

1. Introduce explicit provider/mode, PCM/RTP clocks, packet bounds and directional
   payload mapping in standalone phone media. Reuse the immutable shared codec
   client rather than maintaining duplicate descriptor/context code. Preserve
   PCMU/PCMA-only negotiation and all current G.711 behavior.
2. Prove the neutral change with the existing timed RTP, call-media exact credit,
   provider fault/cleanup, SDP/socket and native jitter suites; run scoped strict
   checks and commit this independent source checkpoint.
3. Add G.722 tests for 320 PCM frames, 160 RTP ticks, 160 bytes and 20 ms using the
   released SpanDSP provider. Qualify product source-rate detection and finite
   tail/timed-send assumptions first. Retain one verified composition catalog
   before any wider offer. Do not bypass DTMF or add a second resampler.
4. Verify incoming/outgoing loopback, private control, event and in-band DTMF,
   source age, retirement, loss and finite-tail behavior. Stop for authorized
   real-hardware testing before expanding codec breadth.

Later parity retains codec-specific units and modes rather than forcing 20 ms.
Dialogic ADPCM/LPC10 RTP compatibility requires an explicit wire decision.
Siren7, Siren14 and G.719 remain deferred and unadvertised.

Review focus: compressed bytes are not PCM frames; G.722's RTP clock differs;
missing provider directions never become advertisement; retries preserve source
birth and packet identity; DTMF/source-rate and finite-tail guards remain truthful.
