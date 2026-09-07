# AllStarLink implementation status

This work is not deployment-ready. The running controller on 524950 has not
been changed during this implementation phase.

## Implemented locally

The original linking-command parser accepts configured prefixes, validates
ambiguous mappings, and distinguishes destination-taking commands from status
and reconnect operations. An empty mapping disables that command. Destination
zero remains a reference to the last operated node, to be resolved by the link
controller. Parsing failures leave the caller's result unchanged.

The deny-first access policy and inherited `link_allow_nodes` / `link_deny_nodes`
configuration are implemented locally. Tests cover all combinations of verified
identity, same-server status, denylist membership, allowlist restriction, and
allowlist membership. An explicit empty per-node list clears the shared default.
Malformed lists are rejected during configuration validation. The access policy
is connected to incoming channel admission with directory/address validation.

The parser is wired into the local worker and control queue. DTMF is normalized
to an internal 8 kHz detector while link audio remains at its negotiated rate.
The integration test exercises connect, disconnect, and timeout commands at
8, 16, and 48 kHz.

On 2026-09-07, the Debian 13 amd64 quality container passed the complete
`make ci` gate: compilation, formatting, Ruff, Cppcheck, Clang-Tidy, Doxygen,
unit tests, staged installation, source-archive rebuilding, and the two-process
IAX integration. Coverage is 100% for lines, functions, and branches. These
results do not establish classic app_rpt interoperability or the native
platform matrix. No deployment has occurred.

## Remaining

- Complete permanent-link state and reconnect recovery.
- Implement remote-command forwarding and linking status/last-keyed reporting.
- Exercise every agreed linking-only command through the live controller.
- Test ordinary app_rpt interoperability and higher-rate capable peers, including
  multiple peers, denial, failures, hangup, and reload.
- Run all four native platform gates and approved testing on 524950 using its
  existing app_rpt linking settings.

## Transport integration checkpoint

Two isolated Asterisk processes exchange bidirectional audio through real IAX
channels using both ulaw (8 kHz) and slin16 (16 kHz). The implementation obtains
available codecs from Asterisk and converts peer audio to the radio's PCM rate.
Per-node routing provides mix-minus, monitor, and local-monitor modes. Incoming
calls enter through `RptAdvanced(node)`; outgoing administrative operations use
`rpt_advanced link`.

The Debian 13 amd64 integration test also verifies deny-over-allow rejection,
explicit disconnect, automatic
remote-hangup cleanup, reconnection initiated from the other end, and configuration
reload with a connected peer. Disconnected readers are joined by a control thread,
not the hardware-paced audio worker. Module compilation and staged installation
pass for this checkpoint. The expanded Debian 13 amd64 unit/function tests reach
1,419/1,419 lines and 1,102/1,102 branches. Doxygen, formatting, Ruff, Cppcheck,
and Clang-Tidy pass; `platform-verify` also passes, including the original radio
integration, staged install, source-archive rebuild, and two-process IAX tests.
These results do not establish classic app_rpt interoperability or replace the
remaining native platform matrix. No production deployment has occurred.

The peer transport handles both redundant explicit keying (`!NEWKEY!`) and
voice-presence keying (`!NEWKEY1!`). Explicit key state expires after four seconds
without refresh; voice-presence state uses a 50 ms tail while queued audio drains.
Tests cover bounded text parsing, negotiation, missing unkey, and disconnect text.

Outbound calls are prepared under the runtime lock, dialed without it, and attached
only if the runtime revision has not changed. Tests verify same-process IAX
admission/disconnect and cancellation of an answered call after intervening reload.
The command parser is consumed by the worker's DTMF execution path; permanent
link recovery and remote/status command execution remain outstanding.

## Reference checks

The manual's [IAX text page](https://allstarlink.github.io/developers/iaxtext/)
explicitly warns that it is incomplete and potentially incorrect. Behavioral
inspection of app_rpt commit
`6966503d14cefb49a5bd269edb8524e549de0a85` found that `!NEWKEY1!` selects
voice-presence signaling, whereas `!NEWKEY!` selects redundant radio-control
signaling. They must not be treated as equivalent handshake strings. No app_rpt
implementation is copied.

Read-only checks on 524950 found an active RadioPlusAdvanced channel, an incoming
dialplan still using `Rpt()` and `RPT_NODE()`, DNS-only lookup, and an IAX radio
profile allowing ulaw, adpcm, and gsm. Test preparation must account for those
interfaces before enabling incoming links or wider-rate negotiation.
