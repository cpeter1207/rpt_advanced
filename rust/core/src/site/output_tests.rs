use super::*;
use crate::config::{ConfigDocument, NodeId, ResolvedSiteSettings};

fn settings(extra: &str) -> crate::config::OutputSettings {
    ResolvedSiteSettings::resolve(&ConfigDocument::parse(&format!("[1000]\n[output 1000 relay]\nenabled=yes\nbackend=gpiod\ndevice=chip\nline=relay\n{extra}")).unwrap(), &NodeId::new("1000").unwrap()).unwrap().value.outputs.remove(0)
}

#[test]
fn site_output_pulse_prior_state() {
    for initially_on in [false, true] {
        let policy = settings(if initially_on {
            "startup_state=on\n"
        } else {
            ""
        });
        let mut output = OutputState::new(policy.clone());
        output.advance(0, true);
        output.command(OutputRequest::Pulse, 1000).unwrap();
        assert!(output.snapshot().requested_on);
        output.reconfigure(policy);
        output.advance(1499, true);
        assert!(output.snapshot().requested_on);
        output.advance(1500, true);
        assert_eq!(output.snapshot().requested_on, initially_on);
    }
}

#[test]
fn site_output_retrigger_explicit_command_and_fault() {
    let mut output = OutputState::new(settings(""));
    output.advance(0, true);
    output.command(OutputRequest::Pulse, 1000).unwrap();
    output.command(OutputRequest::Pulse, 1200).unwrap();
    output.advance(1500, true);
    assert!(output.snapshot().requested_on);
    output.advance(1700, true);
    assert!(!output.snapshot().requested_on);
    output.command(OutputRequest::Pulse, 2000).unwrap();
    output.command(OutputRequest::On, 2100).unwrap();
    output.advance(2500, true);
    assert!(output.snapshot().requested_on);
    output.command(OutputRequest::Pulse, 3000).unwrap();
    output.command(OutputRequest::Off, 3100).unwrap();
    output.advance(3500, true);
    assert!(!output.snapshot().requested_on);
    output.command(OutputRequest::Pulse, 4000).unwrap();
    output.advance(4100, false);
    assert!(output.snapshot().fault);
    assert!(!output.snapshot().requested_on);
    assert!(output.command(OutputRequest::On, 4200).is_err());
    output.advance(4500, true);
    assert!(!output.snapshot().requested_on);
}
