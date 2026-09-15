# ADR 0024: Voting receivers and simulcast use explicit timing boundaries

Status: Accepted

## Context

rpt_advanced must work with current ASL3 voting deployments while providing a
native path for GPS-disciplined receiver voting and best-effort simulcast. The
design must preserve one receiver and transmitter per node, lock-free audio,
and the clock-recovery responsibilities already assigned to the shared PCM
ring.

## Decision

Each configured node owns exactly one receiver and one transmitter. A local
multi-radio system uses additional configured nodes connected through loopback
or the local network rather than multiple radio ports in one node.

The voting boundary interoperates with ASL3 `chan_voter` and Simple Voter. A
native voting path uses GPS-disciplined time and makes a best effort to align
playout for simultaneous transmit; sample-by-sample simultaneity is not
required because propagation and analog radio paths impose unavoidable delay
differences. An attached CM119 uses dynamic rate correction. The appliance's
direct-codec radio ports instead share a carrier audio reference, which may be
locked to the optional GPS 10 MHz reference; their per-transmitter playout
delays remain calibrated in software against the common timebase.

## Consequences

Voting, timing distribution, receiver selection, GPS interface, and simulcast
measurement remain explicit implementation work rather than accidental effects
of ordinary link buffering. The native path must use the lock-free audio and
PCM-ring routing rules of ADRs 0002, 0003, and 0025. New voter protocols or
timing sources require an amendment to this record.
