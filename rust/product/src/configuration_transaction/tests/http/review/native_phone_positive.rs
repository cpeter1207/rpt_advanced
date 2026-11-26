//! Product and native SIP interop with only the physical RF edge under fixture control.
use super::*;
use super::{
    phone_caller::Caller,
    phone_owner::Owners,
    private_phone::{register, registrar},
};
use std::net::{SocketAddr, UdpSocket};

struct Running {
    owners: Option<Box<Owners>>,
    table: Option<Box<crate::abi::rptadv_host_services_v11>>,
    binding: Option<Box<crate::abi::rptadv_station_binding_v1>>,
    active: bool,
}
impl Running {
    fn start(document: &[u8]) -> Self {
        let mut owners = Owners::new();
        let binding = Box::new(ops());
        let table = Box::new(owners.table(&binding));
        let manage =
            unsafe { &*crate::lifecycle::bootstrap::rptadv_product_management_descriptor_v1() };
        let mut running = Self {
            owners: Some(owners),
            binding: Some(binding),
            table: Some(table),
            active: false,
        };
        let status = unsafe {
            (manage.start.unwrap())(
                running.table.as_deref().unwrap(),
                fixture::control_descriptor().cast(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                document.as_ptr().cast(),
                document.len(),
            )
        };
        running.active = matches!(status, 0 | -2);
        assert_eq!(status, 0);
        running
    }
    fn stop(&mut self) -> bool {
        let product = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
        let until = Instant::now() + Duration::from_secs(8);
        while Instant::now() < until {
            if self.active {
                self.active = unsafe { product.stop.unwrap()() } != 0;
            }
            if !self.active && self.owners.as_ref().unwrap().joined() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.stop() {
            // This opt-in fixture runs in its own child. An uncertain native owner
            // retains the borrowed descriptors even during assertion unwinding.
            let _ = self.owners.take().map(Box::leak);
            let _ = self.table.take().map(Box::leak);
            let _ = self.binding.take().map(Box::leak);
        }
    }
}

fn mailbox_access(
    client: &Client<'_>,
    account: &str,
    socket: &UdpSocket,
    contact: SocketAddr,
    output: &fixture::ReviewOutput,
) -> (bool, bool, bool, bool) {
    let count = |value: &std::sync::atomic::AtomicUsize| value.load(Ordering::Acquire);
    let station = || {
        [
            count(&fixture::PHONE_ROUTE_INSTALLS),
            count(&output.keyed),
            count(&output.audible),
        ]
    };
    let mut blocked = [false; 2];
    for (index, number) in ["741", "7410"].into_iter().enumerate() {
        let snapshot = station();
        let verified = count(&fixture::PHONE_MAILBOX_VERIFIED);
        let rejected = count(&fixture::PHONE_MAILBOX_REJECTED);
        let mut caller = Caller::connect(socket, contact, index as u32 + 1, true);
        caller.audible_greeting();
        caller.digits(&format!("{number}#1234#"));
        caller.digits("*3#");
        caller.audio(25, false);
        blocked[index] = count(&fixture::PHONE_MAILBOX_VERIFIED) == verified
            && count(&fixture::PHONE_MAILBOX_REJECTED) > rejected
            && station() == snapshot;
        caller.hangup();
        wait_until(|| count(&output.value) == 0);
    }

    let before = station();
    let verified = count(&fixture::PHONE_MAILBOX_VERIFIED);
    let mut permitted = Caller::connect(socket, contact, 3, true);
    permitted.audible_greeting();
    permitted.digits("0741#1234#");
    permitted.digits("*99#");
    permitted.audio(25, false);
    let admitted = count(&fixture::PHONE_MAILBOX_VERIFIED) == verified + 1
        && station()
            .iter()
            .zip(before)
            .all(|(&after, old)| after > old);
    let timeout = socket.read_timeout().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let revoked = thread::scope(|scope| {
        let receive = scope.spawn(|| permitted.read_revocation());
        // No mailbox, credential or provider binding changes in this edit.
        edit(
            client,
            "PATCH",
            account,
            json!([
                {"op":"add","path":"/configuration/deny_mailboxes","value":["741","0741"]}
            ]),
        );
        receive.join().unwrap()
    });
    socket.set_read_timeout(timeout).unwrap();
    let revoked = match revoked {
        Ok(()) => true,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            // The baseline keeps the call alive. Close it normally before reporting the verdict.
            permitted.hangup();
            false
        }
        Err(error) => panic!("reading the exact phone revocation: {error}"),
    };
    wait_until(|| count(&output.value) == 0);
    (blocked[0], blocked[1], admitted, revoked)
}

struct AutomaticAttachment {
    private: bool,
    authenticated: bool,
    attached: bool,
    starts_muted: bool,
    muted_quiet: bool,
    caller_unmuted: bool,
    fresh_pcm_and_keying: bool,
    caller_remuted: bool,
}

/// Prove login, attachment and mute with real caller digits and the station's
/// carrier held false. Keep the missing-attachment baseline able to close.
fn automatic_attachment(
    client: &Client<'_>,
    port: &str,
    socket: &UdpSocket,
    contact: SocketAddr,
    output: &fixture::ReviewOutput,
) -> AutomaticAttachment {
    fn call(document: &serde_json::Value) -> &serde_json::Value {
        &document["_embedded"]["item"][0]
    }

    let calls = format!(
        "{}calls/?limit=1",
        port.replacen("/configuration/", "/status/", 1)
    );
    let count = |counter: &std::sync::atomic::AtomicUsize| counter.load(Ordering::Acquire);
    let station = || (count(&output.keyed), count(&output.audible));
    let quiet = station();
    let installs = count(&fixture::PHONE_ROUTE_INSTALLS);
    let verified = count(&fixture::PHONE_MAILBOX_VERIFIED);
    let rejected = count(&fixture::PHONE_MAILBOX_REJECTED);

    let mut caller = Caller::connect(socket, contact, 1, true);
    let greeting = caller.receive_private_audio() > 0;
    let private_phase = live_status::wait_for(client, &calls, |document| {
        let call = call(document);
        call["direction"] == "incoming" && call["phase"] == "private" && call["attached"] == false
    });
    let identity = call(&client.get(&calls).body)["call_id"].clone();
    let current = |document: &serde_json::Value| {
        identity.is_string() && call(document)["call_id"] == identity
    };
    caller.digits("741#0000#");
    caller.audio(8, false);
    // Give the native digit and jitter owners time to report the exact attempt,
    // while the caller keeps its regular media clock and remains private.
    let rejection_observed = caller.during(true, || {
        live_status::wait_for(client, &calls, |document| {
            current(document)
                && call(document)["phase"] == "private"
                && call(document)["attached"] == false
                && count(&fixture::PHONE_MAILBOX_REJECTED) > rejected
        })
    });
    let quiet_output = station() == quiet;
    let no_route = count(&fixture::PHONE_ROUTE_INSTALLS) == installs;
    let no_grant = count(&fixture::PHONE_MAILBOX_VERIFIED) == verified;
    eprintln!(
        "PHONE_AUTO_ATTACH_PRIVACY greeting={greeting} private_phase={private_phase} rejection_observed={rejection_observed} quiet_output={quiet_output} no_route={no_route} no_grant={no_grant}"
    );
    let private =
        greeting && private_phase && rejection_observed && quiet_output && no_route && no_grant;

    caller.digits("741#1234#");
    let (authenticated, attached) = caller.during(true, || {
        let authenticated = live_status::wait_for(client, &calls, |document| {
            current(document)
                && matches!(
                    call(document)["phase"].as_str(),
                    Some("authenticated" | "attached")
                )
                && count(&fixture::PHONE_MAILBOX_VERIFIED) == verified + 1
        });
        let attached = live_status::wait_for(client, &calls, |document| {
            current(document)
                && call(document)["phase"] == "attached"
                && call(document)["attached"] == true
                && count(&fixture::PHONE_ROUTE_INSTALLS) > installs
        });
        (authenticated, attached)
    });
    let muted = |expected: bool| {
        live_status::wait_for(client, &calls, |document| {
            current(document)
                && call(document)["attached"] == true
                && call(document)["muted"] == expected
                && (!expected || call(document)["active"] == false)
        })
    };
    let mut result = AutomaticAttachment {
        private,
        authenticated,
        attached,
        starts_muted: false,
        muted_quiet: false,
        caller_unmuted: false,
        fresh_pcm_and_keying: false,
        caller_remuted: false,
    };
    if attached {
        // A status poll does not stop an active SIP sender's RTP clock.
        result.starts_muted = caller.during(true, || muted(true));
        caller.audio(18, false);
        result.muted_quiet = station() == quiet;
        let media = || {
            [
                &fixture::PHONE_EVENT_DIGITS,
                &fixture::PHONE_COMMAND_DIGITS,
                &fixture::PHONE_ORIGINAL,
                &fixture::PHONE_FRESH,
                &fixture::PHONE_PERMITTED,
                &fixture::PHONE_ACCEPTED,
                &output.keyed,
                &output.audible,
            ]
            .map(count)
        };
        let before = media();
        caller.digits("*99#");
        result.caller_unmuted = caller.during(true, || muted(false));
        let unmuted = media();
        caller.audio(18, false);
        let programme = media();
        result.fresh_pcm_and_keying =
            programme[5] > before[5] && programme[6] > before[6] && programme[7] > before[7];
        caller.digits("*99#");
        result.caller_remuted = caller.during(true, || muted(true));
        let remuted = media();
        eprintln!(
            "PHONE_AUTO_ATTACH_MEDIA event/command/original/fresh/permitted/accepted/keyed/audible {before:?} -> {unmuted:?} -> {programme:?} -> {remuted:?}"
        );
    }
    caller.hangup();
    result
}

pub(in super::super) fn positive_phone(scenario: &Scenario<'_>) {
    let _serial = fixture::LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    fixture::REVIEW_CARRIER.store(false, Ordering::Release);
    let (document, address) = provisioning(scenario);
    let mut managed = Running::start(&serde_json::to_vec(&document).unwrap());
    let client = Client {
        pki: scenario.client.pki,
        port: address.port(),
        proxy: None,
    };
    client.ready();
    let path = edit(
        &client,
        "POST",
        PORTS,
        json!({"key":"call","configuration":{"name":"Call","inherit_from":"site","enabled":false}}),
    )
    .headers["location"]
        .clone();
    let port = path.trim_end_matches('/').rsplit('/').next().unwrap();
    let id = port.parse().unwrap();
    let station = &managed.owners.as_ref().unwrap().station;
    *station.port.lock().unwrap() = Some(id);
    station.allow_open.store(true, Ordering::Release);
    let output = fixture::watch_output(port);
    output
        .measure_without_carrier
        .store(true, Ordering::Release);
    fixture::WATCH_PHONE_PCM.store(true, Ordering::Release);
    put(
        &client,
        &format!("{path}interface/"),
        json!({"audio":{"device_identifier":"hw:fixture"}}),
    );
    put(
        &client,
        &format!("{path}radio/"),
        json!({"duplex_mode":"full"}),
    );
    let pin = "$argon2id$v=19$m=19456,t=2,p=1$c2l4dGVlbi1ieXRlLXNhbHQ$JJvcFomwgumB44bmQdOCEbN00saqrIis8c6S0WG6Kzw";
    let mailbox = edit(
        &client,
        "POST",
        &format!("{path}mailboxes/"),
        json!({"key":"741","configuration":{
        "first_name":"Test","callsign":"N0TEST","pin_hash":pin}}),
    )
    .headers["location"]
        .clone();
    assert!(!client.get(&mailbox).body.to_string().contains(pin));
    let access = std::env::var(CASE).as_deref() == Ok("native-private-mailbox-access");
    if access {
        for number in ["0741", "7410"] {
            edit(
                &client,
                "POST",
                &format!("{path}mailboxes/"),
                json!({
                    "key": number,
                    "configuration": {"first_name":"Test","callsign":"N0TEST","pin_hash":pin}
                }),
            );
        }
    }
    if std::env::var(CASE).as_deref() == Ok("native-private-managed-iax") {
        super::phone_iax::positive_iax(
            &client,
            &path,
            &mailbox,
            managed.owners.as_ref().unwrap(),
            &output,
        );
        assert!(managed.stop());
        return;
    }
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(12)))
        .unwrap();
    let mut settings = json!({
        "provider":{"protocol":"sip","server":"127.0.0.1","port":socket.local_addr().unwrap().port(),"username":"desk"},
        "credentials":{"password":"fixture-only"},"vad":{"enabled":false}
    });
    if access {
        settings["allow_mailboxes"] = json!(["741", "0741"]);
        settings["deny_mailboxes"] = json!(["741"]);
    }
    let account = edit(
        &client,
        "POST",
        &format!("{path}protocols/phone-patch/"),
        json!({"configuration":settings}),
    )
    .headers["location"]
        .clone();
    let contact = thread::scope(|scope| {
        let network = scope.spawn(|| {
            let (validated, remote) = register(&socket);
            socket
                .send_to(
                    registrar::response(&validated, 200, false).as_bytes(),
                    remote,
                )
                .unwrap();
            remote
        });
        put(
            &client,
            &path,
            json!({"name":"Call","inherit_from":"site","enabled":true}),
        );
        network.join().unwrap()
    });
    wait_until(|| output.value.load(Ordering::Acquire) == 0);
    let mut auto_attach = None;
    let receipt = if std::env::var(CASE).as_deref() == Ok("native-private-auto-attach") {
        auto_attach = Some(automatic_attachment(
            &client, &path, &socket, contact, &output,
        ));
        None
    } else if access {
        Some(mailbox_access(&client, &account, &socket, contact, &output))
    } else {
        let keys = output.keyed.load(Ordering::Acquire);
        let audible = output.audible.load(Ordering::Acquire);
        let inband = std::env::var(CASE).as_deref() == Ok("native-private-inband");
        let mut caller = Caller::connect(&socket, contact, 1, !inband);
        assert!(caller.receive_private_audio() > 0);
        caller.digits("741#0000#");
        caller.audio(8, false);
        assert_eq!(
            output.keyed.load(Ordering::Acquire),
            keys,
            "caller keyed before mailbox login"
        );
        assert_eq!(
            output.audible.load(Ordering::Acquire),
            audible,
            "private prompt escaped to RF"
        );
        caller.digits("741#1234#");
        caller.digits("*99#");
        caller.audio(18, false);
        assert!(
            output.keyed.load(Ordering::Acquire) > keys,
            "authorized attachment never keyed callback"
        );
        let counters = [
            &fixture::PHONE_ORIGINAL,
            &fixture::PHONE_FRESH,
            &fixture::PHONE_PERMITTED,
            &fixture::PHONE_ACCEPTED,
        ]
        .map(|count| count.load(Ordering::Acquire));
        eprintln!(
            "native PCMU nonzero samples original/fresh/permitted/accepted {counters:?}, callback keys {} audible {}",
            output.keyed.load(Ordering::Acquire),
            output.audible.load(Ordering::Acquire)
        );
        assert!(
            counters[3] > 0,
            "no authentic nonzero caller PCM was credited to the private RF route: {counters:?}"
        );
        assert!(
            output.audible.load(Ordering::Acquire) > audible,
            "actual PCMU never reached station callback"
        );
        caller.digits("*99#");
        wait_until(|| output.value.load(Ordering::Acquire) == 0);
        let muted = output.keyed.load(Ordering::Acquire);
        let quiet = output.audible.load(Ordering::Acquire);
        // The old bare attach command is an idempotent same-port operation.
        caller.digits("*3#");
        caller.audio(18, false);
        assert_eq!(output.keyed.load(Ordering::Acquire), muted);
        assert_eq!(output.audible.load(Ordering::Acquire), quiet);
        caller.digits("*99#");
        caller.audio(18, false);
        assert!(
            output.keyed.load(Ordering::Acquire) > muted,
            "muted caller lost its digit path"
        );
        assert!(output.audible.load(Ordering::Acquire) > quiet);
        let revoked = if inband {
            eprintln!(
                "PCMU-only SIP: scoped mailbox login, attach, voice, mute and unmute reached the station callback"
            );
            caller
        } else {
            let owner = managed.owners.as_ref().unwrap();
            let identity = owner.read_identity();
            assert_eq!(identity.source, socket.local_addr().unwrap());
            assert_ne!(identity.clock_epoch, 0);
            let media = || {
                [
                    owner.read_count.load(Ordering::Acquire),
                    fixture::PHONE_ORIGINAL.load(Ordering::Acquire),
                    fixture::PHONE_FRESH.load(Ordering::Acquire),
                    fixture::PHONE_PERMITTED.load(Ordering::Acquire),
                    fixture::PHONE_ACCEPTED.load(Ordering::Acquire),
                    output.keyed.load(Ordering::Acquire),
                    output.dekeyed.load(Ordering::Acquire),
                    output.audible.load(Ordering::Acquire),
                    fixture::PHONE_LIVE_SAMPLES.load(Ordering::Acquire),
                    fixture::PHONE_GAP_CREDIT.load(Ordering::Acquire),
                    fixture::PHONE_ROUTE_INSTALLS.load(Ordering::Acquire),
                ]
            };
            if std::env::var(CASE).as_deref() == Ok("native-private-retained-vad") {
                caller.during(false, || {
                    edit(
                        &client,
                        "PATCH",
                        &account,
                        json!([{"op":"replace","path":"/configuration/vad/enabled","value":true}]),
                    )
                });
                assert_eq!(owner.read_identity(), identity);
                // Real packets from before the edit may still emerge. Drive genuine silent
                // RTP past the last nonzero sample and the documented 1000 ms speech hang,
                // 250 ms playout target and 500 ms maximum age of this profile.
                let mut last_nonzero = fixture::PHONE_ORIGINAL.load(Ordering::Acquire);
                let mut silent_packets = 0;
                let tail = (1000_usize + 250 + 500).div_ceil(20);
                for _ in 0..2 * tail {
                    caller.audio(1, true);
                    let current = fixture::PHONE_ORIGINAL.load(Ordering::Acquire);
                    if current == last_nonzero {
                        silent_packets += 1;
                    } else {
                        silent_packets = 0;
                        last_nonzero = current;
                    }
                    if silent_packets >= tail {
                        break;
                    }
                }
                assert_eq!(
                    silent_packets, tail,
                    "the observed SIP source never finished delivering its earlier real speech"
                );
                let quiet_keys = output.keyed.load(Ordering::Acquire);
                let prior = media();
                caller.audio(24, true);
                let after = media();
                eprintln!(
                    "retained silent RTP read/original/fresh/permitted/credited/key/dekey/audible/live/gap/route {prior:?} -> {after:?}"
                );
                assert!(
                    after[0] > prior[0] && after[8] > prior[8],
                    "fresh genuine silent RTP lost across policy edit"
                );
                assert_eq!(
                    after[1], prior[1],
                    "prior programme contaminated the silent observation"
                );
                assert_eq!(owner.read_identity(), identity);
                assert_eq!(
                    output.keyed.load(Ordering::Acquire),
                    quiet_keys,
                    "retained dialog must adopt newly enabled speech qualification"
                );
                assert_eq!(output.value.load(Ordering::Acquire), 0);
                caller.spoken_yes();
                caller.audio(12, true);
                assert!(
                    output.keyed.load(Ordering::Acquire) > quiet_keys,
                    "fresh spoken PCMU did not qualify the same VAD-enabled native call"
                );
                let voiced_keys = output.keyed.load(Ordering::Acquire);
                caller.audio(tail, true);
                assert_eq!(
                    output.value.load(Ordering::Acquire),
                    0,
                    "genuine speech hang must expire on continued silence"
                );
                let expired_voice = output.keyed.load(Ordering::Acquire);
                caller.audio(24, true);
                assert_eq!(
                    output.keyed.load(Ordering::Acquire),
                    expired_voice,
                    "an older positive classifier position renewed the expired voice hang"
                );
                eprintln!(
                    "retained VAD real speech keyed {voiced_keys}, expired on fresh silent RTP at {expired_voice}"
                );
                caller.during(true, || {
                    edit(
                        &client,
                        "PATCH",
                        &account,
                        json!([{"op":"replace","path":"/configuration/vad/enabled","value":false}]),
                    )
                });
                caller.audio(24, true);
                assert!(
                    output.keyed.load(Ordering::Acquire) > expired_voice,
                    "disabling speech qualification must restore live silent-speech admission on the same dialog"
                );
                assert_eq!(owner.read_identity(), identity);
                caller.during(false, || edit(&client,"PATCH",&account,json!([
            {"op":"add","path":"/configuration/timing","value":{"target_latency_ms":1,"maximum_latency_ms":1}},
            {"op":"add","path":"/configuration/isolate_peers","value":false}
        ])));
                wait_until(|| output.value.load(Ordering::Acquire) == 0);
                caller.resume_audio_clock();
                let expired = output.keyed.load(Ordering::Acquire);
                caller.audio(24, false);
                assert_eq!(
                    output.keyed.load(Ordering::Acquire),
                    expired,
                    "actual jitter-held RTP older than one millisecond must not open the retained route"
                );
                assert_eq!(owner.read_identity(), identity);
                caller.during(false, || edit(&client,"PATCH",&account,json!([
            {"op":"replace","path":"/configuration/timing","value":{"target_latency_ms":250,"maximum_latency_ms":500}},
            {"op":"replace","path":"/configuration/isolate_peers","value":true}
        ])));
                caller.audio(24, false);
                assert!(output.keyed.load(Ordering::Acquire) > expired);
                assert_eq!(owner.read_identity(), identity);
                eprintln!(
                    "retained native media owner {identity:?}, SIP contact {contact}, qualified VAD and 1/500ms cutoffs"
                );
            }
            let before_edit = media();
            caller.during(false, || {
                edit(
                    &client,
                    "POST",
                    PORTS,
                    json!({"key":"other","configuration":{
        "name":"Other","inherit_from":"site","enabled":false}}),
                )
            });
            let after_edit = media();
            caller.audio(18, false);
            let resumed = media();
            eprintln!(
                "station edit read/original/fresh/permitted/credited/key/dekey/audible/live/gap/route {before_edit:?} -> {after_edit:?} -> {resumed:?}; source {:?}",
                owner.read_identity()
            );
            assert!(
                resumed[0] > after_edit[0],
                "the established native owner stopped reading after unrelated adoption"
            );
            assert_eq!(owner.read_identity(), identity);
            assert!(
                resumed[7] > after_edit[7],
                "unrelated adoption lost the established call"
            );
            caller.hangup();
            wait_until(|| output.value.load(Ordering::Acquire) == 0);
            eprintln!("starting negotiated PCMU-only managed dialog");
            let mut second = Caller::connect(&socket, contact, 2, false);
            second.digits("741#1234#");
            second.digits("*99#");
            let second_before = output.audible.load(Ordering::Acquire);
            second.audio(18, false);
            assert!(output.audible.load(Ordering::Acquire) > second_before);
            second
        };
        thread::scope(|scope| {
            let withdrawn_call = scope.spawn(move || revoked.expect_revocation());
            assert_eq!(client.edit("DELETE", &mailbox, None).code, 202);
            client.ready();
            withdrawn_call.join().unwrap();
        });
        None
    };
    wait_until(|| output.value.load(Ordering::Acquire) == 0);
    thread::scope(|scope| {
        let withdrawn = scope.spawn(|| {
            let mut bytes = [0; 8192];
            let (message, remote) = registrar::receive_register(&socket, &mut bytes).unwrap();
            assert_eq!(remote, contact);
            assert!(message.starts_with("REGISTER "));
            assert_eq!(registrar::header(&message, "Expires"), "0");
            socket
                .send_to(registrar::response(&message, 200, false).as_bytes(), remote)
                .unwrap();
        });
        put(
            &client,
            &path,
            json!({"name":"Call","inherit_from":"site","enabled":false}),
        );
        withdrawn.join().unwrap();
    });
    assert!(managed.stop());
    if let Some(result) = auto_attach {
        assert!(
            WorkingFiles::open(&scenario.fixture.0)
                .unwrap()
                .try_writer_lease()
                .is_ok()
        );
        let AutomaticAttachment {
            private,
            authenticated,
            attached,
            starts_muted,
            muted_quiet,
            caller_unmuted,
            fresh_pcm_and_keying,
            caller_remuted,
        } = result;
        eprintln!(
            "PHONE_AUTO_ATTACH_CLOSED private={private} authenticated={authenticated} attached={attached} starts_muted={starts_muted} muted_quiet={muted_quiet} caller_unmuted={caller_unmuted} fresh_pcm_and_keying={fresh_pcm_and_keying} caller_remuted={caller_remuted} bye_200=true withdrawn=true joined=true writer_released=true"
        );
        assert!(
            private && authenticated,
            "reverse caller must authenticate privately"
        );
        assert!(
            attached,
            "successful reverse login must attach without an explicit attach command"
        );
        assert!(
            starts_muted && muted_quiet,
            "attached reverse caller must start inaudible and unkeyed"
        );
        assert!(
            caller_unmuted && fresh_pcm_and_keying && caller_remuted,
            "the exact caller must control its mute and pass fresh PCM and keying after unmute"
        );
    } else if let Some((denied, unlisted, exact_allowed, acl_only_bye)) = receipt {
        assert!(
            WorkingFiles::open(&scenario.fixture.0)
                .unwrap()
                .try_writer_lease()
                .is_ok()
        );
        eprintln!(
            "PHONE_MAILBOX_ACCESS_CLOSED denied={denied} unlisted={unlisted} exact_allowed={exact_allowed} acl_only_bye={acl_only_bye} byes_200=true withdrawn=true joined=true writer_released=true"
        );
        assert!(
            denied && unlisted && exact_allowed && acl_only_bye,
            "mailbox blocking must reject denied and unlisted IDs and revoke the newly denied live caller"
        );
    } else {
        eprintln!(
            "two real dialogs on {contact}, private prompt delivered, station callback keys {} audible {}, grant revoked and native account withdrawn",
            output.keyed.load(Ordering::Acquire),
            output.audible.load(Ordering::Acquire)
        );
    }
}
