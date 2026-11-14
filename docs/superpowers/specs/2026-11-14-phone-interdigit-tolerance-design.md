# Phone inter-digit tolerance

The owner reports difficulty entering commands slowly on a touchscreen. Phone
input currently shares hard-coded three-second collection thresholds in login,
the phone grammar, and the destination node's source-private collector.

Use the existing phone-account configuration boundary: `inter_digit_timeout_ms`
defaults to 10000 and accepts 1 through 60000 milliseconds. Invalid values warn
and use the default. The upper bound is the existing 60-second login admission
window, not a new security lifetime. Snapshot this value at private call startup
and apply it consistently to partial mailbox/PIN input, phone prefixes and
quotes, and that exact phone origin's destination command collector. An explicit
terminator and already-complete unambiguous command still finish immediately.
The existing completion/discard behavior at inactivity is unchanged.

A single account setting is preferred to separate login/command settings because
one accessibility requirement spans these collectors. Globally extending radio
collection would delay ordinary traffic and is explicitly excluded. Buffering a
second complete command in the host would duplicate the existing private parser
and is unnecessary.

Preserve the separate 60-second unauthenticated admission deadline, failed-login
attempt policy, authorization expiry, call lifetime, and three-second radio and
ordinary-peer timing. No digits, credentials, caller identities, or audio enter
logs. Reload continues to use the existing account/call generation retirement.
No provider ABI, live configuration, node installation, or public push changes.

Tests cover gaps longer than three seconds, configurable boundaries, field and
quoted terminators, expiration cleanup, interleaved ordinary input, and the
unchanged absolute admission deadline. Hardware acceptance remains a later,
explicitly authorized manual checkpoint.
