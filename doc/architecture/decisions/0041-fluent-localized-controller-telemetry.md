# ADR 0041: Fluent catalogs own built-in RF telemetry

Status: Accepted (2026-10-02)

## Context

Built-in connect/disconnect, status, time, group, and scheduled-warning messages
must be translatable without changing controller policy. Custom event templates
already have operator-defined wording and remain a separate feature.

## Decision

Use Project Fluent FTL catalogs for controller-owned RF text/status, speech, and
Morse forms. Rust callers select a typed message and provide its defined
arguments; they do not embed built-in wording or choose arbitrary catalog IDs.
Ship a complete `en-US` catalog. A node may select a locale, inherited from the
global setting. Optional translations fall back independently per message
attribute to English; malformed or incomplete required English rejects the
candidate configuration generation and leaves the active generation unchanged.

Install packaged catalogs under
`/usr/share/asterisk/rpt_advanced/messages/<locale>.ftl`. Administrator
overrides use `/etc/asterisk/rpt_advanced/messages/<locale>.ftl` and take
precedence over packaged catalogs for the same locale.

Load, validate, and format catalogs on the serialized control plane. Audio
callbacks receive only already-prepared telemetry media; they never access the
filesystem, Fluent bundles, or translation locks. Custom event templates and
non-RF diagnostics retain their existing formatting behavior.

## Consequences

Each built-in message maintains `.text`, `.tts`, and `.morse` entries in the
catalog and is included in the installed product package. New built-in RF
wording belongs in the English catalog and is covered by its completeness and
formatting tests. Locale errors are reported as configuration warnings when an
English fallback is possible.

## Implementation status — 2026-10-04

The packaged `en-US` catalog, per-node/global locale selection, administrator
overrides, per-attribute English fallback, built-in RF message migration, and
serialized warning delivery are implemented. The native parrot's peak/RMS
report is a typed catalog message with text, speech, and Morse forms; speech
playback uses the resolved per-node voice, speed, and level. Catalog
preparation and formatting remain outside audio callbacks.
