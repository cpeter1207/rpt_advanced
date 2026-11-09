use super::*;
use crate::runtime::{GenerationSettings, NodeHost, RuntimeGeneration};
fn generation(id: u64) -> RuntimeGeneration<(), ()> {
    RuntimeGeneration::prepare(
        id,
        GenerationSettings {
            node: "524950".into(),
            device: "radio0".into(),
            receive_maximum: 960,
            transmit_maximum: 960,
            squelch_delay_ms: 0,
            status_snapshot_interval_ms: 50,
        },
        (),
        (),
    )
    .unwrap()
}
fn operation(action: LinkAction, remote: &str) -> DigitOperation {
    DigitOperation::new(
        crate::command::Command {
            action,
            node: remote.into(),
        },
        None,
    )
}
fn links() -> NodeLinkControl {
    NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        None,
    )
    .unwrap()
}

#[test]
fn phone_pending_connect_and_initial_retry_end_with_the_actual_call() {
    use crate::link::{PhoneAccountId, PhoneSource};
    for permanent in [false, true] {
        let mut links = links();
        let source = PhoneSource::new(
            PhoneAccountId::new("provider").unwrap(),
            3,
            "aabbccddeeff00112233445566778899".parse().unwrap(),
            4,
            5,
        )
        .unwrap();
        let signals = crate::link::PeerSignals::for_phone(source.clone());
        assert!(signals.authenticate_phone());
        let reply = signals.reply_target();
        let action = if permanent {
            LinkAction::PermanentTransceive
        } else {
            LinkAction::Transceive
        };
        let map = DtmfCommandMap::standard();
        let prefix = &map
            .mappings()
            .iter()
            .find(|item| item.action == action)
            .unwrap()
            .digits;
        let mut operation = None;
        for digit in format!("*{prefix}2000#").chars() {
            if let Some(DigitOutcome::Operation(request)) =
                links.phone_digit(&source, digit, 1, reply.clone())
            {
                operation = Some(request);
            }
        }
        let mut host = NodeHost::new(generation(1));
        let LinkEffect::Connect(attempt) = links
            .command(operation.unwrap(), host.control().work().unwrap(), 2, true)
            .unwrap()
        else {
            panic!("expected a pending private call command")
        };
        assert!(attempt.current_at(2));
        if permanent {
            assert_eq!(
                links.finish_connect_at(attempt, false, 3, None, |_| None),
                Ok(false)
            );
            let retry = links
                .take_retry(10_000, host.control().work().unwrap())
                .unwrap();
            assert!(retry.current_at(10_000));
            links.retire_source(&reply);
            assert!(!retry.current_at(10_001));
            assert_eq!(
                links.finish_retry(retry, true, 10_001),
                Err(AdmissionError::Stale)
            );
        } else {
            links.retire_source(&reply);
            assert!(!attempt.current_at(3));
            assert_eq!(
                links.finish_connect_at(attempt, true, 3, None, |_| None),
                Err(AdmissionError::Stale)
            );
        }
        assert!(links.manager().snapshot().is_empty());
    }
}

#[test]
fn phone_grammar_macro_keeps_private_source_through_dispatch_and_retirement() {
    use crate::{
        link::{PhoneAccountId, PhoneSource},
        phone_dtmf::{PhoneDtmfAction, PhoneDtmfGrammar, PhoneDtmfOptions},
    };
    let mut links = links();
    let definition = crate::config::ResolvedDtmfMacroSettings {
        name: "private-status".into(),
        trigger: "*95".into(),
        protected: false,
        commands: "*70#,*73#".into(),
    };
    let limits = crate::dtmf_macro::DtmfMacroLimits::default();
    let compiled = crate::dtmf_macro::compile(
        &definition,
        limits,
        &DtmfCommandMap::standard(),
        "1000",
        &["*95".into()],
    )
    .unwrap();
    links
        .commands
        .configure_macros(vec![compiled], limits)
        .unwrap();
    let source = PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        3,
        "aabbccddeeff00112233445566778899".parse().unwrap(),
        4,
        5,
    )
    .unwrap();
    let signals = crate::link::PeerSignals::for_phone(source.clone());
    let reply = signals.reply_target();
    for digit in "*95".chars() {
        assert!(
            links
                .phone_digit(&source, digit, 1, reply.clone())
                .is_none()
        );
    }
    assert!(links.advance_macros(1).is_empty());
    assert!(links.peer_commands.is_empty());
    assert!(signals.authenticate_phone());
    let mut grammar =
        PhoneDtmfGrammar::new(links.commands.command_map(), PhoneDtmfOptions::default()).unwrap();
    for digit in "*41000#*95".chars() {
        for action in grammar.digit(digit, 10) {
            match action {
                PhoneDtmfAction::SelectTarget(target) => assert_eq!(target, "1000"),
                PhoneDtmfAction::NodeDigit { target, digit } => {
                    assert_eq!(target, "1000");
                    assert!(
                        links
                            .phone_digit(&source, digit, 10, reply.clone())
                            .is_none()
                    );
                }
                other => panic!("unexpected private-control action: {other:?}"),
            }
        }
    }
    let outcomes = links.advance_macros(10);
    let [DigitOutcome::Operation(operation)] = outcomes.as_slice() else {
        panic!("expected one source-owned macro command: {outcomes:?}")
    };
    assert_eq!(operation.reply, Some(reply.clone()));
    assert_eq!(operation.origin, Some(source.call_id().to_string()));
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(operation.clone(), host.control().work().unwrap(), 10, false),
        Ok(LinkEffect::Telemetry(LinkAction::Status, Some(destination))) if destination == reply
    ));
    assert!(links.manager().snapshot().is_empty());
    links.retire_source(&reply);
    assert!(links.advance_macros(2010).is_empty());
    assert!(matches!(
        links.command(
            operation.clone(),
            host.control().work().unwrap(),
            2010,
            false
        ),
        Err(AdmissionError::Stale)
    ));
    let replacement = PhoneSource::new(
        source.account().clone(),
        source.account_generation(),
        source.call_id(),
        5,
        6,
    )
    .unwrap();
    let replacement_signals = crate::link::PeerSignals::for_phone(replacement.clone());
    assert!(replacement_signals.authenticate_phone());
    assert!(
        links
            .phone_digit(&replacement, '*', 2011, reply.clone())
            .is_none()
    );
    assert!(links.peer_commands.is_empty());
    assert!(links.manager().snapshot().is_empty());
}

#[test]
fn phone_commands_keep_exact_private_source_and_retire_without_link_admission() {
    use crate::link::{PhoneAccountId, PhoneSource};
    let account = PhoneAccountId::new("provider").unwrap();
    let call = "aabbccddeeff00112233445566778899".parse().unwrap();
    let source = PhoneSource::new(account.clone(), 3, call, 4, 5).unwrap();
    let replacement = PhoneSource::new(account, 3, call, 5, 6).unwrap();
    let signals = crate::link::PeerSignals::for_phone(source.clone());
    let reply = signals.reply_target();
    let mut links = links();
    assert!(links.phone_digit(&source, '*', 1, reply.clone()).is_none());
    let mut unverified = operation(LinkAction::Status, "");
    unverified.reply = Some(reply.clone());
    unverified.origin = Some(call.to_string());
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(unverified, host.control().work().unwrap(), 1, false),
        Err(AdmissionError::Denied)
    ));
    assert!(signals.authenticate_phone());
    assert!(
        links
            .phone_digit(&replacement, '*', 1, reply.clone())
            .is_none()
    );
    assert!(
        links.phone_digit(&source, '5', 1, reply.clone()).is_none(),
        "command-only calls cannot regenerate RF digits"
    );
    let mut outcome = None;
    for digit in "*70#".chars() {
        outcome = links
            .phone_digit(&source, digit, 2, reply.clone())
            .or(outcome);
    }
    let Some(DigitOutcome::Operation(operation)) = outcome else {
        panic!("private status operation")
    };
    assert_eq!(operation.reply, Some(reply.clone()));
    assert_eq!(operation.origin.as_deref(), Some(call.to_string().as_str()));
    assert!(links.manager().snapshot().is_empty());
    assert!(links.phone_digit(&source, '*', 2, reply.clone()).is_none());
    assert!(!links.peer_commands.is_empty());
    signals.end();
    links.retire_source(&reply);
    assert!(links.peer_commands.is_empty());
    assert!(links.phone_digit(&source, '*', 3, reply).is_none());
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(operation, host.control().work().unwrap(), 3, false),
        Err(AdmissionError::Stale)
    ));
    let fresh = crate::link::PeerSignals::for_phone(replacement.clone());
    assert!(fresh.authenticate_phone());
    links.close();
    assert!(
        links
            .phone_digit(&replacement, '*', 4, fresh.reply_target())
            .is_none()
    );
    assert!(links.manager().snapshot().is_empty());
}

#[test]
fn mailbox_login_never_unlocks_administration_or_enables_legacy_private_forwarding() {
    use crate::link::{PhoneAccountId, PhoneSource};
    let source = PhoneSource::new(
        PhoneAccountId::new("provider").unwrap(),
        1,
        "aabbccddeeff00112233445566778899".parse().unwrap(),
        2,
        3,
    )
    .unwrap();
    let signals = crate::link::PeerSignals::for_phone(source.clone());
    assert!(signals.authenticate_phone());
    let reply = signals.reply_target();
    let mut links = links();
    for digit in "*804".chars() {
        assert!(
            links
                .phone_digit(&source, digit, 1, reply.clone())
                .is_none(),
            "mailbox authentication is not an admin grant"
        );
    }
    let mut request = operation(LinkAction::Command, "2000");
    request.reply = Some(reply.clone());
    request.origin = Some(source.call_id().to_string());
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(request.clone(), host.control().work().unwrap(), 2, true),
        Err(AdmissionError::Denied)
    ));
    request.command.action = LinkAction::Status;
    request.origin = None;
    assert!(
        matches!(
            links.command(request, host.control().work().unwrap(), 2, false),
            Err(AdmissionError::Denied)
        ),
        "a private call cannot fall back to trusted local origin"
    );
    assert!(links.phone_digit(&source, '!', 3, reply).is_none());
}

#[test]
fn dtmf_macro_dispatch_retains_exact_source_and_rejects_canceled_queued_operations() {
    let mut links = links();
    let definition = crate::config::ResolvedDtmfMacroSettings {
        name: "status".into(),
        trigger: "*95".into(),
        protected: false,
        commands: "*70#,*73#".into(),
    };
    let limits = crate::dtmf_macro::DtmfMacroLimits::default();
    let compiled = crate::dtmf_macro::compile(
        &definition,
        limits,
        &DtmfCommandMap::standard(),
        "524950",
        &["*95".into()],
    )
    .unwrap();
    links.configure_macros(vec![compiled], limits).unwrap();
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    let first = crate::link::PeerSignals::new();
    let second = crate::link::PeerSignals::new();
    for (remote, signals) in [("2000", &first), ("3000", &second)] {
        for digit in "*95".chars() {
            assert!(
                links
                    .peer_digit(remote, digit, 1, signals.reply_target())
                    .is_none()
            );
        }
    }
    let operations = links.advance_macros(1);
    assert_eq!(operations.len(), 2);
    let [DigitOutcome::Operation(a), DigitOutcome::Operation(b)] = operations.as_slice() else {
        panic!("source-tagged operations")
    };
    assert_eq!(a.origin.as_deref(), Some("2000"));
    assert_eq!(a.reply, Some(first.reply_target()));
    assert_eq!(b.reply, Some(second.reply_target()));
    let mut host = NodeHost::new(generation(1));
    let mut foreign = a.clone();
    foreign.origin = Some("3000".into());
    foreign.reply = Some(second.reply_target());
    assert!(matches!(
        links.command(foreign, host.control().work().unwrap(), 2, false),
        Err(AdmissionError::Stale)
    ));
    links.peer_digit("2000", '*', 2, first.reply_target());
    assert!(matches!(
        links.command(a.clone(), host.control().work().unwrap(), 2, false),
        Err(AdmissionError::Stale)
    ));
    assert!(matches!(
        links.command(b.clone(), host.control().work().unwrap(), 2, false),
        Ok(LinkEffect::Telemetry(LinkAction::Status, Some(_)))
    ));
    second.end();
    assert!(links.advance_macros(2001).is_empty());
    assert!(matches!(
        links.command(b.clone(), host.control().work().unwrap(), 2001, false),
        Err(AdmissionError::Stale)
    ));
}

#[test]
fn dtmf_macro_submission_loss_revokes_all_sources_without_clearing_human_selection() {
    let mut links = links();
    let definition = crate::config::ResolvedDtmfMacroSettings {
        name: "status".into(),
        trigger: "*95".into(),
        protected: false,
        commands: "*70#,*73#".into(),
    };
    let limits = crate::dtmf_macro::DtmfMacroLimits::default();
    let compiled = crate::dtmf_macro::compile(
        &definition,
        limits,
        &DtmfCommandMap::standard(),
        "524950",
        &[],
    )
    .unwrap();
    links.configure_macros(vec![compiled], limits).unwrap();
    links.accept("2000", true).unwrap();
    let signals = crate::link::PeerSignals::new();
    for digit in "*95".chars() {
        links.peer_digit("2000", digit, 1, signals.reply_target());
    }
    let queued = links.advance_macros(1);
    links.lost_digits();
    assert!(links.advance_macros(2001).is_empty());
    let DigitOutcome::Operation(operation) = &queued[0] else {
        panic!("queued")
    };
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(operation.clone(), host.control().work().unwrap(), 2, false),
        Err(AdmissionError::Stale)
    ));
}

#[test]
fn dtmf_macro_pending_connect_cannot_outlive_its_source_or_generation() {
    for cancellation in ["input", "source_end", "reload"] {
        let mut links = links();
        let limits = crate::dtmf_macro::DtmfMacroLimits::default();
        let definition = crate::config::ResolvedDtmfMacroSettings {
            name: "connect".into(),
            trigger: "*95".into(),
            protected: false,
            commands: "*33000#".into(),
        };
        let compiled = crate::dtmf_macro::compile(
            &definition,
            limits,
            &DtmfCommandMap::standard(),
            "524950",
            &[],
        )
        .unwrap();
        links.configure_macros(vec![compiled], limits).unwrap();
        links.accept("2000", true).unwrap();
        let source = crate::link::PeerSignals::new();
        for digit in "*95".chars() {
            links.peer_digit("2000", digit, 1, source.reply_target());
        }
        let DigitOutcome::Operation(operation) = links.advance_macros(1).remove(0) else {
            panic!("connect expansion")
        };
        let mut host = NodeHost::new(generation(1));
        let LinkEffect::Connect(attempt) = links
            .command(operation, host.control().work().unwrap(), 1, false)
            .unwrap()
        else {
            panic!("pending dial")
        };
        assert!(attempt.current());
        match cancellation {
            "input" => {
                links.peer_digit("2000", '*', 2, source.reply_target());
            }
            "source_end" => source.end(),
            "reload" => links.reconfigure(
                AccessPolicy::new("", "").unwrap(),
                DtmfCommandMap::standard(),
                None,
            ),
            _ => unreachable!(),
        }
        assert!(
            !attempt.current(),
            "pending external dial must stop: {cancellation}"
        );
        assert!(matches!(
            links.finish_connect_at(attempt, true, 3, None, |_| None),
            Err(AdmissionError::Stale)
        ));
        assert!(
            !links
                .manager()
                .snapshot()
                .iter()
                .any(|peer| peer.name == "3000")
        );
    }
}

#[test]
fn dtmf_macro_cancel_after_connection_publication_keeps_the_completed_link() {
    let mut links = links();
    let limits = crate::dtmf_macro::DtmfMacroLimits::default();
    let definition = crate::config::ResolvedDtmfMacroSettings {
        name: "connect".into(),
        trigger: "*95".into(),
        protected: false,
        commands: "*8133000#".into(),
    };
    let compiled = crate::dtmf_macro::compile(
        &definition,
        limits,
        &DtmfCommandMap::standard(),
        "524950",
        &[],
    )
    .unwrap();
    links.configure_macros(vec![compiled], limits).unwrap();
    for digit in "*95".chars() {
        links.digit(DigitEvent::Digit { digit, now_ms: 1 });
    }
    let DigitOutcome::Operation(operation) = links.advance_macros(1).remove(0) else {
        panic!("connect expansion")
    };
    let mut host = NodeHost::new(generation(1));
    let LinkEffect::Connect(attempt) = links
        .command(operation, host.control().work().unwrap(), 1, false)
        .unwrap()
    else {
        panic!("pending dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, true, 2, None, |_| None),
        Ok(true)
    );
    links.digit(DigitEvent::Digit {
        digit: '*',
        now_ms: 3,
    });
    assert!(
        links
            .manager()
            .snapshot()
            .iter()
            .any(|peer| peer.name == "3000")
    );
    links.ended("3000");
    links.reclaimed("3000", 4);
    let retry = links
        .take_retry(10000, host.control().work().unwrap())
        .expect("completed permanent connection has normal recovery intent");
    assert!(retry.current());
    assert_eq!(links.finish_retry(retry, true, 10001), Ok(true));
}

#[test]
fn dtmf_macro_failed_permanent_connect_keeps_source_ownership_through_initial_retries() {
    for before_retry in [true, false] {
        let mut links = links();
        let limits = crate::dtmf_macro::DtmfMacroLimits::default();
        let definition = crate::config::ResolvedDtmfMacroSettings {
            name: "connect".into(),
            trigger: "*95".into(),
            protected: false,
            commands: "*8133000#".into(),
        };
        let compiled = crate::dtmf_macro::compile(
            &definition,
            limits,
            &DtmfCommandMap::standard(),
            "524950",
            &[],
        )
        .unwrap();
        links.configure_macros(vec![compiled], limits).unwrap();
        for digit in "*95".chars() {
            links.digit(DigitEvent::Digit { digit, now_ms: 1 });
        }
        let DigitOutcome::Operation(operation) = links.advance_macros(1).remove(0) else {
            panic!("permanent expansion")
        };
        let mut host = NodeHost::new(generation(1));
        let LinkEffect::Connect(attempt) = links
            .command(operation, host.control().work().unwrap(), 1, false)
            .unwrap()
        else {
            panic!("pending dial")
        };
        assert_eq!(
            links.finish_connect_at(attempt, false, 2, None, |_| None),
            Ok(false)
        );
        if before_retry {
            links.digit(DigitEvent::Digit {
                digit: '*',
                now_ms: 3,
            });
            assert!(
                links
                    .take_retry(10000, host.control().work().unwrap())
                    .is_none()
            );
        } else {
            let retry = links
                .take_retry(10000, host.control().work().unwrap())
                .expect("initial retry still belongs to the source");
            assert!(retry.current());
            links.digit(DigitEvent::Digit {
                digit: '*',
                now_ms: 10001,
            });
            assert!(!retry.current());
            assert!(matches!(
                links.finish_retry(retry, true, 10002),
                Err(AdmissionError::Stale)
            ));
        }
        assert!(
            !links
                .manager()
                .snapshot()
                .iter()
                .any(|peer| peer.name == "3000" && !peer.retrying && !peer.ended)
        );
    }
}

fn initial_retry(
    links: &mut NodeLinkControl,
    host: &mut NodeHost<(), ()>,
    pending_source: bool,
    now_ms: u64,
) -> RetryWork {
    let command = if pending_source {
        for digit in "*95".chars() {
            links.digit(DigitEvent::Digit { digit, now_ms });
        }
        let DigitOutcome::Operation(command) = links.advance_macros(now_ms).remove(0) else {
            panic!("macro connection")
        };
        command
    } else {
        operation(LinkAction::PermanentTransceive, "3000")
    };
    let LinkEffect::Connect(attempt) = links
        .command(command, host.control().work().unwrap(), now_ms, false)
        .unwrap()
    else {
        panic!("initial connection")
    };
    assert_eq!(
        links.finish_connect_at(attempt, false, now_ms + 1, None, |_| None),
        Ok(false)
    );
    links
        .take_retry(now_ms + 2, host.control().work().unwrap())
        .expect("owned in-flight retry")
}

fn stale_retry_preserves_replacement(old_macro: bool) {
    for new_macro in [true, false] {
        for reload in [false, true] {
            let mut links = links();
            let mut host = NodeHost::new(generation(1));
            let limits = crate::dtmf_macro::DtmfMacroLimits::default();
            let definition = crate::config::ResolvedDtmfMacroSettings {
                name: "connect".into(),
                trigger: "*95".into(),
                protected: false,
                commands: "*8133000#".into(),
            };
            let compiled = crate::dtmf_macro::compile(
                &definition,
                limits,
                &DtmfCommandMap::standard(),
                "524950",
                &[],
            )
            .unwrap();
            links
                .configure_macros(vec![compiled.clone()], limits)
                .unwrap();
            let old = initial_retry(&mut links, &mut host, old_macro, 1);
            if reload {
                links.reconfigure(
                    AccessPolicy::new("", "").unwrap(),
                    DtmfCommandMap::standard(),
                    None,
                );
                links.configure_macros(vec![compiled], limits).unwrap();
            } else {
                links.digit(DigitEvent::Digit {
                    digit: '*',
                    now_ms: 10,
                });
            }
            if reload || !old_macro {
                host.control().publish(generation(2), 11).unwrap();
            }
            links
                .command(
                    operation(LinkAction::DisconnectPermanent, "3000"),
                    host.control().work().unwrap(),
                    12,
                    false,
                )
                .unwrap();
            let new = initial_retry(&mut links, &mut host, new_macro, 20);
            assert_eq!(
                links.finish_retry(old, false, 30),
                Err(AdmissionError::Stale)
            );
            assert!(
                links
                    .manager()
                    .snapshot()
                    .iter()
                    .any(|peer| peer.name == "3000" && peer.retrying),
                "old completion removed replacement: macro={new_macro}, reload={reload}"
            );
            assert_eq!(links.finish_retry(new, false, 31), Ok(false));
            links.digit(DigitEvent::Digit {
                digit: '*',
                now_ms: 32,
            });
            let next = links.take_retry(2000, host.control().work().unwrap());
            if new_macro {
                assert!(
                    next.is_none(),
                    "old completion stripped replacement source fencing: reload={reload}"
                );
            } else {
                let current = next.expect("ordinary replacement retains recovery intent");
                assert!(current.current());
                assert_eq!(links.finish_retry(current, true, 2001), Ok(true));
            }
        }
    }
}

#[test]
fn dtmf_macro_old_macro_retry_completion_preserves_the_replacement_owner() {
    stale_retry_preserves_replacement(true);
}

#[test]
fn dtmf_macro_old_ordinary_retry_completion_cannot_strip_a_new_macro_owner() {
    stale_retry_preserves_replacement(false);
}

fn install_macro_cases(links: &mut NodeLinkControl, definitions: &[(&str, &str, bool)]) {
    let definitions = definitions
        .iter()
        .map(
            |(trigger, commands, protected)| crate::config::ResolvedDtmfMacroSettings {
                name: trigger[1..].into(),
                trigger: (*trigger).into(),
                protected: *protected,
                commands: (*commands).into(),
            },
        )
        .collect::<Vec<_>>();
    let triggers = definitions
        .iter()
        .map(|definition| definition.trigger.clone())
        .collect::<Vec<_>>();
    let limits = crate::dtmf_macro::DtmfMacroLimits::default();
    let compiled = definitions
        .iter()
        .map(|definition| {
            crate::dtmf_macro::compile(
                definition,
                limits,
                links.commands.command_map(),
                "524950",
                &triggers,
            )
            .unwrap()
        })
        .collect();
    links.configure_macros(compiled, limits).unwrap();
}

#[test]
fn dtmf_macro_parallel_retry_cleanup_preserves_other_sources_and_private_denials() {
    let mut links = links();
    install_macro_cases(
        &mut links,
        &[
            ("*95", "*8133000#", false),
            ("*96", "*8134000#", false),
            ("*97", "*70#", true),
        ],
    );
    let mut host = NodeHost::new(generation(1));
    let first = crate::link::PeerSignals::new();
    let second = crate::link::PeerSignals::new();
    for (remote, trigger, source) in [("2000", "*95", &first), ("2500", "*96", &second)] {
        links.accept(remote, true).unwrap();
        for digit in trigger.chars() {
            links.peer_digit(remote, digit, 1, source.reply_target());
        }
    }
    for outcome in links.advance_macros(1) {
        let DigitOutcome::Operation(operation) = outcome else {
            panic!("parallel macro")
        };
        let LinkEffect::Connect(attempt) = links
            .command(operation, host.control().work().unwrap(), 1, false)
            .unwrap()
        else {
            panic!("permanent dial")
        };
        assert_eq!(
            links.finish_connect_at(attempt, false, 2, None, |_| None),
            Ok(false)
        );
    }
    let first_retry = links.take_retry(3, host.control().work().unwrap()).unwrap();
    let second_retry = links.take_retry(3, host.control().work().unwrap()).unwrap();
    assert_eq!(first_retry.remote(), "3000");
    assert_eq!(second_retry.remote(), "4000");
    assert_eq!(links.finish_retry(second_retry, true, 4), Ok(true));
    assert!(
        links
            .manager()
            .snapshot()
            .iter()
            .any(|peer| peer.name == "3000" && peer.retrying)
    );
    links
        .command(
            operation(LinkAction::DisconnectPermanent, "3000"),
            host.control().work().unwrap(),
            5,
            false,
        )
        .unwrap();
    links
        .command(
            operation(LinkAction::Status, ""),
            host.control().work().unwrap(),
            6,
            false,
        )
        .unwrap();
    let mut denied = None;
    for digit in "*97".chars() {
        denied = links
            .peer_digit("2000", digit, 7, first.reply_target())
            .or(denied);
    }
    assert!(
        matches!(denied, Some(DigitOutcome::MacroStopped { reply: Some(reply) }) if reply == first.reply_target())
    );
    assert!(
        links
            .take_retry(2000, host.control().work().unwrap())
            .is_none()
    );
    assert!(
        links
            .manager()
            .snapshot()
            .iter()
            .any(|peer| peer.name == "4000" && !peer.retrying)
    );
}

#[test]
fn dtmf_macro_queued_peer_reply_survives_collector_idle_reclamation_after_stream_completion() {
    let mut links = links();
    install_macro_cases(&mut links, &[("*95", "*70#", false)]);
    links.accept("2000", true).unwrap();
    let source = crate::link::PeerSignals::new();
    for digit in "*95".chars() {
        links.peer_digit("2000", digit, 1, source.reply_target());
    }
    let DigitOutcome::Operation(operation) = links.advance_macros(1).remove(0) else {
        panic!("queued reply")
    };
    assert!(links.advance_macros(2).is_empty());
    assert!(links.advance_macros(3).is_empty());
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(operation, host.control().work().unwrap(), 4, false),
        Ok(LinkEffect::Telemetry(LinkAction::Status, Some(_)))
    ));
}

#[test]
fn dtmf_macro_protected_pending_connections_recheck_expired_grants() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    for answer in [true, false] {
        let mut links = links();
        links.configure_admin(&digest, &digest, 100);
        install_macro_cases(&mut links, &[("*95", "*8133000#", true)]);
        let mut host = NodeHost::new(generation(1));
        for digit in "*8001234#*95".chars() {
            links.digit(DigitEvent::Digit { digit, now_ms: 1 });
        }
        let DigitOutcome::Operation(operation) = links.advance_macros(1).remove(0) else {
            panic!("unlocked macro")
        };
        let LinkEffect::Connect(attempt) = links
            .command(operation, host.control().work().unwrap(), 1, false)
            .unwrap()
        else {
            panic!("pending dial")
        };
        if answer {
            assert_eq!(
                links.finish_connect_at(attempt, true, 101, None, |_| None),
                Err(AdmissionError::Stale)
            );
        } else {
            assert_eq!(
                links.finish_connect_at(attempt, false, 2, None, |_| None),
                Ok(false)
            );
            assert!(
                links
                    .take_retry(101, host.control().work().unwrap())
                    .is_none()
            );
        }
    }
}

#[test]
fn dtmf_macro_remote_base_work_is_rejected_after_source_cancellation() {
    let mut links = links();
    links
        .configure_remote("6", crate::remote_base_command::RemoteDtmfAccess::Open)
        .unwrap();
    install_macro_cases(&mut links, &[("*95", "*651#", false)]);
    for digit in "*95".chars() {
        links.digit(DigitEvent::Digit { digit, now_ms: 1 });
    }
    let DigitOutcome::Operation(operation) = links.advance_macros(1).remove(0) else {
        panic!("typed rig query")
    };
    assert!(links.remote_authorized(&operation, 1));
    links.digit(DigitEvent::Digit {
        digit: '*',
        now_ms: 2,
    });
    assert!(!links.remote_authorized(&operation, 2));
}

#[test]
fn remote_dtmf_dispatch_retains_origin_and_rechecks_the_grant() {
    use crate::remote_base_command::{RemoteCommand, RemoteDtmfAccess};
    let mut host = NodeHost::new(generation(1));
    let mut links = links();
    links.configure_remote("6", RemoteDtmfAccess::Open).unwrap();
    links.accept("2000", true).unwrap();
    let first = crate::link::PeerSignals::new().reply_target();
    let other = crate::link::PeerSignals::new().reply_target();
    let mut result = None;
    for digit in "*651".chars() {
        assert!(links.peer_digit("2000", digit, 1, first.clone()).is_none());
    }
    assert!(links.peer_remote_unkey(2, other).is_empty());
    let mut completed = links.peer_remote_unkey(2, first.clone());
    assert_eq!(completed.len(), 1);
    if let DigitOutcome::Operation(operation) = completed.remove(0) {
        result = Some(operation);
    }
    let operation = result.unwrap();
    assert_eq!(operation.reply, Some(first.clone()));
    assert_eq!(operation.origin.as_deref(), Some("2000"));
    let effect = links
        .command(operation, host.control().work().unwrap(), 2, false)
        .unwrap();
    let LinkEffect::RemoteBase(operation) = effect else {
        panic!("remote effect")
    };
    assert_eq!(operation.remote, Some(RemoteCommand::Status { long: true }));
    assert_eq!(operation.reply, Some(first));
    let mut stale = DigitOperation::new(
        crate::command::Command {
            action: LinkAction::RemoteBase,
            node: String::new(),
        },
        None,
    );
    stale.remote = Some(RemoteCommand::Mode(1));
    links
        .configure_remote("6", RemoteDtmfAccess::Locked)
        .unwrap();
    assert!(matches!(
        links.command(stale, host.control().work().unwrap(), 3, false),
        Err(AdmissionError::Denied)
    ));
    assert!(matches!(
        links.command(
            self::operation(LinkAction::RemoteBase, ""),
            host.control().work().unwrap(),
            3,
            false
        ),
        Err(AdmissionError::Invalid)
    ));
}

#[test]
fn pending_remote_requests_recheck_the_same_source_policy_without_renewing_it() {
    use crate::remote_base_command::{RemoteCommand, RemoteDtmfAccess};
    let mut links = links();
    links.configure_remote("6", RemoteDtmfAccess::Open).unwrap();
    let mut request = operation(LinkAction::RemoteBase, "");
    request.remote = Some(RemoteCommand::Mode(1));
    assert!(links.remote_authorized(&request, 1));
    links
        .configure_remote("6", RemoteDtmfAccess::Locked)
        .unwrap();
    assert!(!links.remote_authorized(&request, 2));
    request.remote = Some(RemoteCommand::Status { long: false });
    assert!(links.remote_authorized(&request, 2));
    let signals = crate::link::PeerSignals::new();
    request.reply = Some(signals.reply_target());
    request.origin = Some("2000".into());
    assert!(!links.remote_authorized(&request, 2));
    links.accept("2000", true).unwrap();
    for digit in "*651#".chars() {
        let _ = links.peer_digit("2000", digit, 2, request.reply.clone().unwrap());
    }
    assert!(links.remote_authorized(&request, 2));
    let other = crate::link::PeerSignals::new();
    let mut other_request = request.clone();
    other_request.reply = Some(other.reply_target());
    assert!(!links.remote_authorized(&other_request, 2));
    other_request.reply = request.reply.clone();
    other_request.origin = Some("3000".into());
    assert!(!links.remote_authorized(&other_request, 2));
    signals.end();
    assert!(!links.remote_authorized(&request, 2));
    assert!(
        links
            .peer_remote_unkey(3, signals.reply_target())
            .is_empty()
    );
    links.close();
    request.reply = None;
    assert!(!links.remote_authorized(&request, 3));
}

#[test]
fn ordinary_links_without_a_schedule_have_no_priority_group_status() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    assert!(links.priority_groups().is_empty());
}

#[test]
fn read_only_selectors_reject_invalid_arguments_and_keep_the_exact_reply() {
    let mut host = NodeHost::new(generation(1));
    let mut links = links();
    assert!(matches!(
        links.command(
            operation(LinkAction::CancelMessage, ""),
            host.control().work().unwrap(),
            0,
            false,
        ),
        Ok(LinkEffect::CancelMessage)
    ));
    let signals = crate::link::PeerSignals::new();
    for action in [LinkAction::SiteStatus, LinkAction::Message] {
        for selector in ["", "0", "01", "unknown"] {
            assert!(matches!(
                links.command(
                    operation(action, selector),
                    host.control().work().unwrap(),
                    0,
                    false,
                ),
                Err(AdmissionError::Invalid)
            ));
        }
        for reply in [None, Some(signals.reply_target())] {
            let mut request = operation(action, "1");
            request.reply = reply.clone();
            let effect = links
                .command(request, host.control().work().unwrap(), 0, false)
                .unwrap();
            let (selector, actual_reply) = match effect {
                LinkEffect::SiteStatus { selector, reply } if action == LinkAction::SiteStatus => {
                    (selector, reply)
                }
                LinkEffect::Message { selector, reply } if action == LinkAction::Message => {
                    (selector, reply)
                }
                _ => panic!("read-only selector was interpreted as another operation"),
            };
            assert_eq!(selector, "1");
            assert_eq!(actual_reply, reply);
        }
    }
    assert!(links.manager().snapshot().is_empty());
}

#[test]
fn remote_origin_reclamation_preserves_last_node_redial() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    for digits in ["*33000#", "*30#"] {
        let mut operations = Vec::new();
        for digit in digits.chars() {
            if let Some(DigitOutcome::Operation(operation)) =
                links.peer_digit("2000", digit, 1, reply.clone())
            {
                operations.push(operation.command);
            }
        }
        assert_eq!(
            operations,
            vec![crate::command::Command {
                action: LinkAction::Transceive,
                node: "3000".into()
            }]
        );
    }
    assert_eq!(
        links.peer_digit("2000", '5', 2, reply),
        Some(DigitOutcome::Regenerate('5'))
    );
}

#[test]
fn remote_origin_state_is_bounded_and_idle_collectors_are_reclaimed() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    let signals = crate::link::PeerSignals::new();
    let reply = signals.reply_target();
    for origin in 3000..4000 {
        let text = format!("D 524950 {origin} 1 5");
        assert!(matches!(
            links.peer_text("2000", text.as_bytes(), reply.clone(), false, 0, 1),
            Ok(LinkEffect::Digit(DigitOutcome::Regenerate('5')))
        ));
    }
    assert!(
        links.peer_commands.is_empty(),
        "idle regeneration must not retain origin state"
    );
    for origin in 3000..3257 {
        let text = format!("D 524950 {origin} 1 *");
        links
            .peer_text("2000", text.as_bytes(), reply.clone(), false, 0, 1)
            .unwrap();
    }
    assert_eq!(
        links.peer_commands.len(),
        128,
        "bound pending origins per connection"
    );
    assert!(links.expire_peer_digits(4000).is_empty());
    assert!(
        links.peer_commands.is_empty(),
        "timed-out empty commands must be reclaimed"
    );
    assert!(links.peer_digit("2000", '*', 4001, reply).is_none());
    assert_eq!(links.peer_commands.len(), 1);
}

#[test]
fn remote_command_origins_do_not_share_authorization() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    let mut links = links();
    links.configure_admin(&digest, &digest, 60_000);
    links.accept("2000", true).unwrap();
    let signals = crate::link::PeerSignals::new();
    let reply = signals.reply_target();
    for digit in "*8001234#".chars() {
        let text = format!("D 524950 3000 1 {digit}");
        assert!(matches!(
            links.peer_text("2000", text.as_bytes(), reply.clone(), false, 0, 1),
            Ok(LinkEffect::None)
        ));
    }
    for digit in "*804".chars() {
        let text = format!("D 524950 4000 2 {digit}");
        assert!(matches!(
            links.peer_text("2000", text.as_bytes(), reply.clone(), false, 0, 2),
            Ok(LinkEffect::None)
        ));
    }
    let mut authorized = None;
    for digit in "*804".chars() {
        let text = format!("D 524950 3000 3 {digit}");
        if let LinkEffect::Digit(DigitOutcome::Operation(operation)) = links
            .peer_text("2000", text.as_bytes(), reply.clone(), false, 0, 3)
            .unwrap()
        {
            authorized = Some(operation);
        }
    }
    let operation = authorized.unwrap();
    assert_eq!(operation.origin.as_deref(), Some("3000"));
    assert_eq!(operation.reply, Some(reply.clone()));
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(operation, host.control().work().unwrap(), 4, false),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    assert!(matches!(
        links.peer_text("2000", b"D 524950 4000 4 5", reply.clone(), false, 0, 4),
        Ok(LinkEffect::Digit(DigitOutcome::Regenerate('5')))
    ));
    signals.end();
    assert!(
        links
            .peer_text("2000", b"D 524950 3000 4 5", reply, false, 0, 4)
            .is_err()
    );
}

#[test]
fn remote_candidate_does_not_forward_prefix_and_selects_relay_destination() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    let mut host = NodeHost::new(generation(1));
    for digit in "*43000".chars() {
        if let Some(DigitOutcome::RemoteCandidate(candidate)) =
            links.digit(DigitEvent::Digit { digit, now_ms: 1 })
        {
            let valid = candidate.command.node == "3000";
            let effect = links.command(candidate, host.control().work().unwrap(), 1, valid);
            if valid {
                assert!(matches!(effect, Ok(LinkEffect::SelectedRemote)));
            } else {
                assert!(effect.is_err());
            }
        }
    }
    assert!(links.digit(DigitEvent::Unkey { now_ms: 2 }).is_none());
    let Some(DigitOutcome::Operation(digit)) = links.digit(DigitEvent::Digit {
        digit: '5',
        now_ms: 3,
    }) else {
        panic!("selected remote")
    };
    let LinkEffect::ForwardDigits(deliveries) = links
        .command(digit, host.control().work().unwrap(), 3, false)
        .unwrap()
    else {
        panic!("forward")
    };
    assert_eq!(deliveries, [("2000".into(), "D 3000 524950 1 5".into())]);
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '#',
                now_ms: 4
            })
            .is_none()
    );
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '5',
                now_ms: 5
            })
            .is_none()
    );
}

#[test]
fn remote_origin_timeout_finishes_only_its_own_command() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    for digit in "*33000".chars() {
        assert!(matches!(
            links.peer_text(
                "2000",
                format!("D 524950 4000 1 {digit}").as_bytes(),
                reply.clone(),
                false,
                0,
                10
            ),
            Ok(LinkEffect::None)
        ));
    }
    assert!(links.expire_peer_digits(3009).is_empty());
    let expired = links.expire_peer_digits(3010);
    assert_eq!(expired.len(), 1);
    let DigitOutcome::Operation(operation) = &expired[0] else {
        panic!("expired command")
    };
    assert_eq!(operation.command.node, "3000");
    assert_eq!(operation.origin.as_deref(), Some("4000"));
    assert_eq!(operation.reply, Some(reply));
    assert!(links.expire_peer_digits(6010).is_empty());
}

#[test]
fn interleaved_peer_and_local_commands_keep_their_collectors() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    let first = crate::link::PeerSignals::new().reply_target();
    let second = crate::link::PeerSignals::new().reply_target();
    assert!(links.peer_digit("2000", '*', 1, first.clone()).is_none());
    assert!(links.peer_digit("2000", '7', 2, first.clone()).is_none());
    for digit in "*722".chars() {
        links.peer_digit("3000", digit, 3, second.clone());
    }
    for digit in "*70".chars() {
        links.digit(DigitEvent::Digit { digit, now_ms: 4 });
    }
    let reply = links.peer_digit("2000", '0', 5, first.clone());
    assert!(
        reply.is_some(),
        "another input must not consume this peer's partial command"
    );
    let DigitOutcome::Operation(reply) = reply.unwrap() else {
        panic!("operation")
    };
    assert_eq!(reply.command.action, LinkAction::Status);
    assert_eq!(reply.reply, Some(first));
}

#[test]
fn peer_unlock_authorizes_only_that_connection_and_ended_work_is_stale() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};
    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    let mut links = links();
    links.configure_admin(&digest, &digest, 60_000);
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    let signal = crate::link::PeerSignals::new();
    let first = signal.reply_target();
    let second = crate::link::PeerSignals::new().reply_target();
    for digit in "*8001234#".chars() {
        assert!(links.peer_digit("2000", digit, 1, first.clone()).is_none());
    }
    for digit in "*804".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 2 })
                .is_none()
        );
        assert!(links.peer_digit("3000", digit, 2, second.clone()).is_none());
    }
    let mut allowed = None;
    for digit in "*804".chars() {
        allowed = links
            .peer_digit("2000", digit, 3, first.clone())
            .or(allowed);
    }
    let DigitOutcome::Operation(allowed) = allowed.unwrap() else {
        panic!("operation")
    };
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(allowed.clone(), host.control().work().unwrap(), 4, false),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    links.confirm_peer_administrative_command(Some("2000"), &first, 4);
    let mut wrong_origin = allowed.clone();
    wrong_origin.origin = Some("4000".into());
    assert!(matches!(
        links.command(wrong_origin, host.control().work().unwrap(), 5, false),
        Err(AdmissionError::Denied)
    ));
    links.confirm_peer_administrative_command(Some("4000"), &first, 60_000);
    assert!(matches!(
        links.command(
            allowed.clone(),
            host.control().work().unwrap(),
            60_004,
            false
        ),
        Err(AdmissionError::Denied)
    ));
    signal.end();
    assert!(links.peer_digit("2000", '1', 5, first.clone()).is_none());
    assert!(matches!(
        links.command(allowed, host.control().work().unwrap(), 5, false),
        Err(AdmissionError::Stale)
    ));
    let replacement = crate::link::PeerSignals::new().reply_target();
    links.confirm_peer_administrative_command(Some("2000"), &replacement, 5);
    for digit in "*804".chars() {
        assert!(
            links
                .peer_digit("2000", digit, 6, replacement.clone())
                .is_none()
        );
    }
}

#[test]
fn peer_remote_selection_and_disconnect_use_the_exact_collector() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    let first = crate::link::PeerSignals::new().reply_target();
    let second = crate::link::PeerSignals::new().reply_target();
    assert!(links.peer_digit("2000", '*', 0, first.clone()).is_none());
    assert!(links.peer_digit("3000", '*', 0, second.clone()).is_none());
    let mut host = NodeHost::new(generation(1));
    let mut selected = operation(LinkAction::Command, "3000");
    selected.reply = Some(first.clone());
    selected.origin = Some("2000".into());
    assert!(matches!(
        links.command(selected, host.control().work().unwrap(), 1, true),
        Ok(LinkEffect::SelectedRemote)
    ));
    let mut untracked = operation(LinkAction::Command, "3000");
    untracked.reply = Some(crate::link::PeerSignals::new().reply_target());
    assert!(matches!(
        links.command(untracked, host.control().work().unwrap(), 2, true),
        Ok(LinkEffect::SelectedRemote)
    ));
    links.ended("2000");
    assert!(
        links
            .peer_commands
            .iter()
            .all(|(_, source, _)| source != &first)
    );
    assert!(
        links
            .peer_commands
            .iter()
            .any(|(_, source, _)| source == &second)
    );
}

#[test]
fn parrot_actions_require_a_valid_admin_unlock_and_use_live_typed_effects() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let digest = |code: &[u8], salt: &str| {
        Argon2::default()
            .hash_password(code, &SaltString::from_b64(salt).unwrap())
            .unwrap()
            .to_string()
    };
    let mut links = links();
    links.configure_admin(
        &digest(b"1234", "c29tZXNhbHQ"),
        &digest(b"5678", "bG9ja3NhbHQ"),
        60_000,
    );
    let mut host = NodeHost::new(generation(1));
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '*',
                now_ms: 1
            })
            .is_none()
    );
    for digit in "8009999#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 2 })
                .is_none()
        );
    }
    for digit in "*804".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 3 })
                .is_none()
        );
    }
    for digit in "*8001234#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 4 })
                .is_none()
        );
    }
    let mut enabled = None;
    for digit in "*804".chars() {
        enabled = links
            .digit(DigitEvent::Digit { digit, now_ms: 5 })
            .or(enabled);
    }
    assert!(matches!(
        links.command(
            match enabled.unwrap() {
                DigitOutcome::Operation(operation) => operation,
                _ => panic!("operation"),
            },
            host.control().work().unwrap(),
            6,
            false
        ),
        Ok(LinkEffect::ParrotEnabled(true))
    ));
    links.confirm_administrative_command(6);
    assert!(matches!(
        links.command(
            operation(LinkAction::ParrotDisable, ""),
            host.control().work().unwrap(),
            7,
            false,
        ),
        Err(AdmissionError::Denied)
    ));
    for digit in "*8015678#".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 8 })
                .is_none()
        );
    }
    for digit in "*805".chars() {
        assert!(
            links
                .digit(DigitEvent::Digit { digit, now_ms: 9 })
                .is_none()
        );
    }
}

#[test]
fn authorized_parrot_disable_confirms_the_disabled_state() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let digest = Argon2::default()
        .hash_password(b"1234", &SaltString::from_b64("c29tZXNhbHQ").unwrap())
        .unwrap()
        .to_string();
    let mut links = links();
    links.configure_admin(&digest, &digest, 60_000);
    let mut host = NodeHost::new(generation(1));
    for digit in "*8001234#".chars() {
        let _ = links.digit(DigitEvent::Digit { digit, now_ms: 1 });
    }
    let mut disable = None;
    for digit in "*805".chars() {
        disable = links
            .digit(DigitEvent::Digit { digit, now_ms: 2 })
            .or(disable);
    }
    assert!(matches!(
        links.command(
            match disable.unwrap() {
                DigitOutcome::Operation(operation) => operation,
                _ => panic!("operation"),
            },
            host.control().work().unwrap(),
            3,
            false
        ),
        Ok(LinkEffect::ParrotEnabled(false))
    ));
    links.confirm_administrative_command(3);
    assert!(matches!(
        links.command(
            operation(LinkAction::AdminUnlock, ""),
            host.control().work().unwrap(),
            4,
            false,
        ),
        Err(AdmissionError::Denied)
    ));
}

#[test]
fn reconnecting_an_empty_configured_schedule_has_no_detach_effect() {
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(LinkScheduler::new(1, vec![], vec![], None).unwrap()),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    assert!(matches!(
        links.command(
            operation(LinkAction::ReconnectAll, ""),
            host.control().work().unwrap(),
            0,
            false
        ),
        Ok(LinkEffect::None)
    ));
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    host.reclaim();
}

#[test]
fn reconfiguration_preserves_callback_selection_for_an_unchanged_group() {
    use super::super::link_schedule::RouteSpec;
    let routes = || {
        ["2000", "3000"]
            .into_iter()
            .enumerate()
            .map(|(priority, remote)| RouteSpec {
                local: "524950".into(),
                remote: remote.into(),
                permanent: true,
                group_label: Some("north".into()),
                group_name: Some("North network".into()),
                group_priority: Some(priority),
            })
            .collect()
    };
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(LinkScheduler::new(1, routes(), vec![], None).unwrap()),
    )
    .unwrap();
    let member = links.group_member("2000").unwrap();
    member
        .selection()
        .publish_desired(std::num::NonZeroUsize::new(1));
    assert_eq!(member.selection().callback_slot(true), 1);

    links.reconfigure(
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(LinkScheduler::new(2, routes(), vec![], None).unwrap()),
    );

    let retained = links.group_member("2000").unwrap();
    assert_eq!(retained.selection().active(), Some(retained.slot()));
}

#[test]
fn priority_status_distinguishes_unattempted_reachable_and_ended_members() {
    use super::super::link_schedule::RouteSpec;
    let schedule = LinkScheduler::new(
        1,
        vec![RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
            group_label: Some("north".into()),
            group_name: Some("North".into()),
            group_priority: Some(0),
        }],
        vec![],
        None,
    )
    .unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();

    assert!(!links.priority_groups()[0].unavailable);
    links.accept("2000", true).unwrap();
    assert!(!links.priority_groups()[0].unavailable);
    links.ended("2000");
    assert!(links.priority_groups()[0].unavailable);
}

#[test]
fn unconfigured_schedule_and_closed_or_stale_admission_do_not_reserve_work() {
    let mut links = links();
    let mut host = NodeHost::new(generation(1));
    links.tick(
        CivilTime::new(2026, 9, 15, crate::schedule::Weekday::Tuesday, 12, 0).unwrap(),
        0,
        0,
        |_| None,
    );
    assert!(
        links
            .next_scheduled(host.control().work().unwrap())
            .is_none()
    );
    for action in [LinkAction::DisconnectAll, LinkAction::ReconnectAll] {
        assert!(
            links
                .command(
                    operation(action, ""),
                    host.control().work().unwrap(),
                    0,
                    false
                )
                .is_ok()
        );
    }
    links.accept("2000", true).unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            0,
            true
        ),
        Err(AdmissionError::Loop)
    ));
    let stale = host.control().work().unwrap();
    let retry_stale = host.control().work().unwrap();
    host.control().publish(generation(2), 0).unwrap();
    assert!(links.next_scheduled(stale).is_none());
    assert!(links.take_retry(0, retry_stale).is_none());
    links.close();
    assert!(
        links
            .next_scheduled(host.control().work().unwrap())
            .is_none()
    );
    host.control().stop(0).unwrap();
    host.control().mark_detached(1).unwrap();
    host.control().mark_detached(2).unwrap();
    host.reclaim();
}
#[test]
fn copied_peer_text_updates_topology_relays_advice_and_detaches_loops() {
    let mut links = links();
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    assert_eq!(links.due_topology(0).len(), 2);
    assert!(links.due_topology(1).is_empty());
    let LinkEffect::Text(messages) = links
        .peer_text(
            "2000",
            b"K? * 2000 0 0",
            crate::link::PeerSignals::new().reply_target(),
            true,
            1000,
            4000,
        )
        .unwrap()
    else {
        panic!("key advice")
    };
    assert_eq!(messages[0], ("2000".into(), "K 2000 524950 1 3".into()));
    assert_eq!(messages[1], ("3000".into(), "K? * 2000 0 0".into()));
    assert!(matches!(
        links.peer_text(
            "2000",
            b"L T4000",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            5000
        ),
        Ok(LinkEffect::None)
    ));
    assert!(links.manager().reaches("4000", false));
    assert_eq!(links.due_topology(5000).len(), 2);
    assert!(matches!(
        links.peer_text("2000", b"L T3000", crate::link::PeerSignals::new().reply_target(), false, 0, 6000),
        Ok(LinkEffect::Detach(peers)) if peers == ["2000"]
    ));
    assert_eq!(
        links
            .peer_text(
                "2000",
                b"L T5000",
                crate::link::PeerSignals::new().reply_target(),
                false,
                0,
                7000
            )
            .err(),
        Some(AdmissionError::Invalid)
    );
    assert!(
        links
            .due_topology(7000)
            .iter()
            .all(|(name, _)| name == "3000")
    );
}

#[test]
fn administrative_cancellation_and_foreign_scheduled_dials_do_not_change_links() {
    let mut host = NodeHost::new(generation(1));
    let mut local = links();
    let LinkEffect::Connect(attempt) = local
        .command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            0,
            true,
        )
        .unwrap()
    else {
        panic!("administrative dial")
    };
    local.cancel_connect(attempt);
    assert!(local.manager().snapshot().is_empty());
    let LinkEffect::Connect(attempt) = local
        .command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            0,
            true,
        )
        .unwrap()
    else {
        panic!("administrative dial")
    };
    local.close();
    assert_eq!(
        local.finish_connect_at(attempt, true, 0, None, |_| None),
        Err(AdmissionError::Stale)
    );
    let mut local = links();
    let schedule = LinkScheduler::new(
        1,
        vec![super::super::link_schedule::RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
            group_label: None,
            group_name: None,
            group_priority: None,
        }],
        vec![],
        None,
    )
    .unwrap();
    let mut scheduled = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let LinkEffect::Connect(attempt) = scheduled
        .next_scheduled(host.control().work().unwrap())
        .unwrap()
    else {
        panic!("scheduled dial")
    };
    assert_eq!(
        local.finish_connect_at(attempt, true, 0, None, |_| None),
        Err(AdmissionError::Stale)
    );
}
#[test]
fn slow_connect_revalidates_generation_and_new_loop_before_publication() {
    let mut host = NodeHost::new(generation(1));
    let (mut control, _, _) = host.split();
    let mut links = links();
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::Transceive, "2000"),
            control.work().unwrap(),
            0,
            false,
        )
        .unwrap()
    else {
        panic!("dial")
    };
    control.publish(generation(2), 1).unwrap();
    assert!(!attempt.current());
    assert_eq!(
        links.finish_connect_at(attempt, true, 2, None, |_| None),
        Err(AdmissionError::Stale)
    );
    assert!(!links.manager.reaches("2000", true));
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::Transceive, "3000"),
            control.work().unwrap(),
            2,
            false,
        )
        .unwrap()
    else {
        panic!("dial")
    };
    links.accept("3000", true).unwrap();
    assert_eq!(
        links.finish_connect_at(attempt, true, 3, None, |_| None),
        Err(AdmissionError::Loop)
    );
}
#[test]
fn permanent_failure_retains_retry_and_operator_disconnect_cancels_it() {
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::PermanentTransceive, "2000"),
            control.work().unwrap(),
            0,
            false,
        )
        .unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, false, 10, None, |_| None),
        Ok(false)
    );
    assert!(links.manager.reaches("2000", true));
    let retry = links.take_retry(10, control.work().unwrap()).unwrap();
    links
        .command(
            operation(LinkAction::DisconnectPermanent, "2000"),
            control.work().unwrap(),
            11,
            false,
        )
        .unwrap();
    assert_eq!(
        links.finish_retry(retry, true, 12),
        Err(AdmissionError::Stale)
    );
    assert!(!links.manager.reaches("2000", true));
}

#[test]
fn disconnect_all_and_reconnect_all_only_change_permanent_links() {
    let mut host = NodeHost::new(generation(1));
    let mut links = links();
    for (action, remote) in [
        (LinkAction::PermanentTransceive, "2000"),
        (LinkAction::Transceive, "3000"),
    ] {
        let LinkEffect::Connect(attempt) = links
            .command(
                operation(action, remote),
                host.control().work().unwrap(),
                0,
                false,
            )
            .unwrap()
        else {
            panic!("expected dial for {remote}");
        };
        assert!(
            links
                .finish_connect_at(attempt, true, 0, None, |_| None)
                .unwrap()
        );
    }

    assert!(matches!(
        links.command(
            operation(LinkAction::DisconnectPermanentAll, ""),
            host.control().work().unwrap(),
            1,
            false,
        ),
        Ok(LinkEffect::Detach(names)) if names == ["2000"]
    ));
    let status = links.manager().snapshot();
    assert!(
        status
            .iter()
            .any(|peer| peer.name == "3000" && !peer.permanent)
    );
    assert!(status.iter().any(|peer| peer.name == "2000" && peer.paused));

    assert!(matches!(
        links.command(
            operation(LinkAction::ReconnectPermanentAll, ""),
            host.control().work().unwrap(),
            2,
            false,
        ),
        Ok(LinkEffect::None)
    ));
    let retry = links.take_retry(2, host.control().work().unwrap()).unwrap();
    assert_eq!(retry.remote(), "2000");
    assert!(links.finish_retry(retry, true, 3).unwrap());
    let status = links.manager().snapshot();
    assert!(
        status
            .iter()
            .any(|peer| peer.name == "3000" && !peer.permanent)
    );
    assert!(
        status
            .iter()
            .any(|peer| peer.name == "2000" && peer.permanent)
    );
}

#[test]
fn permanent_transceive_command_resumes_one_disconnect_all_paused_node() {
    let mut host = NodeHost::new(generation(1));
    let mut links = links();
    let LinkEffect::Connect(attempt) = links
        .command(
            operation(LinkAction::PermanentTransceive, "2000"),
            host.control().work().unwrap(),
            0,
            false,
        )
        .unwrap()
    else {
        panic!("expected initial dial");
    };
    links
        .finish_connect_at(attempt, true, 0, None, |_| None)
        .unwrap();
    links
        .command(
            operation(LinkAction::DisconnectPermanentAll, ""),
            host.control().work().unwrap(),
            1,
            false,
        )
        .unwrap();

    assert!(matches!(
        links.command(
            operation(LinkAction::PermanentTransceive, "2000"),
            host.control().work().unwrap(),
            2,
            false,
        ),
        Ok(LinkEffect::None)
    ));
    let retry = links.take_retry(2, host.control().work().unwrap()).unwrap();
    assert_eq!(retry.remote(), "2000");
    assert!(retry.mode().transmits());
    assert!(links.finish_retry(retry, true, 3).unwrap());
    assert!(
        links
            .manager()
            .snapshot()
            .iter()
            .any(|peer| peer.name == "2000" && peer.permanent && !peer.retrying && !peer.paused)
    );
}

#[test]
fn topology_blocked_automatic_route_retries_only_after_explicit_evidence_change() {
    use super::super::link_schedule::RouteSpec;
    let schedule = |generation| {
        LinkScheduler::new(
            generation,
            vec![RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: true,
                group_label: None,
                group_name: None,
                group_priority: None,
            }],
            vec![],
            None,
        )
        .unwrap()
    };
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule(1)),
    )
    .unwrap();
    links.accept("2000", true).unwrap();
    assert!(matches!(
        links.peer_text(
            "2000",
            b"L T3000",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            0
        ),
        Ok(LinkEffect::None)
    ));
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("configured dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, None, |_| None),
        Err(AdmissionError::Loop)
    );
    let status = links
        .manager()
        .snapshot()
        .into_iter()
        .find(|status| status.name == "3000")
        .unwrap();
    assert!(status.retrying && status.topology_blocked);
    assert_eq!(status.due_ms, None);
    assert!(
        links
            .take_retry(u64::MAX, control.work().unwrap())
            .is_none()
    );

    assert!(matches!(
        links.command(
            operation(LinkAction::ReconnectAll, ""),
            control.work().unwrap(),
            1,
            false
        ),
        Ok(LinkEffect::None)
    ));
    let retry = links.take_retry(1, control.work().unwrap()).unwrap();
    assert_eq!(
        links.finish_retry(retry, true, 1),
        Err(AdmissionError::Loop)
    );

    links.reconfigure(
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule(2)),
    );
    let retry = links.take_retry(2, control.work().unwrap()).unwrap();
    assert_eq!(
        links.finish_retry(retry, true, 2),
        Err(AdmissionError::Loop)
    );

    links.accept("4000", true).unwrap();
    links
        .peer_text(
            "4000",
            b"L T5000",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            3,
        )
        .unwrap();
    assert!(
        links
            .take_retry(u64::MAX, control.work().unwrap())
            .is_none()
    );
    links.ended("2000");
    links.reclaimed("2000", 4);
    assert!(
        links
            .take_retry(u64::MAX, control.work().unwrap())
            .is_none()
    );

    links.accept("2000", true).unwrap();
    links
        .peer_text(
            "2000",
            b"L T6000",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            5,
        )
        .unwrap();
    let retry = links.take_retry(5, control.work().unwrap()).unwrap();
    assert_eq!(links.finish_retry(retry, false, 5), Ok(false));
    let status = links
        .manager()
        .snapshot()
        .into_iter()
        .find(|status| status.name == "3000")
        .unwrap();
    assert!(!status.topology_blocked);
    assert_eq!(status.due_ms, Some(1005));
}

#[test]
fn group_member_is_not_topology_blocked_but_other_routes_are() {
    use super::super::link_schedule::RouteSpec;
    use crate::schedule::Weekday;
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let schedule = LinkScheduler::new(
        1,
        vec![
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: true,
                group_label: Some("network".into()),
                group_name: Some("Network".into()),
                group_priority: Some(0),
            },
            RouteSpec {
                local: "524950".into(),
                remote: "4000".into(),
                permanent: true,
                group_label: Some("network".into()),
                group_name: Some("Network".into()),
                group_priority: Some(1),
            },
            RouteSpec {
                local: "524950".into(),
                remote: "5000".into(),
                permanent: true,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
        ],
        vec![],
        None,
    )
    .unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    links.accept("2000", true).unwrap();
    links
        .peer_text(
            "2000",
            b"L T3000,T4000,T5000",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            1,
        )
        .unwrap();
    let civil = CivilTime::new(2026, 9, 15, Weekday::Tuesday, 11, 0).unwrap();
    links.tick(civil, 0, 1, |_| None);

    let LinkEffect::Connect(group) = links.next_scheduled(control.work().unwrap()).unwrap() else {
        panic!("group member dial")
    };
    assert_eq!(group.remote(), "3000");
    assert_eq!(
        links.finish_connect_at(group, true, 2, None, |_| None),
        Ok(true)
    );
    assert!(matches!(
        links.peer_text(
            "3000",
            b"L T2000",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            2
        ),
        Ok(LinkEffect::None)
    ));
    links.ended("3000");
    links.reclaimed("3000", 3);
    let retry = links.take_retry(3, control.work().unwrap()).unwrap();
    assert_eq!(links.finish_retry(retry, true, 4), Ok(true));
    assert!(
        links
            .manager()
            .snapshot()
            .iter()
            .any(|peer| peer.name == "3000" && !peer.topology_blocked)
    );

    let LinkEffect::Connect(group) = links.next_scheduled(control.work().unwrap()).unwrap() else {
        panic!("second group member dial")
    };
    assert_eq!(group.remote(), "4000");
    assert_eq!(
        links.finish_connect_at(group, true, 3, None, |_| None),
        Ok(true)
    );

    let LinkEffect::Connect(other) = links.next_scheduled(control.work().unwrap()).unwrap() else {
        panic!("other permanent dial")
    };
    assert_eq!(other.remote(), "5000");
    assert_eq!(
        links.finish_connect_at(other, true, 4, None, |_| None),
        Err(AdmissionError::Loop)
    );
}

#[test]
fn priority_group_status_reports_only_callback_active_peer_and_complete_outage() {
    use super::super::link_schedule::RouteSpec;
    let schedule = || {
        LinkScheduler::new(
            1,
            ["2000", "3000"]
                .into_iter()
                .enumerate()
                .map(|(priority, remote)| RouteSpec {
                    local: "524950".into(),
                    remote: remote.into(),
                    permanent: true,
                    group_label: Some("network".into()),
                    group_name: Some("Blind Hams Network".into()),
                    group_priority: Some(priority),
                })
                .collect(),
            vec![],
            None,
        )
        .unwrap()
    };
    let mut unavailable = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule()),
    )
    .unwrap();
    assert!(!unavailable.priority_groups()[0].unavailable);
    for remote in ["2000", "3000"] {
        unavailable
            .manager
            .retain_retry_member(remote, Mode::MONITOR, 0, unavailable.group_member(remote))
            .unwrap();
    }
    let status = unavailable.priority_groups();
    assert!(status[0].unavailable);
    assert_eq!(status[0].name, "Blind Hams Network");

    let mut selected = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule()),
    )
    .unwrap();
    let group = selected.group_member("2000").unwrap();
    selected
        .manager
        .attach_member("2000", Mode::TRANSCEIVE, true, Some(group.clone()))
        .unwrap();
    assert_eq!(group.selection().callback_slot(true), 1);
    let status = selected.priority_groups();
    assert_eq!(status[0].selected.as_deref(), Some("2000"));
    assert!(!status[0].unavailable);
}

#[test]
fn callback_group_selection_republishes_topology_immediately() {
    use super::super::link_schedule::RouteSpec;
    use crate::schedule::Weekday;

    let routes = [("3000", 0), ("4000", 1)]
        .into_iter()
        .map(|(remote, priority)| RouteSpec {
            local: "524950".into(),
            remote: remote.into(),
            permanent: true,
            group_label: Some("network".into()),
            group_name: Some("Network".into()),
            group_priority: Some(priority),
        })
        .collect();
    let schedule = LinkScheduler::new(1, routes, vec![], None).unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    links.tick(
        CivilTime::new(2026, 9, 15, Weekday::Tuesday, 11, 0).unwrap(),
        0,
        0,
        |_| None,
    );
    for _ in 0..2 {
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("permanent group dial")
        };
        assert!(
            links
                .finish_connect_at(attempt, true, 1, None, |_| None)
                .unwrap()
        );
    }

    let member = links.group_member("3000").unwrap();
    assert!(
        links
            .due_topology(2)
            .iter()
            .any(|(peer, text)| peer == "4000" && text == "L L3000")
    );

    assert_eq!(member.selection().callback_slot(true), 1);
    assert!(
        links
            .due_topology(3)
            .iter()
            .any(|(peer, text)| peer == "4000" && text == "L T3000")
    );

    let modes = links
        .manager()
        .snapshot()
        .into_iter()
        .map(|peer| (peer.name, peer.mode.transmits()))
        .collect::<Vec<_>>();
    assert_eq!(modes, [("3000".into(), true), ("4000".into(), false)]);
}

#[test]
fn incoming_and_remote_selection_use_current_deny_first_policy() {
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    assert_eq!(links.accept("2000", false), Err(AdmissionError::Denied));
    links.accept("2000", true).unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Command, "2000"),
            control.work().unwrap(),
            0,
            true
        ),
        Ok(LinkEffect::SelectedRemote)
    ));
    links.reconfigure(
        AccessPolicy::new("", "2000").unwrap(),
        DtmfCommandMap::standard(),
        None,
    );
    let reply = crate::link::PeerSignals::new().reply_target();
    assert!(matches!(
        links.peer_text("2000", b"D 524950 3000 1 5", reply, false, 0, 0),
        Err(AdmissionError::Denied)
    ));
    assert_eq!(
        links
            .command(
                operation(LinkAction::Command, "2000"),
                control.work().unwrap(),
                0,
                true
            )
            .err(),
        Some(AdmissionError::Denied)
    );
}

#[test]
fn remote_command_rejects_self_unreachable_and_invalid_digits_without_advancing_sequence() {
    let mut host = NodeHost::new(generation(1));
    let mut links = links();
    for remote in ["524950", "2000"] {
        assert!(matches!(
            links.command(
                operation(LinkAction::Command, remote),
                host.control().work().unwrap(),
                0,
                true,
            ),
            Err(AdmissionError::Denied)
        ));
    }
    links.accept("2000", true).unwrap();
    links.accept("3000", true).unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    assert!(matches!(
        links.peer_text("2000", b"D 3000 4000 7 5", reply, false, 0, 0),
        Ok(LinkEffect::ForwardDigits(deliveries))
            if deliveries == [("3000".into(), "D 3000 4000 7 5".into())]
    ));
    for digit in ['x', '\u{0131}'] {
        let mut invalid = operation(LinkAction::Command, "2000");
        invalid.digit = Some(digit);
        assert!(matches!(
            links.command(invalid, host.control().work().unwrap(), 1, false),
            Err(AdmissionError::Invalid)
        ));
    }
    let mut valid = operation(LinkAction::Command, "2000");
    valid.digit = Some('5');
    assert!(matches!(
        links.command(valid, host.control().work().unwrap(), 2, false),
        Ok(LinkEffect::ForwardDigits(deliveries))
            if deliveries == [("2000".into(), "D 2000 524950 1 5".into())]
    ));
}

#[test]
fn configured_dial_rechecks_window_boundary_and_requires_clock_only_for_windows() {
    use super::super::link_schedule::{ReplacementSpec, RouteSpec};
    use crate::schedule::{ScheduledWindow, Weekday};
    let clock = |hour| CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
    for window in [false, true] {
        let mut host = NodeHost::new(generation(1));
        let (control, _, _) = host.split();
        let routes = vec![
            RouteSpec {
                local: "524950".into(),
                remote: "2000".into(),
                permanent: true,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
        ];
        let windows = if window {
            vec![ReplacementSpec {
                routes: vec![1],
                replaced: vec![0],
                window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
                end_inactivity_ms: 0,
                warning_before_start_ms: Vec::new(),
                warning_before_end_ms: Vec::new(),
                warning_message_id: None,
            }]
        } else {
            vec![]
        };
        let schedule = LinkScheduler::new(1, routes, windows, None).unwrap();
        let mut links = NodeLinkControl::new(
            "524950",
            AccessPolicy::new("", "").unwrap(),
            DtmfCommandMap::standard(),
            Some(schedule),
        )
        .unwrap();
        links.tick(clock(11), 0, 1, |_| None);
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("dial")
        };
        let result =
            links.finish_connect_at(attempt, true, 2, window.then_some((clock(12), 0)), |_| None);
        assert_eq!(
            result,
            if window {
                Err(AdmissionError::Stale)
            } else {
                Ok(true)
            }
        );
        assert_eq!(links.manager.reaches("2000", true), !window);
    }
}

#[test]
fn dial_modes_failed_temporary_calls_and_telemetry_preserve_command_semantics() {
    for (action, mode, permanent) in [
        (LinkAction::Monitor, Mode::MONITOR, false),
        (LinkAction::LocalMonitor, Mode::LOCAL_MONITOR, false),
        (LinkAction::Transceive, Mode::TRANSCEIVE, false),
        (LinkAction::PermanentMonitor, Mode::MONITOR, true),
        (LinkAction::PermanentLocalMonitor, Mode::LOCAL_MONITOR, true),
        (LinkAction::PermanentTransceive, Mode::TRANSCEIVE, true),
    ] {
        let mut host = NodeHost::new(generation(1));
        let (control, _, _) = host.split();
        let mut links = links();
        let LinkEffect::Connect(attempt) = links
            .command(operation(action, "2000"), control.work().unwrap(), 0, false)
            .unwrap()
        else {
            panic!("dial")
        };
        assert_eq!(
            (attempt.remote(), attempt.mode(), attempt.permanent()),
            ("2000", mode, permanent)
        );
        assert!(attempt.current());
        assert_eq!(
            links.finish_connect_at(attempt, false, 1, None, |_| None),
            Ok(false)
        );
        assert_eq!(links.manager().owns_permanent("2000"), permanent);
    }
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    for action in [
        LinkAction::Status,
        LinkAction::FullStatus,
        LinkAction::LastKeyed,
        LinkAction::Time,
    ] {
        assert!(
            matches!(links.command(operation(action, ""), control.work().unwrap(), 0, false), Ok(LinkEffect::Telemetry(found, None)) if found == action)
        );
    }
    assert!(
        matches!(links.command(operation(LinkAction::Disconnect, "2000"), control.work().unwrap(), 0, false), Ok(LinkEffect::Detach(names)) if names.is_empty())
    );
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "524950"),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Loop)
    ));
    links.policy = AccessPolicy::new("", "2000").unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "2000"),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Denied)
    ));
}

#[test]
fn remote_selection_forwards_digits_and_temporary_disconnect_preserves_permanents() {
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    let mut links = links();
    links.accept("2000", true).unwrap();
    links
        .manager
        .attach("3000", Mode::TRANSCEIVE, true)
        .unwrap();
    let reply = crate::link::PeerSignals::new().reply_target();
    assert!(links.peer_digit("missing", '*', 0, reply.clone()).is_none());
    assert!(links.peer_digit("2000", '*', 0, reply).is_none());
    assert!(matches!(
        links.command(
            operation(LinkAction::Command, "2000"),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Denied)
    ));
    links
        .command(
            operation(LinkAction::Command, "2000"),
            control.work().unwrap(),
            0,
            true,
        )
        .unwrap();
    let DigitOutcome::Operation(forwarded) = links
        .digit(DigitEvent::Digit {
            digit: '5',
            now_ms: 1,
        })
        .unwrap()
    else {
        panic!("operation")
    };
    assert!(
        matches!(links.command(forwarded, control.work().unwrap(), 1, false), Ok(LinkEffect::ForwardDigits(deliveries)) if deliveries == [("2000".into(), "D 2000 524950 1 5".into())])
    );
    assert!(
        matches!(links.command(operation(LinkAction::DisconnectNonPermanentAll, ""), control.work().unwrap(), 2, false), Ok(LinkEffect::Detach(names)) if names == ["2000"])
    );
    assert!(links.manager().owns_permanent("3000"));
    assert!(
        links
            .digit(DigitEvent::Digit {
                digit: '6',
                now_ms: 3
            })
            .is_none()
    );
}

#[test]
fn copied_peer_negotiation_disconnect_and_closed_admission_have_no_inline_effects() {
    let mut host = NodeHost::new(generation(1));
    let (mut control, _, _) = host.split();
    let mut links = links();
    links.accept("2000", true).unwrap();
    for bytes in [b"!NEWKEY1!".as_slice(), b"!IAXKEY!"] {
        assert!(matches!(
            links.peer_text(
                "2000",
                bytes,
                crate::link::PeerSignals::new().reply_target(),
                false,
                0,
                0
            ),
            Ok(LinkEffect::None)
        ));
    }
    assert!(matches!(
        links.peer_text(
            "2000",
            b"bad",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            0
        ),
        Err(AdmissionError::Invalid)
    ));
    assert!(
        matches!(links.peer_text("2000", b"!!DISCONNECT!!", crate::link::PeerSignals::new().reply_target(), false, 0, 0), Ok(LinkEffect::Detach(names)) if names == ["2000"])
    );
    links.reclaimed("2000", 0);
    let stale = control.work().unwrap();
    control.publish(generation(2), 0).unwrap();
    assert!(matches!(
        links.command(operation(LinkAction::Status, ""), stale, 0, false),
        Err(AdmissionError::Stale)
    ));
    links.close();
    assert_eq!(links.accept("2000", true), Err(AdmissionError::Stale));
    assert!(matches!(
        links.peer_text(
            "2000",
            b"L",
            crate::link::PeerSignals::new().reply_target(),
            false,
            0,
            0
        ),
        Err(AdmissionError::Stale)
    ));
    assert!(links.take_retry(0, control.work().unwrap()).is_none());
    assert!(links.next_scheduled(control.work().unwrap()).is_none());
    assert!(matches!(
        links.command(
            operation(LinkAction::Status, ""),
            control.work().unwrap(),
            0,
            false
        ),
        Err(AdmissionError::Stale)
    ));
}

#[test]
fn retry_completion_rechecks_current_generation_policy_and_answer() {
    for outcome in ["answered", "unanswered", "denied", "stale", "closed"] {
        let mut host = NodeHost::new(generation(1));
        let (mut control, _, _) = host.split();
        let mut links = links();
        links
            .manager
            .retain_retry("2000", Mode::MONITOR, 0)
            .unwrap();
        let retry = links.take_retry(0, control.work().unwrap()).unwrap();
        assert_eq!((retry.remote(), retry.mode()), ("2000", Mode::MONITOR));
        assert!(retry.current());
        match outcome {
            "denied" => links.policy = AccessPolicy::new("", "2000").unwrap(),
            "stale" => {
                control.publish(generation(2), 1).unwrap();
                assert!(!retry.current());
            }
            "closed" => {
                links.close();
            }
            _ => {}
        }
        assert_eq!(
            links.finish_retry(retry, outcome != "unanswered", 2),
            match outcome {
                "answered" => Ok(true),
                "unanswered" => Ok(false),
                "denied" => Err(AdmissionError::Denied),
                _ => Err(AdmissionError::Stale),
            }
        );
    }
}

#[test]
fn scheduled_cancel_clock_failure_and_window_withdrawal_release_exact_reservations() {
    use super::super::link_schedule::{ReplacementSpec, RouteSpec};
    use crate::schedule::{ScheduledWindow, Weekday};
    let civil = |hour| CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
    let schedule = LinkScheduler::new(
        1,
        vec![
            RouteSpec {
                local: "524950".into(),
                remote: "2000".into(),
                permanent: true,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
            RouteSpec {
                local: "524950".into(),
                remote: "3000".into(),
                permanent: false,
                group_label: None,
                group_name: None,
                group_priority: None,
            },
        ],
        vec![ReplacementSpec {
            routes: vec![1],
            replaced: vec![0],
            window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        }],
        None,
    )
    .unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    let (control, _, _) = host.split();
    links.tick(civil(11), 0, 0, |_| None);
    let LinkEffect::Connect(cancelled) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    links.cancel_connect(cancelled);
    for clock in [None, Some((civil(11), 60))] {
        let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
        else {
            panic!("dial")
        };
        assert_eq!(
            links.finish_connect_at(attempt, true, 0, clock, |_| None),
            Err(AdmissionError::Stale)
        );
    }
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    links.policy = AccessPolicy::new("", "2000").unwrap();
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, Some((civil(11), 0)), |_| None),
        Err(AdmissionError::Denied)
    );
    links.policy = AccessPolicy::new("", "").unwrap();
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(
        links.finish_connect_at(attempt, true, 0, Some((civil(11), 0)), |_| None),
        Ok(true)
    );
    links.tick(civil(12), 0, 1, |_| None);
    assert!(
        matches!(links.next_scheduled(control.work().unwrap()), Some(LinkEffect::Detach(names)) if names == ["2000"])
    );
    links.reclaimed("2000", 1);
    let LinkEffect::Connect(attempt) = links.next_scheduled(control.work().unwrap()).unwrap()
    else {
        panic!("dial")
    };
    assert_eq!(attempt.remote(), "3000");
    assert_eq!(
        links.finish_connect_at(attempt, true, 1, Some((civil(12), 0)), |_| None),
        Ok(true)
    );
    links
        .command(
            operation(LinkAction::DisconnectAll, ""),
            control.work().unwrap(),
            2,
            false,
        )
        .unwrap();
    links.reclaimed("3000", 2);
    links.tick(civil(13), 0, 3, |_| None);
    assert!(
        matches!(links.command(operation(LinkAction::ReconnectAll, ""), control.work().unwrap(), 3, false), Ok(LinkEffect::Detach(names)) if names == ["3000"])
    );
    assert!(matches!(
        links.next_scheduled(control.work().unwrap()),
        Some(LinkEffect::Connect(_))
    ));
}

#[test]
fn reconnect_permanent_all_detaches_scheduled_route_outside_its_window() {
    use super::super::link_schedule::{ReplacementSpec, RouteSpec};
    use crate::schedule::{ScheduledWindow, Weekday};

    let clock = |hour| CivilTime::new(2026, 9, 15, Weekday::Tuesday, hour, 0).unwrap();
    let routes = vec![
        RouteSpec {
            local: "524950".into(),
            remote: "2000".into(),
            permanent: true,
            group_label: None,
            group_name: None,
            group_priority: None,
        },
        RouteSpec {
            local: "524950".into(),
            remote: "3000".into(),
            permanent: false,
            group_label: None,
            group_name: None,
            group_priority: None,
        },
    ];
    let schedule = LinkScheduler::new(
        1,
        routes,
        vec![ReplacementSpec {
            routes: vec![1],
            replaced: vec![0],
            window: ScheduledWindow::parse(None, None, "12:00", "13:00").unwrap(),
            end_inactivity_ms: 0,
            warning_before_start_ms: Vec::new(),
            warning_before_end_ms: Vec::new(),
            warning_message_id: None,
        }],
        None,
    )
    .unwrap();
    let mut links = NodeLinkControl::new(
        "524950",
        AccessPolicy::new("", "").unwrap(),
        DtmfCommandMap::standard(),
        Some(schedule),
    )
    .unwrap();
    let mut host = NodeHost::new(generation(1));
    links.tick(clock(12), 0, 0, |_| None);
    let LinkEffect::Connect(attempt) = links
        .next_scheduled(host.control().work().unwrap())
        .unwrap()
    else {
        panic!("scheduled replacement should connect");
    };
    assert!(
        links
            .finish_connect_at(attempt, true, 0, Some((clock(12), 0)), |_| None)
            .unwrap()
    );
    assert!(matches!(
        links.command(
            operation(LinkAction::DisconnectPermanentAll, ""),
            host.control().work().unwrap(),
            1,
            false,
        ),
        Ok(LinkEffect::Detach(names)) if names == ["3000"]
    ));
    links.tick(clock(13), 0, 1, |_| None);

    assert!(matches!(
        links.command(
            operation(LinkAction::ReconnectPermanentAll, ""),
            host.control().work().unwrap(),
            2,
            false,
        ),
        Ok(LinkEffect::Detach(names)) if names == ["3000"]
    ));
}

#[test]
fn connect_commands_distinguish_temporary_links_and_existing_loops() {
    let mut links = links();
    let mut host = NodeHost::new(generation(1));

    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "2000"),
            host.control().work().unwrap(),
            1,
            false,
        ),
        Ok(LinkEffect::Connect(attempt)) if attempt.remote() == "2000" && !attempt.permanent()
    ));

    links
        .manager
        .attach("3000", Mode::TRANSCEIVE, false)
        .unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "3000"),
            host.control().work().unwrap(),
            2,
            false,
        ),
        Err(AdmissionError::Loop)
    ));
    links
        .manager
        .attach("4000", Mode::TRANSCEIVE, true)
        .unwrap();
    assert!(matches!(
        links.command(
            operation(LinkAction::PermanentTransceive, "4000"),
            host.control().work().unwrap(),
            3,
            false,
        ),
        Err(AdmissionError::Loop)
    ));
    assert!(matches!(
        links.command(
            operation(LinkAction::Transceive, "524950"),
            host.control().work().unwrap(),
            4,
            false,
        ),
        Err(AdmissionError::Loop)
    ));
}
