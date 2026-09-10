# ADR 0004: Hamlib remote-base boundary

Status: Accepted

## Context

Remote-base operation needs common control of many amateur radios while their
audio interfaces vary among external CM119 devices, onboard USB sound devices,
and capable streaming-radio back ends.

## Decision

Use Hamlib for rig control. Configure one rig per node and select its model by
a canonical enumerated model name rather than a numeric Hamlib model ID. Select
the audio source explicitly as CM119, ALSA, or a supported Hamlib stream. COS,
CTCSS, and DCS remain USBRadioPlus DSP or hardware-signaling responsibilities
unless a practical Hamlib replacement exists.

## Consequences

Rig control and audio selection are separate configuration concerns. A radio
model that Hamlib controls but that lacks a usable configured audio source
cannot become an operational remote base. The accepted model-name catalog must
be validated against the installed Hamlib version.
