//! Project physical connector selections onto the provider's one-owner pin map.
use super::{
    Interface, ParallelInput, ParallelOutput, RadioProfileError, RadioSignals, ResourceBinding,
    SignalSource,
};

pub(super) fn apply(
    interface: &mut Interface,
    s: &RadioSignals,
    owner: ResourceBinding,
) -> Result<(), RadioProfileError> {
    let conflict = || RadioProfileError {
        resource: owner,
        conflicting: None,
        field: "pin",
    };
    let rx = &s.rx;
    let selections = [
        (
            rx.squelch.enabled,
            &rx.squelch.binding,
            ParallelInput::Carrier,
        ),
        (rx.ctcss.enabled, &rx.ctcss.binding, ParallelInput::Ctcss),
    ];
    // A selected role owns exactly its resolved pins. Resolve both roles before reassigning
    // so a connector can move from carrier to tone (or vice versa) in one candidate.
    for (enabled, b, role) in selections {
        if enabled && b.source == SignalSource::Parallel {
            for (pin, slot) in [10, 12, 13, 15]
                .into_iter()
                .zip(&mut interface.parallel.inputs)
            {
                if *slot == role && !b.pins.contains(&pin) {
                    *slot = ParallelInput::Unassigned;
                }
            }
        }
    }
    for (enabled, b, assignment) in selections {
        if enabled && b.source == SignalSource::Parallel {
            for n in &b.pins {
                let index = [10, 12, 13, 15]
                    .iter()
                    .position(|pin| pin == n)
                    .ok_or_else(conflict)?;
                let slot = &mut interface.parallel.inputs[index];
                if !matches!(slot, ParallelInput::Unassigned | ParallelInput::Input)
                    && *slot != assignment
                {
                    return Err(conflict());
                }
                *slot = assignment;
            }
        }
    }
    if s.tx.ptt.source == SignalSource::Parallel {
        for (pin, slot) in (2..=9).zip(&mut interface.parallel.outputs) {
            if *slot == ParallelOutput::PushToTalk && !s.tx.ptt.pins.contains(&pin) {
                *slot = ParallelOutput::Unassigned;
            }
        }
        for &pin in &s.tx.ptt.pins {
            let slot = &mut interface.parallel.outputs[usize::from(pin - 2)];
            if !matches!(
                slot,
                ParallelOutput::Unassigned | ParallelOutput::PushToTalk
            ) {
                return Err(conflict());
            }
            *slot = ParallelOutput::PushToTalk;
        }
    }
    // The provider has one output interlock per active remote encoder.
    let tx = &s.tx;
    let external = [
        (tx.ctcss.enabled, tx.ctcss.source, &tx.ctcss.output),
        (tx.dcs.enabled, tx.dcs.source, &tx.dcs.output),
    ];
    for (enabled, source, output) in external {
        if !enabled || source != SignalSource::External {
            continue;
        }
        let Some(binding) = output else { continue };
        if binding.source == SignalSource::Serial {
            if binding.pins.is_empty()
                || s.tx.ptt.source == SignalSource::Serial
                    && binding.pins.iter().any(|pin| s.tx.ptt.pins.contains(pin))
            {
                return Err(conflict());
            }
            continue;
        }
        let &[pin] = binding.pins.as_slice() else {
            return Err(conflict());
        };
        match binding.source {
            SignalSource::Parallel => {
                let slot = &mut interface.parallel.outputs[usize::from(pin - 2)];
                let idle = if binding.inverted {
                    ParallelOutput::High
                } else {
                    ParallelOutput::Low
                };
                if *slot != ParallelOutput::Unassigned && *slot != idle {
                    return Err(conflict());
                }
                *slot = idle;
            }
            SignalSource::SoundChipGpio => {
                if interface.gpio.clip_led_pin == Some(pin) {
                    return Err(conflict());
                }
                let slot = &mut interface.gpio.pins[usize::from(pin - 1)];
                let idle = if binding.inverted {
                    super::GpioMode::High
                } else {
                    super::GpioMode::Low
                };
                if *slot != super::GpioMode::Input && *slot != idle {
                    return Err(conflict());
                }
                *slot = idle;
            }
            _ => return Err(conflict()),
        }
    }
    Ok(())
}

/// Active owners open the whole parallel endpoint or rig even when their individual pin sets differ.
pub(super) fn reserve(
    claims: &mut std::collections::BTreeMap<(&'static str, String), ResourceBinding>,
    interface: &Interface,
    signals: &RadioSignals,
    remote: &super::RemotePolicy,
    owner: ResourceBinding,
) -> Result<(), RadioProfileError> {
    let mut claim = |field, selector: &str| {
        if !selector.is_empty() {
            let device_class = if field == "hamlib_device" {
                "serial_device"
            } else {
                field
            };
            if let Some(previous) = claims.insert((device_class, selector.to_owned()), owner) {
                return Err(RadioProfileError {
                    resource: owner,
                    conflicting: Some(previous),
                    field,
                });
            }
        }
        Ok(())
    };
    if interface.audio.selection == super::DeviceSelection::Exact {
        claim("device_identifier", &interface.audio.device_identifier)?;
        claim("usb_serial", &interface.audio.usb_serial)?;
    }
    let port = &interface.parallel;
    let parallel_rf = remote.enabled
        && matches!(
            remote.backend,
            Some(super::RemoteBackend::ParallelChannel | super::RemoteBackend::ParallelRtx)
        );
    let active_output = port.outputs.iter().any(|output| match output {
        ParallelOutput::Unassigned => false,
        ParallelOutput::PushToTalk => signals.tx.ptt.source == SignalSource::Parallel,
        ParallelOutput::High | ParallelOutput::Low => true,
    });
    if parallel_rf
        || active_output
        || port
            .inputs
            .iter()
            .any(|pin| *pin != ParallelInput::Unassigned)
    {
        claim("parallel_device", &port.device)?;
        // AUTO tries ppdev first and then the configured x86 I/O base. An empty pathname is raw-only.
        if cfg!(all(
            target_os = "linux",
            any(target_arch = "x86", target_arch = "x86_64")
        )) && (1..u32::from(u16::MAX)).contains(&port.base_address)
        {
            claim("parallel_base_address", &port.base_address.to_string())?;
        }
    }
    let encoder_serial = [
        (
            signals.tx.ctcss.enabled,
            signals.tx.ctcss.source,
            &signals.tx.ctcss.output,
        ),
        (
            signals.tx.dcs.enabled,
            signals.tx.dcs.source,
            &signals.tx.dcs.output,
        ),
    ]
    .iter()
    .any(|(enabled, source, output)| {
        *enabled
            && *source == SignalSource::External
            && output
                .as_ref()
                .is_some_and(|b| b.source == SignalSource::Serial)
    });
    let serial = encoder_serial
        || signals.tx.ptt.source == SignalSource::Serial
        || signals.rx.squelch.enabled && signals.rx.squelch.binding.source == SignalSource::Serial
        || signals.rx.ctcss.enabled && signals.rx.ctcss.binding.source == SignalSource::Serial;
    // Rig, DCD and PTT pathnames can designate the same port within a single
    // CAT owner. A separately selected modem or another active radio cannot.
    if remote.enabled && remote.backend == Some(super::RemoteBackend::Hamlib)
        || signals.tx.ptt.source == SignalSource::Hamlib
    {
        let paths: std::collections::BTreeSet<_> = remote
            .hamlib_options
            .iter()
            .filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "rig_pathname" | "ptt_pathname" | "dcd_pathname"
                )
            })
            .map(|(_, value)| value.as_str())
            .collect();
        for path in paths {
            claim("hamlib_device", path)?;
        }
    }
    if serial && let Some(path) = &interface.serial.device {
        claim("serial_device", path)?;
    }
    Ok(())
}
