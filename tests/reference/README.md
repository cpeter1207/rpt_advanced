# Reference parity fixtures

The existing C implementation and its tests remain the behavior reference
until the corresponding Rust migration task establishes parity and removes the
replaced C source. Each Rust port adds literal behavioral vectors before its
implementation; this directory records any fixtures shared between the C and
Rust tests.
