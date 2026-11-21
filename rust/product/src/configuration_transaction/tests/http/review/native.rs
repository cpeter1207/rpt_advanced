//! Authenticated desired state is adopted by the actual checked product executor.
use super::*;
use crate::{fixture, lifecycle::Engine, management::native::NativeRuntime};

struct ConfirmationProbe {
    engine: std::sync::Weak<Engine>,
    revision: String,
    port: String,
    original: u64,
    reached: std::sync::mpsc::SyncSender<u64>,
    proceed: std::sync::mpsc::Receiver<bool>,
}
static CONFIRMATION: Mutex<Option<ConfirmationProbe>> = Mutex::new(None);

unsafe extern "C" fn submit_recovery(
    context: *mut std::ffi::c_void,
    task: crate::abi::rptadv_control_task_v1,
) -> i32 {
    // Other submitters, including the ticker, continue through the real executor.
    if thread::current().name() == Some("rpt-config-adoption") {
        let mut slot = CONFIRMATION.lock().unwrap();
        if let Some(probe) = slot.as_ref()
            && let Some(observed) = probe.engine.upgrade().and_then(|engine| {
                engine.fixture_published_generation(&probe.revision, &probe.port)
            })
            && observed != probe.original
        {
            let probe = slot.take().unwrap();
            drop(slot);
            if probe.reached.send(observed).is_err()
                || probe.proceed.recv_timeout(Duration::from_secs(4)) != Ok(true)
            {
                return -1;
            }
        }
    }
    // SAFETY: forward the original task and context once. Rejection above leaves ownership
    // with the caller, under the same contract as the real executor.
    unsafe { (*fixture::control_descriptor()).submit.unwrap()(context, task) }
}

/// Invoked by the real store/PKI harness in its isolated service-credential process.
pub(super) fn run(scenario: &mut Scenario<'_>, case: &str) {
    let _serial = fixture::LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
    let confirm = matches!(case, "native-confirm" | "native-confirm-failure");
    let two_live = confirm || case == "native-two-live";
    // SAFETY: the complete descriptor copy stays alive through the confirmed stop below.
    let mut control = unsafe { *fixture::control_descriptor() };
    if confirm {
        control.submit = Some(submit_recovery);
    }
    let counters = Counters::default();
    let binding = ops();
    // SAFETY: the lifecycle fixture keeps the complete initialized template alive.
    let mut host = unsafe { *fixture::host_descriptor() };
    host.context = std::ptr::from_ref(&counters).cast_mut().cast();
    host.station_binding = &binding;
    host.radio_quiesce = Some(quiesce);
    if case == "native-stop" {
        crate::lifecycle::install_delayed_site_bank(&mut host);
    }
    let path = scenario.fixture.0.clone();
    let inspection_document = (case == "native-inspection-registered")
        .then(|| serde_json::to_vec(&provisioning(scenario).0).unwrap());
    // The checked station group includes the existing commit/rollback callback.
    // This fixture refuses physical open, so it can never own an update to finish.
    unsafe extern "C" fn no_radio_update(
        _: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
        _: u32,
    ) -> i32 {
        -1
    }
    host.radio_update = Some(no_radio_update);
    // SAFETY: all fixture descriptors and callback state outlive the confirmed stop below.
    let engine = unsafe {
        Engine::start_management(
            &store,
            &host,
            &control,
            rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
            rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
        )
    }
    .unwrap();
    if two_live || case == "native-reopen" {
        engine.start_fixture_ticker();
    }
    let runtime = Arc::new(NativeRuntime::new(Arc::clone(&engine)));
    let coordinator = Coordinator::with_profile_inputs(
        store,
        KeyId::new("example").unwrap(),
        LIMITS,
        "/api/v1",
        runtime.clone(),
        Arc::new(inputs()),
    )
    .unwrap();
    let client = scenario.client;
    let stopping = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            while !stopping.load(Ordering::Acquire) {
                if !coordinator.poll_listener(scenario.listener, scenario.inbox) {
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        let _stop = Stop(&stopping);
        let opens = fixture::RADIO_OPENS.load(Ordering::Acquire);
        let health = client.get("/api/v1/health/").body;
        assert_eq!(health["status"]["desired_revision"], "first");
        assert!(health["status"]["active_revision"].is_null());
        assert!(runtime.observed().active_revision.is_none());

        let submitted = operation(
            client,
            json!({"submitted":true,
            "action":{"action":"apply_configuration"},"desired_etag":client.etag(DESIRED)}),
        );
        assert!((200..300).contains(&submitted.code), "{}", submitted.body);
        client.ready();
        let site_name = || {
            engine
                .run(|host| {
                    let configuration = host
                        .runtime
                        .modular_configuration()
                        .ok_or(rpt_advanced_core::runtime::RuntimeError::MissingNode)?;
                    let site = ResourceScope::Site(configuration.registry.site_id());
                    assert!(host.runtime.node_names().is_empty());
                    Ok(configuration.scopes[&site].metadata.name.clone())
                })
                .unwrap()
        };
        assert_eq!(site_name(), "first");
        put(
            client,
            SITE_PATH,
            json!({"name":"Second real owner","inherit_from":null}),
        );
        assert_eq!(site_name(), "Second real owner");
        assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens);

        let resource = port(client);
        let port_id: rpt_advanced_core::config::resources::candidate::ResourceId = resource
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        if matches!(
            case,
            "native-stop"
                | "native-reopen"
                | "native-two-live"
                | "native-confirm"
                | "native-confirm-failure"
                | "native-local"
                | "native-bookkeeping"
                | "native-repair"
                | "native-new-fault"
        ) {
            put(
                client,
                &format!("{resource}radio/"),
                json!({"duplex_mode":"full"}),
            );
        }
        let output_a = fixture::watch_output(&port_id.to_string());
        let other = if two_live || case == "native-local" {
            let other=edit(client,"POST",PORTS,json!({"key":"second","configuration":{"name":"B","inherit_from":"site","enabled":false}})).headers["location"].clone();
            put(
                client,
                &format!("{other}interface/"),
                json!({"audio":{"device_identifier":"hw:other"}}),
            );
            if two_live {
                put(
                    client,
                    &format!("{other}radio/"),
                    json!({"duplex_mode":"full"}),
                );
            }
            Some(other)
        } else {
            None
        };
        *counters.port.lock().unwrap() = Some(port_id);
        let enable = json!({"configuration":{"name":"Radio","inherit_from":"site","enabled":true}});
        let disabled = runtime.observed().active_revision.unwrap();
        let old_tag = client.etag(DESIRED);
        // Exact selection with neither a device identifier nor a USB serial is
        // an invalid stopped provider request, before selector publication.
        let invalid = client.edit("PUT", &resource, Some(&enable));
        assert_eq!(invalid.code, 503, "{}", invalid.body);
        assert_eq!(client.etag(DESIRED), old_tag);
        assert_eq!(
            runtime.observed().active_revision.as_deref(),
            Some(disabled.as_str())
        );
        assert_eq!(counters.prepared.load(Ordering::Acquire), 0);
        put(
            client,
            &format!("{resource}interface/"),
            if case == "native-stop" {
                json!({"audio":{"device_identifier":"hw:fixture"},"gpio":{"pins":{"7":"out0"}}})
            } else {
                json!({"audio":{"device_identifier":"hw:fixture"}})
            },
        );
        let old = runtime.observed().active_revision.unwrap();
        let before_plans = counters.prepared.load(Ordering::Acquire);
        let attempt = client.edit("PUT", &resource, Some(&enable));
        assert!((200..300).contains(&attempt.code), "{}", attempt.body);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let health = client.get("/api/v1/health/").body;
            let state = &health["status"];
            if state["historical_activation"] == "failed" {
                assert_eq!(state["active_revision"], old);
                assert_eq!(state["last_known_good"], old);
                assert_ne!(state["desired_revision"], old);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "radio failure was never confirmed"
            );
            thread::sleep(Duration::from_millis(4));
        }
        assert_eq!(counters.prepared.load(Ordering::Acquire), before_plans + 1);
        assert_eq!(counters.refused.load(Ordering::Acquire), 1);
        assert_eq!(counters.released.load(Ordering::Acquire), before_plans + 1);
        assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens);
        assert_eq!(
            runtime.observed().active_revision.as_deref(),
            Some(old.as_str())
        );

        // Reapply the same selected plan after the test physical owner becomes available.
        // No working edit, implicit creation or special runtime activation entry.
        counters.allow_open.store(true, Ordering::Release);
        if matches!(
            case,
            "native-bookkeeping" | "native-repair" | "native-new-fault"
        ) {
            *counters.block_bookkeeping.lock().unwrap() = Some(path.clone());
        }
        let reply = operation(
            client,
            json!({"submitted":true,"action":{"action":"apply_configuration"},
            "desired_etag":client.etag(DESIRED)}),
        );
        if matches!(
            case,
            "native-bookkeeping" | "native-repair" | "native-new-fault"
        ) {
            // Make only the selector replacement unwritable in open_prepared, after private
            // preparation. Restore write permission after the synchronous apply has finished.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            let health = client.get("/api/v1/health/").body;
            assert_eq!(
                client.get(STORAGE).body["storage"],
                "storage_fault",
                "{health}"
            );
            if case == "native-bookkeeping" {
                denied_save(client, &path, "storage_fault");
                assert_eq!(
                    health["status"]["active_revision"],
                    health["status"]["desired_revision"]
                );
                assert_ne!(
                    health["status"]["last_known_good"],
                    health["status"]["active_revision"]
                );
                assert!(runtime.observed().owners.contains_key(&port_id));
                fixture::REVIEW_CARRIER.store(true, Ordering::Release);
                wait_until(|| fixture::REVIEW_CALLBACKS.load(Ordering::Acquire) > 8);
                assert_eq!(fixture::REVIEW_OUTPUT.load(Ordering::Acquire), 0);
                assert!(
                    !runtime.permits_save(health["status"]["desired_revision"].as_str().unwrap())
                );
            } else {
                if case == "native-new-fault" {
                    *runtime.final_inspection_fixture.lock().unwrap() =
                        Some(|| fixture_key_available(false));
                }
                let repair = operation(
                    client,
                    json!({"submitted":true,"action":{"action":"apply_configuration"},"desired_etag":client.etag(DESIRED)}),
                );
                if case == "native-new-fault" {
                    assert!(
                        matches!(
                            repair.body["receipt"]["state"].as_str(),
                            Some("rejected" | "indeterminate")
                        ) || repair.code >= 400,
                        "{}",
                        repair.body
                    );
                    let health = client.get("/api/v1/health/").body;
                    assert_eq!(health["status"]["secrets_state"], "locked", "{health}");
                    assert!(
                        !runtime
                            .permits_save(health["status"]["desired_revision"].as_str().unwrap())
                    );
                    fixture_key_available(true);
                } else {
                    assert_eq!(
                        repair.body["receipt"]["state"], "completed",
                        "{}",
                        repair.body
                    );
                    client.ready();
                    assert!(
                        runtime
                            .permits_save(runtime.observed().active_revision.as_deref().unwrap())
                    );
                }
            }
            return;
        }
        assert!((200..300).contains(&reply.code), "{}", reply.body);
        client.ready();
        assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens + 1);
        let generation = runtime
            .observed()
            .owners
            .get(&port_id)
            .copied()
            .expect("actual registered command owner");
        assert_ne!(generation, 0);
        assert_eq!(
            engine
                .run(move |host| {
                    let node = host
                        .runtime
                        .node(&port_id.to_string())
                        .ok_or(rpt_advanced_core::runtime::RuntimeError::MissingNode)?;
                    Ok(node.command_work().unwrap().id())
                })
                .unwrap(),
            generation
        );
        if case == "native-reopen" {
            fixture::REVIEW_CARRIER.store(true, Ordering::Release);
            wait_until(|| output_a.value.load(Ordering::Acquire) == 3);
            let opened = fixture::RADIO_OPENS.load(Ordering::Acquire);
            let drops = fixture::RADIO_DROPS.load(Ordering::Acquire);
            counters.stage.store(1, Ordering::Release);
            let target = format!("{resource}interface/");
            let reply = client.edit(
                "PUT",
                &target,
                Some(&json!({"configuration":{"audio":{"device_identifier":"hw:reopen"}}})),
            );
            assert_eq!(reply.code, 202, "{}", reply.body);
            wait_until(|| {
                client.get("/api/v1/health/").body["status"]["historical_activation"]
                    == "recovery_required"
            });
            assert_eq!(counters.stages.load(Ordering::Acquire), 1);
            assert_eq!(fixture::RADIO_DROPS.load(Ordering::Acquire), drops + 1);
            wait_for_physical_state(&engine, &port_id.to_string(), true);
            output_a.value.store(usize::MAX, Ordering::Release);
            let fixed = operation(
                client,
                json!({"submitted":true,"action":{"action":"apply_configuration"},"desired_etag":client.etag(DESIRED)}),
            );
            assert_eq!(
                fixed.body["receipt"]["state"], "completed",
                "{}",
                fixed.body
            );
            client.ready();
            assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opened + 1);
            assert_ne!(runtime.observed().owners[&port_id], generation);
            assert!(
                engine
                    .run(move |host| Ok(host
                        .runtime
                        .node(&port_id.to_string())
                        .unwrap()
                        .register_audio()
                        .is_none()))
                    .unwrap(),
                "the fresh node must already have registered its unique callback pair"
            );
            wait_until(|| output_a.value.load(Ordering::Acquire) == 3);
            return;
        }
        if two_live {
            let other = other.as_ref().unwrap();
            let id: rpt_advanced_core::config::resources::candidate::ResourceId = other
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            *counters.other.lock().unwrap() = Some(id);
            let output_b = fixture::watch_output(&id.to_string());
            edit(client, "PUT", other, enable.clone());
            fixture::REVIEW_CARRIER.store(true, Ordering::Release);
            let sample = |phase, a, b| {
                output_a.value.store(usize::MAX, Ordering::Release);
                output_b.value.store(usize::MAX, Ordering::Release);
                let until = Instant::now() + Duration::from_secs(4);
                loop {
                    let actual = (
                        output_a.value.load(Ordering::Acquire),
                        output_b.value.load(Ordering::Acquire),
                    );
                    if actual == (a, b) {
                        break;
                    }
                    assert!(
                        Instant::now() < until,
                        "{phase}: expected PTT/audio {:?}, observed {actual:?}",
                        (a, b)
                    );
                    thread::sleep(Duration::from_millis(4));
                }
            };
            sample("initial two transmitting incumbents", 3, 3);
            wait_for_physical_state(&engine, &port_id.to_string(), false);
            wait_for_physical_state(&engine, &id.to_string(), false);
            let health = client.get("/api/v1/health/").body;
            assert!(
                runtime.permits_save(health["status"]["desired_revision"].as_str().unwrap()),
                "{health}"
            );
            let a_dekeys = output_a.dekeyed.load(Ordering::Acquire);
            let original = runtime.observed();
            eprintln!(
                "two registered incumbent command generations A={} B={}",
                original.owners[&port_id], original.owners[&id]
            );
            counters.stage.store(2, Ordering::Release);
            fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
            let reply = client.edit(
                "PUT",
                &format!("{other}interface/"),
                Some(&json!({"configuration":{"audio":{"device_identifier":"hw:changed-b"}}})),
            );
            assert_eq!(reply.code, 202, "{}", reply.body);
            wait_until(|| {
                client.get("/api/v1/health/").body["status"]["historical_activation"] == "failed"
            });
            assert_eq!(runtime.observed().active_revision, original.active_revision);
            assert_eq!(runtime.observed().owners, original.owners);
            assert_eq!(counters.stages.load(Ordering::Acquire), 1);
            eprintln!(
                "confirmed rollback preserved A/B command generations {:?}; both physical handles are sampled independently",
                runtime.observed().owners
            );
            sample("confirmed rollback with restored B", 3, 0);
            let assert_b_held = || {
                let keys = output_b.keyed.load(Ordering::Acquire);
                let frames = output_b.dekeyed.load(Ordering::Acquire);
                wait_until(|| output_b.dekeyed.load(Ordering::Acquire) >= frames + 8);
                assert_eq!(output_b.keyed.load(Ordering::Acquire), keys);
                assert_eq!(output_a.dekeyed.load(Ordering::Acquire), a_dekeys);
            };
            assert_b_held();
            let manual = |value| {
                engine
                    .run(move |host| {
                        let node = host.runtime.node(&id.to_string()).unwrap();
                        node.site_command(rpt_advanced_core::site::SiteCommand::Inhibit(value), 0)
                            .unwrap();
                        Ok(())
                    })
                    .unwrap()
            };
            manual(true);
            manual(false);
            assert_b_held();
            if confirm {
                let success = case == "native-confirm";
                let health = client.get("/api/v1/health/").body;
                let desired = health["status"]["desired_revision"].as_str().unwrap();
                let (reached, observer) = std::sync::mpsc::sync_channel(1);
                let (release, proceed) = std::sync::mpsc::sync_channel(1);
                *CONFIRMATION.lock().unwrap() = Some(ConfirmationProbe {
                    engine: Arc::downgrade(&engine),
                    revision: desired.to_owned(),
                    port: id.to_string(),
                    original: original.owners[&id],
                    reached,
                    proceed,
                });
                let keys_before = output_b.keyed.load(Ordering::Acquire);
                let audible_before = output_b.audible.load(Ordering::Acquire);
                let (new_generation, premature, early_audio, a_interruptions, reply) =
                    thread::scope(|jobs| {
                        let job = jobs.spawn(|| operation(client, json!({"submitted":true,
                        "action":{"action":"apply_configuration"},"desired_etag":client.etag(DESIRED)})));
                        let new_generation = observer.recv_timeout(Duration::from_secs(5)).unwrap();
                        let frames = || {
                            output_b.keyed.load(Ordering::Acquire)
                                + output_b.dekeyed.load(Ordering::Acquire)
                        };
                        let start = frames();
                        let start_a = output_a.audible.load(Ordering::Acquire);
                        wait_until(|| {
                            frames() >= start + 8
                                && output_a.audible.load(Ordering::Acquire) >= start_a + 8
                        });
                        let premature = output_b.keyed.load(Ordering::Acquire) - keys_before;
                        let early_audio = output_b.audible.load(Ordering::Acquire) - audible_before;
                        let a_interruptions = output_a.dekeyed.load(Ordering::Acquire) - a_dekeys;
                        release.send(success).unwrap();
                        (
                            new_generation,
                            premature,
                            early_audio,
                            a_interruptions,
                            job.join().unwrap(),
                        )
                    });
                eprintln!(
                    "new B generation {new_generation}, old {}, B keyed {premature} and audible {early_audio} before confirmation, A dekeys {a_interruptions}, receipt {}",
                    original.owners[&id], reply.body["receipt"]["state"]
                );
                if success {
                    assert_eq!(
                        reply.body["receipt"]["state"], "completed",
                        "{}",
                        reply.body
                    );
                    client.ready();
                    assert_eq!(runtime.observed().owners[&id], new_generation);
                    sample("confirmed B now transmits", 3, 3);
                } else {
                    assert_ne!(
                        reply.body["receipt"]["state"], "completed",
                        "{}",
                        reply.body
                    );
                    wait_until(|| {
                        client.get("/api/v1/health/").body["status"]["historical_activation"]
                            == "recovery_required"
                    });
                    let quiet = output_b.dekeyed.load(Ordering::Acquire);
                    let keys = output_b.keyed.load(Ordering::Acquire);
                    wait_until(|| output_b.dekeyed.load(Ordering::Acquire) >= quiet + 8);
                    assert_eq!(output_b.keyed.load(Ordering::Acquire), keys);
                    assert_eq!(keys, keys_before, "B keyed during rejected confirmation");
                    assert_eq!(
                        output_b.audible.load(Ordering::Acquire),
                        audible_before,
                        "B emitted audio during rejected confirmation"
                    );
                }
                assert_eq!(a_interruptions, 0, "unaffected A lost local repeat");
                assert_eq!(
                    (premature, early_audio),
                    (0, 0),
                    "new B transmitted before native confirmation"
                );
                return;
            }
            manual(true);
            let fixed = operation(
                client,
                json!({"submitted":true,"action":{"action":"apply_configuration"},"desired_etag":client.etag(DESIRED)}),
            );
            assert_eq!(
                fixed.body["receipt"]["state"], "completed",
                "{}",
                fixed.body
            );
            client.ready();
            sample("confirmed recovery retains manual B inhibit", 3, 0);
            manual(false);
            sample("manual release after confirmed recovery", 3, 3);
            return;
        }
        if matches!(case, "native-remove" | "native-disable") {
            let previous = runtime.observed().active_revision.unwrap();
            let drops = fixture::RADIO_DROPS.load(Ordering::Acquire);
            counters.hold_stop.store(true, Ordering::Release);
            let request = if case == "native-remove" {
                client.edit("DELETE", &resource, None)
            } else {
                client.edit("PUT", &resource, Some(&json!({"configuration":{"name":"Radio","inherit_from":"site","enabled":false}})))
            };
            let deadline = Instant::now() + Duration::from_secs(4);
            while counters.stop_attempts.load(Ordering::Acquire) == 0 && Instant::now() < deadline {
                thread::yield_now();
            }
            let held = fixture::RADIO_DROPS.load(Ordering::Acquire);
            // A delayed stop spans several independent owner and durable-status turns.
            thread::sleep(Duration::from_millis(120));
            let health = client.get("/api/v1/health/").body;
            counters.hold_stop.store(false, Ordering::Release);
            assert_eq!(request.code, 202, "{}", request.body);
            assert_eq!(held, drops);
            assert_eq!(health["status"]["last_known_good"], previous, "{health}");
            assert_eq!(health["status"]["active_revision"], previous, "{health}");
            assert_ne!(health["status"]["historical_activation"], "confirmed");
            client.ready();
            assert_eq!(fixture::RADIO_DROPS.load(Ordering::Acquire), drops + 1);
            return;
        }
        if case == "native-local" {
            let other = other.as_ref().unwrap();
            fixture::REVIEW_CARRIER.store(true, Ordering::Release);
            wait_until(|| fixture::REVIEW_OUTPUT.load(Ordering::Acquire) == 3);
            fixture::REVIEW_CARRIER.store(false, Ordering::Release);
            fixture::REVIEW_OUTPUT.store(0, Ordering::Release);
            let before = runtime.observed();
            let prev_open = fixture::RADIO_OPENS.load(Ordering::Acquire);
            *counters.other.lock().unwrap() = Some(
                other
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap(),
            );
            fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
            let reply = client.edit("PUT", other, Some(&enable));
            assert_eq!(reply.code, 202, "{}", reply.body);
            wait_until(|| {
                client.get("/api/v1/health/").body["status"]["historical_activation"] == "failed"
            });
            assert_eq!(runtime.observed().owners, before.owners);
            let health = client.get("/api/v1/health/").body;
            assert_eq!(
                health["status"]["active_revision"].as_str(),
                before.active_revision.as_deref()
            );
            assert_eq!(
                health["status"]["last_known_good"],
                health["status"]["active_revision"]
            );
            assert_ne!(
                health["status"]["desired_revision"],
                health["status"]["active_revision"]
            );
            assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), prev_open);
            fixture::REVIEW_CARRIER.store(true, Ordering::Release);
            wait_until(|| fixture::REVIEW_OUTPUT.load(Ordering::Acquire) == 3);
            return;
        }
        if case == "native-stop" {
            fixture::REVIEW_CARRIER.store(true, Ordering::Release);
            wait_until(|| fixture::REVIEW_OUTPUT.load(Ordering::Acquire) == 3);
            edit(
                client,
                "POST",
                &format!("{resource}outputs/"),
                json!({"key":"relay","configuration":{"enabled":true,"source":{"type":"interface","node":"radio","property":"/gpio/pins/7"},"startup_state":"on","safe_state":"off"}}),
            );
            wait_until(|| crate::lifecycle::site_bank_delay(0) == 0x40);
            crate::lifecycle::site_bank_delay(usize::MAX);
            let stopped = engine.stop();
            let held = crate::lifecycle::site_bank_delay(0);
            assert!(
                !stopped,
                "shutdown returned before the site acknowledged its safe value"
            );
            assert_eq!(held, 0x40);
            let owned = WorkingFiles::open(&path).unwrap();
            assert!(matches!(
                owned.try_writer_lease(),
                Err(rptadv_peer_support::working_files::CaptureError::Busy)
            ));
            wait_until(|| fixture::REVIEW_OUTPUT.load(Ordering::Acquire) == 0);
            return;
        }
        if let Some(document) = &inspection_document {
            assert_eq!(managed_check(document, None), 0);
            let mut records = Vec::new();
            assert_eq!(managed_check(document, Some(&mut records)), 0);
            assert_eq!(
                records,
                vec![(*port_id.uuid().as_bytes(), "radio".to_owned(), 1)]
            );
            fixture_key_available(false);
            assert_eq!(managed_check(document, None), -1);
            assert_eq!(managed_check(document, Some(&mut records)), -1);
            assert_eq!(records.len(), 1);
            fixture_key_available(true);
            return;
        }
        let active = runtime.observed().active_revision.unwrap();
        let request = |expected: &str| {
            json!({"submitted":true,"targets":[port_id.to_string()],
            "expected_active_revision":expected,"action":{"action":"set_transmit_inhibit","enabled":true}})
        };
        let stale = operation(client, request("retired-active-revision"));
        assert_eq!(stale.code, 409);
        let effect = operation(client, request(&active));
        assert!((200..300).contains(&effect.code), "{}", effect.body);
        assert_eq!(effect.body["receipt"]["state"], "completed");
        assert_eq!(
            effect.body["receipt"]["outcomes"],
            json!([{"target":port_id.to_string(),"outcome":"admitted"}])
        );
        let inspect = || {
            engine
                .run(move |host| {
                    let node = host
                        .runtime
                        .node(&port_id.to_string())
                        .ok_or(rpt_advanced_core::runtime::RuntimeError::MissingNode)?;
                    Ok((
                        node.command_work().unwrap().id(),
                        node.site().transmit_inhibited(),
                    ))
                })
                .unwrap()
        };
        assert_eq!(inspect(), (generation, true));
        put(
            client,
            SITE_PATH,
            json!({"name":"Metadata only with active radio","inherit_from":null}),
        );
        // A committed configuration retires prior command authority, while the radio
        // stays open and the operator's manual inhibit survives policy reconfiguration.
        assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens + 1);
        let (current, inhibited) = inspect();
        assert!(current > generation);
        assert!(inhibited);
        assert_eq!(operation(client, request(&active)).code, 409);
    });
    drop(coordinator);
    assert!(engine.stop());
    drop(runtime);
    drop(engine);
    if case == "native-stop" {
        drop(
            WorkingFiles::open(&path)
                .unwrap()
                .try_writer_lease()
                .unwrap(),
        );
    }
    if case == "native-cold" {
        let locked = WorkingFiles::open(&path).unwrap();
        let guard = locked.try_edit().unwrap();
        let store = WorkingFiles::open(&path).unwrap();
        // SAFETY: all borrowed callback tables still live through confirmed stop.
        let second = unsafe {
            Engine::start_management(
                &store,
                &host,
                fixture::control_descriptor(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
            )
        }
        .unwrap();
        let rt = Arc::new(NativeRuntime::new(Arc::clone(&second)));
        let app = Coordinator::for_service(
            store,
            KeyId::new("example").unwrap(),
            LIMITS,
            rt.clone(),
            Arc::new(inputs()),
        )
        .unwrap();
        wait_until(|| second.fault_epoch() >= 2);
        drop(guard);
        let end = AtomicBool::new(false);
        thread::scope(|s| {
            s.spawn(|| {
                while !end.load(Ordering::Acquire) {
                    if !app.poll_listener(scenario.listener, scenario.inbox) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            let _stop = Stop(&end);
            let prior = client.etag(DESIRED);
            let opens = fixture::RADIO_OPENS.load(Ordering::Acquire);
            let reply = client.edit(
                "PUT",
                SITE_PATH,
                Some(&json!({"configuration":{"name":"Cold edit","inherit_from":null}})),
            );
            assert_eq!(reply.code, 409, "{}", reply.body);
            assert_eq!(client.etag(DESIRED), prior);
            assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens);
            assert!(rt.observed().active_revision.is_none());
            let repair = operation(
                client,
                json!({"submitted":true,"action":{"action":"apply_configuration"},"desired_etag":prior}),
            );
            assert_eq!(
                repair.body["receipt"]["state"], "completed",
                "{}",
                repair.body
            );
            client.ready();
            assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens + 1);
            put(
                client,
                SITE_PATH,
                json!({"name":"Recovered edit","inherit_from":null}),
            );
        });
        drop(app);
        assert!(second.stop());
    }
    assert_eq!(
        counters.prepared.load(Ordering::Acquire),
        counters.released.load(Ordering::Acquire)
    );
}

#[derive(Default)]
struct Counters {
    port: std::sync::Mutex<Option<rpt_advanced_core::config::resources::candidate::ResourceId>>,
    allow_open: AtomicBool,
    other: std::sync::Mutex<Option<rpt_advanced_core::config::resources::candidate::ResourceId>>,
    stage: std::sync::atomic::AtomicUsize,
    stages: std::sync::atomic::AtomicUsize,
    hold_stop: AtomicBool,
    stop_attempts: std::sync::atomic::AtomicUsize,
    block_bookkeeping: std::sync::Mutex<Option<std::path::PathBuf>>,
    prepared: std::sync::atomic::AtomicUsize,
    refused: std::sync::atomic::AtomicUsize,
    released: std::sync::atomic::AtomicUsize,
}

#[track_caller]
fn wait_until(mut ready: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(4);
    while !ready() {
        assert!(Instant::now() < until, "owner observation timed out");
        thread::sleep(Duration::from_millis(8));
    }
}
unsafe extern "C" fn quiesce(ctx: *mut std::ffi::c_void, _: *mut std::ffi::c_void) -> i32 {
    // SAFETY: engine borrows this fixture for the entire callback lifetime.
    let counters = unsafe { &*ctx.cast::<Counters>() };
    counters.stop_attempts.fetch_add(1, Ordering::Relaxed);
    i32::from(counters.hold_stop.load(Ordering::Acquire))
}
fn ops() -> crate::abi::rptadv_station_binding_v1 {
    use crate::abi::{rptadv_station_binding_v1, rptadv_station_request_v1};
    use std::ffi::c_void;
    unsafe extern "C" fn prepare(
        context: *mut c_void,
        request: *const rptadv_station_request_v1,
        plan: *mut *mut c_void,
    ) -> i32 {
        // SAFETY: the checked station client supplies one complete request and writable output.
        let request = unsafe { &*request };
        if request.abi_version != 1
            || request.struct_size as usize != std::mem::size_of::<rptadv_station_request_v1>()
            || request.port_uuid == [0; 16]
            || request.candidate_identity == 0
            || request.prepared.is_null()
            || request.product.is_null()
        {
            return -1;
        }
        // SAFETY: this fixture retains its counters until Engine::stop and all plans release.
        let counters = unsafe { &*context.cast::<Counters>() };
        let Ok(port) = counters.port.lock() else {
            return -1;
        };
        let Some(port) = *port else {
            return -1;
        };
        let other = *counters.other.lock().unwrap();
        let Some(port) = [Some(port), other]
            .into_iter()
            .flatten()
            .find(|p| p.uuid().as_bytes() == &request.port_uuid)
        else {
            return -1;
        };
        counters.prepared.fetch_add(1, Ordering::AcqRel);
        // SAFETY: output owns this prepared registered port until release.
        unsafe {
            *plan = Box::into_raw(Box::new(port.to_string())).cast();
        }
        0
    }
    unsafe extern "C" fn open(
        context: *mut c_void,
        plan: *const c_void,
        maximum: usize,
        radio: *mut *mut c_void,
    ) -> i32 {
        if plan.is_null() {
            return -1;
        }
        // SAFETY: both context and the uniquely retained plan outlive this callback.
        let (counters, name) = unsafe { (&*context.cast::<Counters>(), &*plan.cast::<String>()) };
        if !counters.allow_open.load(Ordering::Acquire) {
            counters.refused.fetch_add(1, Ordering::AcqRel);
            // SAFETY: the checked caller supplied the output slot.
            unsafe {
                *radio = std::ptr::null_mut();
            }
            return -1;
        }
        if let Some(path) = counters.block_bookkeeping.lock().unwrap().take() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o500)).unwrap();
        }
        // SAFETY: delegate transport allocation to the existing deterministic physical
        // test owner. Its activate/stop/release callbacks remain on the same checked host.
        unsafe {
            (*fixture::host_descriptor()).radio_open.unwrap()(
                context,
                name.as_ptr().cast(),
                name.len(),
                maximum,
                radio,
            )
        }
    }
    unsafe extern "C" fn stage(context: *mut c_void, _: *mut c_void, _: *const c_void) -> i32 {
        // SAFETY: the fixture retains its counters until the engine stops.
        let counters = unsafe { &*context.cast::<Counters>() };
        counters.stages.fetch_add(1, Ordering::AcqRel);
        match counters.stage.load(Ordering::Acquire) {
            1 => 0,
            2 => 1,
            _ => -1,
        }
    }
    unsafe extern "C" fn release(context: *mut c_void, plan: *mut c_void) {
        // SAFETY: release takes the one Box allocated by prepare.
        unsafe {
            drop(Box::from_raw(plan.cast::<String>()));
            (&*context.cast::<Counters>())
                .released
                .fetch_add(1, Ordering::AcqRel);
        }
    }
    rptadv_station_binding_v1 {
        struct_size: std::mem::size_of::<rptadv_station_binding_v1>() as u32,
        abi_version: 1,
        capability_name: c"rptadv.station-binding1".as_ptr(),
        prepare: Some(prepare),
        open_prepared: Some(open),
        stage_update: Some(stage),
        release_plan: Some(release),
    }
}

type PortRecord = ([u8; 16], String, u32);
fn managed_check(bytes: &[u8], records: Option<&mut Vec<PortRecord>>) -> i32 {
    unsafe extern "C" fn copy(
        context: *mut std::ffi::c_void,
        id: *const u8,
        key: *const std::ffi::c_char,
        length: usize,
        enabled: u32,
    ) -> i32 {
        if id.is_null() || key.is_null() || context.is_null() || enabled > 1 {
            return -1;
        }
        // SAFETY: the managed visitor lends 16 UUID bytes, a bounded key span and the originally supplied output vector.
        let (uuid, name, out) = unsafe {
            (
                *id.cast::<[u8; 16]>(),
                std::slice::from_raw_parts(key.cast::<u8>(), length),
                &mut *context.cast::<Vec<PortRecord>>(),
            )
        };
        let Ok(name) = std::str::from_utf8(name) else {
            return -1;
        };
        out.push((uuid, name.to_owned(), enabled));
        0
    }
    let (sink, context): (crate::abi::rptadv_managed_port_sink_v1, _) = match records {
        Some(out) => (Some(copy), std::ptr::from_mut(out).cast()),
        None => (None, std::ptr::null_mut()),
    };
    // SAFETY: the product's immutable static table outlives both synchronous spans.
    unsafe {
        (*crate::lifecycle::bootstrap::rptadv_product_inspection_descriptor_v1())
            .inspect
            .unwrap()(
            bytes.as_ptr().cast(),
            bytes.len(),
            sink,
            context,
            None,
            std::ptr::null_mut(),
        )
    }
}

/// Exercise the public management descriptor and a real, separately authenticated listener.
pub(super) fn bootstrap(scenario: &Scenario<'_>) {
    let _serial = fixture::LIFECYCLE.lock().unwrap_or_else(|e| e.into_inner());
    let (provision, address) = provisioning(scenario);
    // SAFETY: process-lifetime management table exported by this product.
    let descriptor =
        unsafe { &*crate::lifecycle::bootstrap::rptadv_product_management_descriptor_v1() };
    let start = |bytes: &[u8]| {
        // SAFETY: descriptors outlive successful stop and the bytes remain live for the call.
        unsafe {
            descriptor.start.unwrap()(
                fixture::host_descriptor(),
                fixture::control_descriptor().cast(),
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
                bytes.as_ptr().cast(),
                bytes.len(),
            )
        }
    };
    assert_eq!(descriptor.abi_version, 1);
    assert_eq!(
        descriptor.struct_size as usize,
        std::mem::size_of_val(descriptor)
    );
    assert_eq!(start(b"[radio]"), -1);
    let mut raw = provision.clone();
    raw["aead_key"] = json!("injected");
    assert_eq!(start(&serde_json::to_vec(&raw).unwrap()), -1);
    let client = Client {
        pki: scenario.client.pki,
        port: address.port(),
        proxy: None,
    };
    // SAFETY: this immutable historical table and all referenced code live in this process.
    let product = unsafe { &*crate::lifecycle::rptadv_product_descriptor_v1() };
    let check = |bytes: &[u8]| managed_check(bytes, None);
    assert_eq!(check(b"[station]"), -1);
    for expected in [None, Some("first")] {
        fixture_key_available(expected.is_some());
        let bytes = serde_json::to_vec(&provision).unwrap();
        assert_eq!(check(&bytes), if expected.is_some() { 0 } else { -1 });
        if std::env::var(CASE).as_deref() == Ok("inspection-r1") {
            let mut records = Vec::new();
            assert_eq!(managed_check(&bytes, Some(&mut records)), check(&bytes));
            assert!(records.is_empty());
        }
        let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
        assert!(store.try_writer_lease().is_ok());
        assert_eq!(start(&serde_json::to_vec(&provision).unwrap()), 0);
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let health = client.get("/api/v1/health/");
            assert_eq!(health.code, 200);
            let status = &health.body["status"];
            assert_eq!(status["desired_revision"], "first");
            if status["active_revision"].as_str() == expected {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "healthy boot did not receive actual adoption confirmation"
            );
            thread::sleep(Duration::from_millis(4));
        }
        if expected.is_none() {
            denied_save(&client, &scenario.fixture.0, "unavailable");
        }
        if expected.is_some() {
            let file = "working/site/metadata.json";
            let mut site: Value =
                serde_json::from_slice(&fs::read(scenario.fixture.0.join(file)).unwrap()).unwrap();
            site["configuration"]["name"] = json!("Reloaded through the service owner");
            scenario
                .fixture
                .put(file, &serde_json::to_vec(&site).unwrap());
            // The previous startup may have published active just before completing its
            // store receipt. Wait until a fresh storage read sees the precise external edit.
            let deadline = Instant::now() + Duration::from_secs(8);
            while client.get(STORAGE).body["storage"] != "external_edit" {
                assert!(Instant::now() < deadline, "working edit was not observed");
                thread::sleep(Duration::from_millis(4));
            }
            // No caller-supplied controller text is accepted on this path.
            let legacy = b"[station]";
            // SAFETY: the span is readable for the call; managed reload must refuse it.
            assert_eq!(
                unsafe { product.reload.unwrap()(legacy.as_ptr().cast(), legacy.len()) },
                -1
            );
            // SAFETY: null/zero requests the fixed trusted-host reload.
            let mut result = unsafe { product.reload.unwrap()(std::ptr::null(), 0) };
            let deadline = Instant::now() + Duration::from_secs(8);
            while result == 1 && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
                // SAFETY: polling is part of this selected product's lifecycle.
                result = unsafe { product.reload_poll.unwrap()() };
            }
            assert_eq!(result, 0, "trusted host reload failed");
            let health = client.get("/api/v1/health/").body;
            assert_ne!(health["status"]["desired_revision"], "first");
            assert_eq!(
                health["status"]["active_revision"],
                health["status"]["desired_revision"]
            );
            assert_eq!(
                client.get(SITE_PATH).body["configuration"]["name"],
                "Reloaded through the service owner"
            );
            let index = client.get("/api/v1/operations/").body;
            let href = index["_links"]["item"].as_array().unwrap()[0]["href"]
                .as_str()
                .unwrap();
            let receipt = client.get(href).body;
            assert_eq!(receipt["receipt"]["state"], "completed");
            assert_eq!(
                receipt["configuration"]["action"]["action"],
                "reload_configuration"
            );
        }
        let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
        assert!(matches!(
            store.try_writer_lease(),
            Err(rptadv_peer_support::working_files::CaptureError::Busy)
        ));
        assert_eq!(crate::lifecycle::rptadv_product_stop(), 0);
        assert!(store.try_writer_lease().is_ok());
    }
}

fn wait_for_physical_state(engine: &Arc<Engine>, name: &str, stopped: bool) {
    use rpt_advanced_core::runtime::LifecycleState;
    let deadline = Instant::now() + Duration::from_secs(5);
    let expected = if stopped {
        LifecycleState::Stopped
    } else {
        LifecycleState::Running
    };
    loop {
        let name = name.to_owned();
        let status = engine
            .run(move |host| Ok(host.runtime.node(&name).unwrap().status(0)))
            .unwrap();
        if status.state == expected {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "physical owner did not reach {expected:?}: {status:?}"
        );
        thread::sleep(Duration::from_millis(6));
    }
}

fn denied_save(client: &Client<'_>, root: &Path, reason: &str) {
    let before = fs::read(root.join("state.json")).unwrap();
    let metadata = client.get(SITE_PATH);
    assert_eq!(metadata.code, 200);
    let denied = client.send(
        "PUT",
        SITE_PATH,
        Some(("If-Match", &metadata.headers["etag"])),
        Some(&json!({"configuration":{"name":"Denied","inherit_from":null}})),
    );
    assert_eq!(
        (denied.code, denied.body["error"].as_str()),
        (503, Some(reason)),
        "{}",
        denied.body
    );
    assert_eq!(before, fs::read(root.join("state.json")).unwrap());
}

fn provisioning(scenario: &Scenario<'_>) -> (Value, std::net::SocketAddr) {
    let directory = std::path::PathBuf::from(std::env::var_os("CREDENTIALS_DIRECTORY").unwrap());
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let put = |name: &str, bytes: &[u8]| {
        let path = directory.join(format!("rpt-advanced.tls.{name}"));
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).unwrap();
    };
    for name in ["server", "client", "cli"] {
        put(name, cert(scenario.client.pki, name).as_ref());
    }
    let key = Command::new("openssl")
        .args(["pkcs8", "-topk8", "-nocrypt", "-outform", "DER", "-in"])
        .arg(scenario.client.pki.join("server.key"))
        .output()
        .unwrap();
    assert!(key.status.success());
    put("server-key", &key.stdout);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let provision = json!({
        "store_path":scenario.fixture.0, "store_uuid":STORE, "write_key_id":"example",
        "listen":address.to_string(),"authorities":["localhost",format!("localhost:{}",address.port())],
        "server_chain":["server"],"server_key":"server-key","client_issuers":["client"],
        "clients":[{"identity":"cli","leaf_certificate":"cli"}]
    });
    (provision, address)
}

struct Foreground(std::process::Child);

impl Drop for Foreground {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl Foreground {
    fn signal(&self, signal: &str) {
        assert!(
            Command::new("kill")
                .args([signal, &self.0.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
    }

    fn running(&mut self, deadline: Instant) {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "foreground process exited prematurely"
        );
        assert!(Instant::now() < deadline, "foreground process timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn standalone(scenario: &Scenario<'_>) {
    let binary =
        std::env::var_os("RPTADV_TEST_STANDALONE_BIN").expect("actual built foreground binary");
    let (service, address) = provisioning(scenario);
    let document = scenario.client.pki.join("service.json");
    fs::write(&document, b"[station]").unwrap();
    fs::set_permissions(&document, fs::Permissions::from_mode(0o600)).unwrap();
    let check = || {
        Command::new(&binary)
            .arg("--check-config")
            .arg(&document)
            .status()
            .unwrap()
    };
    assert!(!check().success());
    fs::write(&document, serde_json::to_vec(&service).unwrap()).unwrap();
    assert!(check().success());
    let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
    assert!(store.try_writer_lease().is_ok());
    let mut process = Foreground(
        Command::new(&binary)
            .arg("--foreground")
            .arg(&document)
            .spawn()
            .unwrap(),
    );
    check_frontend_reload(scenario, address, &store, &mut process, |process| {
        process.signal("-HUP")
    });
    process.signal("-TERM");
    wait_stopped(&mut process);
    assert!(store.try_writer_lease().is_ok());
}

fn check_frontend_reload(
    scenario: &Scenario<'_>,
    address: std::net::SocketAddr,
    store: &WorkingFiles,
    process: &mut Foreground,
    reload: impl FnOnce(&mut Foreground),
) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while std::net::TcpStream::connect_timeout(&address, Duration::from_millis(40)).is_err() {
        process.running(deadline);
    }
    let client = Client {
        pki: scenario.client.pki,
        port: address.port(),
        proxy: None,
    };
    while client.get("/api/v1/health/").body["status"]["active_revision"] != "first" {
        process.running(deadline);
    }
    assert!(matches!(
        store.try_writer_lease(),
        Err(rptadv_peer_support::working_files::CaptureError::Busy)
    ));
    let path = "working/site/metadata.json";
    let mut edited: Value =
        serde_json::from_slice(&fs::read(scenario.fixture.0.join(path)).unwrap()).unwrap();
    edited["configuration"]["name"] = json!("Frontend working reload");
    scenario
        .fixture
        .put(path, &serde_json::to_vec(&edited).unwrap());
    while client.get(STORAGE).body["storage"] != "external_edit" {
        process.running(deadline);
    }
    reload(process);
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let status = client.get("/api/v1/health/").body;
        let active = &status["status"]["active_revision"];
        if active.is_string()
            && active != "first"
            && active == &status["status"]["desired_revision"]
        {
            break;
        }
        process.running(deadline);
    }
    assert_eq!(
        client.get(SITE_PATH).body["configuration"]["name"],
        "Frontend working reload"
    );
}

fn wait_stopped(process: &mut Foreground) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "foreground did not drain");
        thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn asterisk(scenario: &Scenario<'_>) {
    let modules = std::path::PathBuf::from(
        std::env::var_os("RPT_TEST_MODULE_DIR").expect("fresh module directory"),
    );
    assert!(modules.join("app_rpt_advanced.so").is_file());
    let (service, address) = provisioning(scenario);
    let directory = scenario.fixture.0.join("asterisk");
    fs::create_dir(&directory).unwrap();
    let document = directory.join("rpt_advanced.service.json");
    fs::write(&document, serde_json::to_vec(&service).unwrap()).unwrap();
    fs::set_permissions(document, fs::Permissions::from_mode(0o600)).unwrap();
    let mut configuration = String::from("[directories]\n");
    // Asterisk reads its packaged Stasis XML from its configured data directory.
    // Only the test-owned runtime, configuration and writable state directories move.
    for key in [
        "astetcdir",
        "astrundir",
        "astlogdir",
        "astvarlibdir",
        "astdbdir",
        "astspooldir",
        "astagidir",
        "astkeydir",
    ] {
        configuration.push_str(&format!("{key} => {}\n", directory.display()));
    }
    configuration.push_str(&format!("astmoddir => {}\n", modules.display()));
    let config = directory.join("asterisk.conf");
    fs::write(&config, configuration).unwrap();
    fs::write(
        directory.join("modules.conf"),
        "[modules]\nautoload=no\nload=app_rpt_advanced.so\n",
    )
    .unwrap();
    let mut process = Foreground(
        Command::new("asterisk")
            .args(["-f", "-C"])
            .arg(&config)
            .spawn()
            .unwrap(),
    );
    let cli = |command: &str| {
        let result = Command::new("asterisk")
            .arg("-C")
            .arg(&config)
            .args(["-rx", command])
            .output()
            .unwrap();
        assert!(result.status.success(), "isolated module CLI failed");
        String::from_utf8(result.stdout).unwrap()
    };
    let store = WorkingFiles::open(&scenario.fixture.0).unwrap();
    check_frontend_reload(scenario, address, &store, &mut process, |_| {
        assert!(cli("module show like app_rpt_advanced").contains("Running"));
        cli("module reload app_rpt_advanced.so");
    });
    cli("module unload app_rpt_advanced.so");
    assert!(cli("module show like app_rpt_advanced").contains("0 modules loaded"));
    assert!(store.try_writer_lease().is_ok());
    cli("core stop now");
    wait_stopped(&mut process);
}
