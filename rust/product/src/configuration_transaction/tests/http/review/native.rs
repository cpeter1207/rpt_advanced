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
    let store = scenario.fixture.managed_store().unwrap();
    // This mTLS scenario runs alone in its own credential-isolated test process.
    // The C executor contains panics at its ABI boundary, so a join alone cannot expose them.
    let ticker_panicked = Arc::new(AtomicBool::new(false));
    if case == "native-site-views" {
        let observed = Arc::clone(&ticker_panicked);
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if thread::current().name() == Some("rpt-control-tick") {
                observed.store(true, Ordering::Release);
            }
            previous(info);
        }));
    }
    let confirm = matches!(case, "native-confirm" | "native-confirm-failure");
    let shared_views = matches!(
        case,
        "native-site-views" | "native-site-fence" | "native-site-fault-reload"
    );
    let review_radio = matches!(
        case,
        "native-site-claim-release"
            | "native-site-output-override"
            | "native-site-ptt-silence"
            | "native-site-alarm-live"
    );
    let shared_views = shared_views || review_radio;
    let two_live = confirm || case == "native-two-live" || shared_views;
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
    if review_radio
        || matches!(
            case,
            "native-stop" | "native-site-views" | "native-site-fence" | "native-site-fault-reload"
        )
    {
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
            Arc::clone(&store),
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

        if matches!(case, "native-site-alarm-zero" | "native-site-action-fault") {
            automatic_site_alarm(client, &engine, case, None);
            return;
        }
        if case == "native-site-zero" {
            shared_site_without_radios(client, &engine, opens);
            return;
        }

        let resource = port(client);
        let port_id: rpt_advanced_core::config::resources::candidate::ResourceId = resource
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        if review_radio
            || matches!(
                case,
                "native-stop"
                    | "native-reopen"
                    | "native-two-live"
                    | "native-site-views"
                    | "native-site-fence"
                    | "native-site-fault-reload"
                    | "native-confirm"
                    | "native-confirm-failure"
                    | "native-local"
                    | "native-bookkeeping"
                    | "native-repair"
                    | "native-new-fault"
            )
        {
            put(
                client,
                &format!("{resource}radio/"),
                json!({"duplex_mode":"full"}),
            );
        }
        let output_a = fixture::watch_output(&port_id.to_string());
        let other = if two_live || case == "native-local" {
            let key = if shared_views {
                "selected radio"
            } else {
                "second"
            };
            let other=edit(client,"POST",PORTS,json!({"key":key,"configuration":{"name":"B","inherit_from":"site","enabled":false}})).headers["location"].clone();
            put(
                client,
                &format!("{other}interface/"),
                if shared_views {
                    json!({"audio":{"device_identifier":"hw:other"},"gpio":{"pins":{"7":"out0","8":"in"}}})
                } else {
                    json!({"audio":{"device_identifier":"hw:other"}})
                },
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
                assert!(matches!(
                    WorkingFiles::open(&path).unwrap().try_writer_lease(),
                    Err(CaptureError::Busy)
                ));
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
            if review_radio {
                match case {
                    "native-site-claim-release" | "native-site-output-override" => {
                        reviewed_fan_claim(client, &resource, &port_id.to_string(), case)
                    }
                    "native-site-ptt-silence" => {
                        selected_callback_silence(client, &engine, &id.to_string(), &output_b)
                    }
                    "native-site-alarm-live" => {
                        automatic_site_alarm(client, &engine, case, Some(&output_b))
                    }
                    _ => unreachable!(),
                }
                return;
            }
            if case == "native-site-fault-reload" {
                shared_fan_fault_reload(
                    client,
                    &engine,
                    &resource,
                    &id.to_string(),
                    &port_id.to_string(),
                    opens,
                );
                return;
            }
            if case == "native-site-fence" {
                shared_output_fence(
                    client,
                    &engine,
                    (id, &output_b),
                    (port_id, &output_a),
                    opens,
                );
                return;
            }
            if shared_views {
                let surviving_radio = Arc::clone(&output_b);
                let (sensor, override_path) = shared_radio_views(
                    client,
                    &engine,
                    &resource,
                    [(id, output_b), (port_id, output_a)],
                    opens,
                );
                if std::env::var_os("RPTADV_SITE_FOCUS").is_some() {
                    return;
                }
                let original = runtime.observed();
                let domain = engine
                    .run(|host| Ok(Arc::clone(host.runtime.modular_configuration().unwrap())))
                    .unwrap();
                counters.stage.store(2, Ordering::Release);
                fixture::RADIO_OPEN_RESULT.store(1, Ordering::Release);
                let reply = client.edit("PUT", &format!("{resource}interface/"),
                    Some(&json!({"configuration":{"audio":{"device_identifier":"hw:replacement-refused"}}})));
                assert_eq!(reply.code, 202, "{}", reply.body);
                wait_until(|| {
                    client.get("/api/v1/health/").body["status"]["historical_activation"]
                        == "failed"
                });
                assert_eq!(runtime.observed().active_revision, original.active_revision);
                assert_eq!(runtime.observed().owners, original.owners);
                let source = client.get(&effective(&override_path)).body;
                assert_eq!(
                    source["meta"]["sources"]["/source/property"]["href"],
                    sensor
                );
                assert_eq!(source["meta"]["sources"]["/label"]["href"], override_path);
                let confirmed = Arc::clone(&domain);
                engine
                    .run(move |host| {
                        assert!(Arc::ptr_eq(
                            &confirmed,
                            host.runtime.modular_configuration().unwrap()
                        ));
                        for (owner, expected) in [
                            (SITE.to_owned(), 5.0),
                            (id.to_string(), 5.0),
                            (port_id.to_string(), 10.0),
                        ] {
                            let status = host.status_text(&owner)?;
                            assert!(
                                status.contains(&format!("sensor sense value=Some({expected:.1})")),
                                "{status}"
                            );
                        }
                        Ok(())
                    })
                    .unwrap();
                let audible = surviving_radio.audible.load(Ordering::Acquire);
                wait_until(|| surviving_radio.audible.load(Ordering::Acquire) >= audible + 4);
                // This time the native stage accepts but neither update nor reverse confirms.
                counters.stage.store(1, Ordering::Release);
                let failed = operation(
                    client,
                    json!({"submitted":true,"action":{"action":"apply_configuration"},
                    "desired_etag":client.etag(DESIRED)}),
                );
                assert_ne!(
                    failed.body["receipt"]["state"], "completed",
                    "{}",
                    failed.body
                );
                wait_until(|| {
                    client.get("/api/v1/health/").body["status"]["historical_activation"]
                        == "recovery_required"
                });
                assert!(runtime.observed().active_revision.is_none());
                wait_for_physical_state(&engine, &port_id.to_string(), true);
                assert_eq!(
                    engine.fixture_schedule_tick(),
                    Ok(()),
                    "scheduled work must continue after failed station rollback"
                );
                assert!(
                    engine
                        .run(|host| Ok(host.runtime.configuration_gate().load(Ordering::Acquire)))
                        .unwrap()
                );
                let quiet = surviving_radio.dekeyed.load(Ordering::Acquire);
                let keyed = surviving_radio.keyed.load(Ordering::Acquire);
                let audible = surviving_radio.audible.load(Ordering::Acquire);
                wait_until(|| surviving_radio.dekeyed.load(Ordering::Acquire) >= quiet + 4);
                assert_eq!(surviving_radio.keyed.load(Ordering::Acquire), keyed);
                assert_eq!(surviving_radio.audible.load(Ordering::Acquire), audible);
                let view = client.get(&effective(&override_path)).body;
                assert_eq!(view["meta"]["sources"]["/source/property"]["href"], sensor);
                assert_eq!(view["meta"]["sources"]["/label"]["href"], override_path);
                engine
                    .run(move |host| {
                        assert!(Arc::ptr_eq(
                            &domain,
                            host.runtime.modular_configuration().unwrap()
                        ));
                        for (owner, expected) in [
                            (SITE.to_owned(), 5.0),
                            (id.to_string(), 5.0),
                            (port_id.to_string(), 10.0),
                        ] {
                            let status = host.status_text(&owner)?;
                            assert!(
                                status.contains(&format!("sensor sense value=Some({expected:.1})")),
                                "{status}"
                            );
                        }
                        Ok(())
                    })
                    .unwrap();
                return;
            }
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
    assert!(
        !ticker_panicked.load(Ordering::Acquire),
        "the owned control ticker panicked during shared-site recovery"
    );
    if case == "native-site-fence" {
        assert_eq!(crate::lifecycle::site_bank_electrical(), (0, 0, false));
    }
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
    if case == "native-site-zero" {
        let board = scenario.client.pki.join("site-board");
        assert!(fs::read(board.join("pwm1")).unwrap().starts_with(b"0\n"));
        fs::write(board.join("in_temp_input"), b"unavailable\n").unwrap();
        let store = scenario.fixture.managed_store().unwrap();
        // Every provider/table remains live until this distinct cold service stops.
        let second = unsafe {
            Engine::start_management(
                Arc::clone(&store),
                &host,
                &control,
                rptadv_file_adapter::rptadv_file_adapter_descriptor().cast(),
                rptadv_speech_adapter::rptadv_speech_adapter_descriptor().cast(),
            )
        }
        .unwrap();
        let rt = Arc::new(NativeRuntime::new(Arc::clone(&second)));
        let app = Coordinator::for_service(
            store,
            KeyId::new("example").unwrap(),
            rt.clone(),
            Arc::new(inputs()),
        )
        .unwrap();
        let end = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                while !end.load(Ordering::Acquire) {
                    if !app.poll_listener(scenario.listener, scenario.inbox) {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            });
            let _stop = Stop(&end);
            let start = fixture::RADIO_OPENS.load(Ordering::Acquire);
            client.ready();
            let read = || {
                let clock = second.clock();
                second
                    .run(move |host| {
                        assert!(host.runtime.node_names().is_empty());
                        host.pump(clock)?;
                        host.status_text(SITE)
                    })
                    .unwrap()
            };
            wait_until(|| read().contains("sensor temperature value=None quality=Fault"));
            let health = client.get("/api/v1/health/").body;
            assert_eq!(
                health["status"]["active_revision"].as_str(),
                rt.observed().active_revision.as_deref()
            );
            let collection = client.get(&format!("{SITE_PATH}sensors/")).body;
            let href = collection["_links"]["item"][0]["href"].as_str().unwrap();
            let view = client.get(&effective(href)).body;
            assert_eq!(view["effective"]["label"], "Cabinet temperature");
            assert_eq!(view["meta"]["sources"]["/source/channel"]["href"], href);
            put(
                client,
                SITE_PATH,
                json!({"name":"Cold sensor unavailable","inherit_from":null}),
            );
            assert_ne!(
                health["status"]["active_revision"],
                client.get("/api/v1/health/").body["status"]["active_revision"]
            );
            assert!(read().contains("sensor temperature value=None quality=Fault"));
            assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), start);
        });
        drop(app);
        assert!(second.stop());
    }
    if case == "native-cold" {
        let locked = WorkingFiles::open(&path).unwrap();
        let guard = locked.try_edit().unwrap();
        let store = scenario.fixture.managed_store().unwrap();
        // SAFETY: all borrowed callback tables still live through confirmed stop.
        let second = unsafe {
            Engine::start_management(
                Arc::clone(&store),
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

/// A provisioned, radio-free site runs its real acquisition worker and thermal policy.
fn shared_site_without_radios(client: &Client<'_>, engine: &Arc<Engine>, opens: usize) {
    let board = client.pki.join("site-board");
    fs::create_dir(&board).unwrap();
    fs::write(board.join("in_temp_input"), b"42000\n").unwrap();
    let sensor = edit(
        client,
        "POST",
        &format!("{SITE_PATH}sensors/"),
        json!({"key":"temperature","configuration":{
            "enabled":true,"label":"Cabinet temperature",
            "source":{"type":"iio","device":board,"channel":"in_temp"},
            "timing":{"poll_interval_ms":50,"stale_after_ms":5000}
        }}),
    )
    .headers["location"]
        .clone();
    let view = client.get(&effective(&sensor));
    assert_eq!(view.body["effective"]["label"], "Cabinet temperature");
    assert_eq!(view.body["meta"]["sources"]["/label"]["href"], sensor);
    let status = || {
        let clock = engine.clock();
        engine
            .run(move |host| {
                assert!(host.runtime.node_names().is_empty());
                host.pump(clock)?;
                host.status_text(SITE)
            })
            .expect("the active site has its own status at zero radio ports")
    };
    wait_until(|| status().contains("sensor temperature value=Some(42.0) quality=Valid"));
    let active = client.get("/api/v1/health/").body["status"]["active_revision"].clone();
    let radio_only = operation(
        client,
        json!({"submitted":true,"targets":[SITE],"expected_active_revision":active,
        "action":{"action":"set_transmit_inhibit","enabled":true}}),
    );
    assert_eq!(radio_only.code, 503, "{}", radio_only.body);
    assert!(!status().contains("site transmit-inhibited=true"));

    fs::write(board.join("pwm1_enable"), b"1\n").unwrap();
    fs::write(board.join("pwm1"), b"0\n").unwrap();
    for (collection, key, configuration) in [
        (
            "outputs",
            "blower",
            json!({"enabled":true,"source":{
            "type":"hwmon","device":board,"channel":"pwm1"}}),
        ),
        (
            "alarms",
            "hot",
            json!({"enabled":true,"source":"temperature",
            "condition":"above","trip_value":40,"clear_value":35}),
        ),
        (
            "fans",
            "cooling",
            json!({"enabled":true,"mode":"temperature",
            "temperature_sensor":"temperature","on_temperature":40,
            "off_temperature":35,"output":"blower"}),
        ),
    ] {
        edit(
            client,
            "POST",
            &format!("{SITE_PATH}{collection}/"),
            json!({"key":key,"configuration":configuration}),
        );
    }
    wait_until(|| {
        let text = status();
        text.contains("alarm hot active=true")
            && text.contains("fan cooling requested-duty=1 fault=false")
            && text.lines().any(|line| {
                line.contains("output blower")
                    && line.contains("submitted=Some((1.0,")
                    && line.contains("readback=unavailable")
            })
            && fs::read(board.join("pwm1")).unwrap() == b"255\n"
    });
    assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens);
}

/// A metadata-only port reload cannot turn an unchanged failed fan submission into admission.
fn shared_fan_fault_reload(
    client: &Client<'_>,
    engine: &Arc<Engine>,
    unrelated_port: &str,
    selected: &str,
    caller: &str,
    opens: usize,
) {
    use crate::lifecycle::{site_bank_electrical, site_bank_sample};
    edit(
        client,
        "POST",
        &format!("{SITE_PATH}outputs/"),
        json!({
            "key":"blower","configuration":{"enabled":true,
                "source":{"type":"interface","node":"selected radio","property":"/gpio/pins/7"}}
        }),
    );
    edit(
        client,
        "POST",
        &format!("{SITE_PATH}fans/"),
        json!({
            "key":"cooling","configuration":{"enabled":true,"mode":"ptt","output":"blower",
                "ptt_nodes":["selected radio"],"timing":{"run_on_ms":0}}
        }),
    );
    let active = || client.get("/api/v1/health/").body["status"]["active_revision"].clone();
    let request = |target: &str, duty: Value| {
        let reply = operation(
            client,
            json!({"submitted":true,"targets":[target],"expected_active_revision":active(),
            "action":{"action":"set_fan","name":"cooling","duty":duty}}),
        );
        let outcome = &reply.body["receipt"]["outcomes"][0];
        assert_eq!(outcome["target"], target, "{}", reply.body);
        outcome["outcome"].as_str().unwrap().to_owned()
    };
    let read = |target: &str| {
        let scope = target.to_owned();
        let now = engine.clock();
        engine
            .run(move |host| {
                host.pump(now)?;
                host.status_text(&scope)
            })
            .unwrap()
    };
    wait_until(|| site_bank_electrical() == (0x40, 0, false));
    let writes = || site_bank_sample(selected, 20_000, 1).1;
    let initial_writes = writes();
    assert!(matches!(
        request(caller, json!(0.5)).as_str(),
        "rejected" | "indeterminate"
    ));
    assert!(read(SITE).contains("site-action cooling fault=Unavailable"));
    assert!(!read(caller).contains("site-action cooling"));
    let prior = active();
    put(
        client,
        unrelated_port,
        json!({"name":"Unrelated station label","enabled":true,"inherit_from":"site"}),
    );
    assert_ne!(active(), prior);
    let retained = read(SITE);
    assert!(
        retained.contains("fan cooling requested-duty=0.5"),
        "{retained}"
    );
    let outcomes = [SITE, caller, selected].map(|target| request(target, json!(0.5)));
    assert!(
        outcomes
            .iter()
            .all(|outcome| matches!(outcome.as_str(), "rejected" | "indeterminate")),
        "failed fan demand became admitted after unrelated reload: {outcomes:?}"
    );
    assert!(
        retained.contains("site-action cooling fault=Unavailable"),
        "{retained}"
    );
    for port in [caller, selected] {
        let status = read(port);
        assert!(
            status.contains("fan cooling requested-duty=0.5"),
            "{status}"
        );
        assert!(!status.contains("site-action cooling fault="), "{status}");
    }
    assert_eq!(site_bank_electrical(), (0x40, 0, false));
    assert_eq!(writes(), initial_writes);
    assert_eq!(request(SITE, json!("auto")), "admitted");
    assert!(!read(SITE).contains("site-action cooling fault="));
    assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens + 2);
}

fn add_equipment(
    client: &Client<'_>,
    scope: &str,
    collection: &str,
    key: &str,
    configuration: Value,
) -> String {
    edit(
        client,
        "POST",
        &format!("{scope}{collection}/"),
        json!({"key":key,"configuration":configuration}),
    )
    .headers["location"]
        .clone()
}

fn operation_outcome(client: &Client<'_>, target: &str, action: Value) -> Value {
    let revision = client.get("/api/v1/health/").body["status"]["active_revision"].clone();
    let reply = operation(
        client,
        json!({"submitted":true,"targets":[target],
        "expected_active_revision":revision,"action":action}),
    );
    let outcome = &reply.body["receipt"]["outcomes"][0];
    assert_eq!(outcome["target"], target, "{}", reply.body);
    outcome["outcome"].clone()
}

fn eventually(mut predicate: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(2);
    while !predicate() {
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_millis(8));
    }
    true
}

fn reviewed_fan_claim(client: &Client<'_>, port: &str, port_id: &str, case: &str) {
    use crate::lifecycle::site_bank_electrical;
    add_equipment(
        client,
        SITE_PATH,
        "outputs",
        "blower",
        json!({"enabled":true,
        "source":{"type":"interface","node":"selected radio","property":"/gpio/pins/7"}}),
    );
    wait_until(|| site_bank_electrical() == (0x40, 0, false));
    let release = case == "native-site-claim-release";
    let fan_scope = if release { port } else { SITE_PATH };
    let fan_target = if release { port_id } else { SITE };
    let fan = add_equipment(
        client,
        fan_scope,
        "fans",
        "cooling",
        json!({"enabled":true,"mode":"ptt",
        "output":"blower","ptt_nodes":["selected radio"],"timing":{"run_on_ms":0}}),
    );
    let request_fan = || {
        operation_outcome(
            client,
            fan_target,
            json!({"action":"set_fan","name":"cooling","duty":1}),
        )
    };
    assert_eq!(request_fan(), "admitted");
    wait_until(|| site_bank_electrical() == (0x40, 0x40, false));
    if release {
        let remove = client.edit("DELETE", &fan, None);
        assert_eq!(remove.code, 202, "{}", remove.body);
        client.ready();
        let released = eventually(|| site_bank_electrical() == (0x40, 0, false));
        let receipt = operation_outcome(
            client,
            SITE,
            json!({"action":"set_output","name":"blower","state":"off"}),
        );
        let off = eventually(|| site_bank_electrical() == (0x40, 0, false));
        assert!(
            released && receipt == "admitted" && off,
            "released fan output must physically revert to direct OFF: released={released} receipt={receipt} actual={:?}",
            site_bank_electrical()
        );
    } else {
        let board = client.pki.join("independent-output");
        fs::create_dir(&board).unwrap();
        fs::write(board.join("pwm1_enable"), b"1\n").unwrap();
        fs::write(board.join("pwm1"), b"0\n").unwrap();
        add_equipment(
            client,
            port,
            "outputs",
            "blower",
            json!({"enabled":true,
            "source":{"type":"hwmon","device":board,"channel":"pwm1"}}),
        );
        assert_eq!(request_fan(), "admitted");
        wait_until(|| site_bank_electrical() == (0x40, 0x40, false));
        let receipt = operation_outcome(
            client,
            port_id,
            json!({"action":"set_output","name":"blower","state":"on"}),
        );
        assert_eq!(
            receipt, "admitted",
            "inherited fan reserved independent port output"
        );
        wait_until(|| fs::read(board.join("pwm1")).unwrap().starts_with(b"255\n"));
        assert_eq!(
            operation_outcome(
                client,
                port_id,
                json!({"action":"set_output","name":"blower","state":"off"})
            ),
            "admitted"
        );
        wait_until(|| fs::read(board.join("pwm1")).unwrap().starts_with(b"0\n"));
        assert_eq!(site_bank_electrical(), (0x40, 0x40, false));
    }
}

fn automatic_site_alarm(
    client: &Client<'_>,
    engine: &Arc<Engine>,
    case: &str,
    radio: Option<&fixture::ReviewOutput>,
) {
    let board = client.pki.join("alarm-board");
    fs::create_dir(&board).unwrap();
    fs::write(board.join("in_temp_input"), b"12000\n").unwrap();
    add_equipment(
        client,
        SITE_PATH,
        "sensors",
        "temperature",
        json!({"enabled":true,
        "source":{"type":"iio","device":board,"channel":"in_temp"},"timing":{"poll_interval_ms":50,"stale_after_ms":5000}}),
    );
    let disabled = case == "native-site-action-fault";
    let action = if disabled {
        fs::write(board.join("pwm1_enable"), b"1\n").unwrap();
        fs::write(board.join("pwm1"), b"0\n").unwrap();
        add_equipment(
            client,
            SITE_PATH,
            "outputs",
            "blower",
            json!({"enabled":true,
            "source":{"type":"hwmon","device":board,"channel":"pwm1"}}),
        );
        add_equipment(
            client,
            SITE_PATH,
            "fans",
            "cooling",
            json!({"enabled":false,"mode":"ptt","output":"blower"}),
        );
        "fan:cooling:1"
    } else {
        "inhibit:on"
    };
    add_equipment(
        client,
        SITE_PATH,
        "alarms",
        "hot",
        json!({"enabled":true,"source":"temperature",
        "condition":"above","trip_value":40,"clear_value":35,"on_trip":[action]}),
    );
    let status = || {
        let clock = engine.clock();
        engine
            .run(move |host| {
                host.pump(clock)?;
                host.status_text(SITE)
            })
            .unwrap()
    };
    wait_until(|| status().contains("sensor temperature value=Some(12.0) quality=Valid"));
    if let Some(output) = radio {
        fixture::REVIEW_CARRIER.store(true, Ordering::Release);
        wait_until(|| output.value.load(Ordering::Acquire) == 3);
    }
    fs::write(board.join("in_temp_input"), b"42000\n").unwrap();
    wait_until(|| status().contains("alarm hot active=true"));
    let after = status();
    if disabled {
        assert!(
            after.contains("site-action alarm:hot fault=Unavailable"),
            "automatic disabled-fan command lost its source fault: {after}"
        );
        assert!(fs::read(board.join("pwm1")).unwrap().starts_with(b"0\n"));
    } else {
        if let Some(output) = radio {
            let previous = output.audible.load(Ordering::Acquire);
            wait_until(|| output.audible.load(Ordering::Acquire) >= previous + 4);
        }
        assert!(
            after.contains("site transmit-inhibited=false")
                && after.contains("site-action alarm:hot fault=Unavailable"),
            "site alarm has no RF target and must report unavailable without effective inhibition: {after}"
        );
    }
}

fn selected_callback_silence(
    client: &Client<'_>,
    engine: &Arc<Engine>,
    selected: &str,
    output: &fixture::ReviewOutput,
) {
    use crate::lifecycle::site_bank_electrical;
    add_equipment(
        client,
        SITE_PATH,
        "outputs",
        "blower",
        json!({"enabled":true,
        "source":{"type":"interface","node":"selected radio","property":"/gpio/pins/7"}}),
    );
    add_equipment(
        client,
        SITE_PATH,
        "fans",
        "cooling",
        json!({"enabled":true,"mode":"ptt",
        "output":"blower","ptt_nodes":["selected radio"],"timing":{"run_on_ms":0}}),
    );
    fixture::REVIEW_CARRIER.store(true, Ordering::Release);
    let opens = fixture::RADIO_OPENS.load(Ordering::Acquire);
    let status = || engine.run(|host| host.status_text(SITE)).unwrap();
    let observations = [true, false].map(|keyed| {
        let demand = usize::from(keyed);
        let high = if keyed { 0x40 } else { 0 };
        assert_eq!(
            operation_outcome(
                client,
                selected,
                json!({"action":"set_transmit_inhibit","enabled":!keyed})
            ),
            "admitted"
        );
        wait_until(|| {
            status().contains(&format!("fan cooling requested-duty={demand} fault=false"))
                && site_bank_electrical() == (0x40, high, false)
        });
        let before = output.skipped_tx.load(Ordering::Acquire);
        output.pause_tx.store(true, Ordering::Release);
        wait_until(|| output.skipped_tx.load(Ordering::Acquire) >= before + 4);
        let completed =
            output.keyed.load(Ordering::Acquire) + output.dekeyed.load(Ordering::Acquire);
        let expired = eventually(|| {
            status().contains(&format!("fan cooling requested-duty={demand} fault=true"))
        });
        assert_eq!(
            output.keyed.load(Ordering::Acquire) + output.dekeyed.load(Ordering::Acquire),
            completed
        );
        assert_eq!(site_bank_electrical(), (0x40, high, false));
        assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens);
        output.pause_tx.store(false, Ordering::Release);
        wait_until(|| {
            output.keyed.load(Ordering::Acquire) + output.dekeyed.load(Ordering::Acquire)
                > completed
                && status().contains(&format!("fan cooling requested-duty={demand} fault=false"))
        });
        expired
    });
    assert_eq!(
        observations,
        [true, true],
        "complete TX callback silence must fault both keyed and dekeyed fan observations"
    );
}

/// Keep one physical pulse and require a safe acknowledgment before disposing its output.
fn shared_output_fence(
    client: &Client<'_>,
    engine: &Arc<Engine>,
    selected: (
        rpt_advanced_core::config::resources::candidate::ResourceId,
        &fixture::ReviewOutput,
    ),
    other: (
        rpt_advanced_core::config::resources::candidate::ResourceId,
        &fixture::ReviewOutput,
    ),
    opens: usize,
) {
    use crate::lifecycle::{site_bank_delay, site_bank_electrical, site_bank_sample};
    let (radio, selected_output) = selected;
    let (caller, unaffected) = other;
    let resource = edit(client, "POST", &format!("{SITE_PATH}outputs/"),
        json!({"key":"utility","configuration":{
            "enabled":true,"source":{"type":"interface","node":"selected radio","property":"/gpio/pins/7"},
            "timing":{"pulse_ms":1800},"safe_state":"off"
        }})).headers["location"].clone();
    let current = client.get("/api/v1/health/").body["status"]["active_revision"].clone();
    let request = |target: &str, action: Value| {
        let result = operation(
            client,
            json!({"submitted":true,"targets":[target],
            "expected_active_revision":current,"action":action}),
        );
        assert_eq!(
            result.body["receipt"]["state"], "completed",
            "{}",
            result.body
        );
        assert_eq!(
            result.body["receipt"]["outcomes"],
            json!([{"target":target,"outcome":"admitted"}])
        );
    };
    let status = |name: String| engine.run(move |host| host.status_text(&name)).unwrap();
    wait_until(|| site_bank_electrical() == (0x40, 0, false));
    let owner = radio.to_string();
    let writes = || site_bank_sample(&owner, 20_000, 1).1;
    let before = writes();
    site_bank_delay(usize::MAX);
    request(
        &caller.to_string(),
        json!({"action":"set_output","name":"utility","state":"pulse"}),
    );
    wait_until(|| site_bank_electrical() == (0x40, 0, true));
    for scope in [SITE.to_owned(), caller.to_string(), owner.clone()] {
        let text = status(scope);
        assert!(
            text.lines()
                .any(|line| line.contains("output utility requested-on=true")
                    && line.contains("pulse-deadline-ms=Some(")
                    && line.contains("submitted=Some((0.0,")
                    && line.contains("readback=unavailable")),
            "{text}"
        );
    }
    assert_eq!(writes(), before + 1);
    site_bank_delay(0);
    wait_until(|| site_bank_electrical() == (0x40, 0x40, false));
    wait_until(|| site_bank_electrical() == (0x40, 0, false));
    assert_eq!(writes(), before + 2);
    request(
        SITE,
        json!({"action":"set_output","name":"utility","state":"on"}),
    );
    wait_until(|| site_bank_electrical() == (0x40, 0x40, false));
    request(
        &owner,
        json!({"action":"set_transmit_inhibit","enabled":true}),
    );
    fixture::REVIEW_CARRIER.store(true, Ordering::Release);
    wait_until(|| {
        selected_output.value.load(Ordering::Acquire) == 0
            && unaffected.value.load(Ordering::Acquire) == 3
    });
    let drops = fixture::RADIO_DROPS.load(Ordering::Acquire);
    site_bank_delay(usize::MAX);
    let remove = client.edit("DELETE", &resource, None);
    assert_eq!(remove.code, 202, "{}", remove.body);
    wait_until(|| site_bank_electrical() == (0x40, 0x40, true));
    let before_keys = unaffected.keyed.load(Ordering::Acquire);
    let before_dekeys = selected_output.dekeyed.load(Ordering::Acquire);
    wait_until(|| {
        unaffected.keyed.load(Ordering::Acquire) >= before_keys + 4
            && selected_output.dekeyed.load(Ordering::Acquire) >= before_dekeys + 4
    });
    let health = client.get("/api/v1/health/").body;
    assert_ne!(health["status"]["desired_revision"], current);
    assert_eq!(health["status"]["active_revision"], current);
    assert_eq!(fixture::RADIO_DROPS.load(Ordering::Acquire), drops);
    assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens + 2);
    assert!(
        !engine.stop(),
        "unconfirmed safe request must retain the physical owner"
    );
    assert_eq!(site_bank_electrical(), (0x40, 0x40, true));
    site_bank_delay(0);
    // The caller confirms the retried stop after the authenticated listener drains.
}

/// Two real radio controllers share a single cached bank under its exact registered owner.
fn shared_radio_views(
    client: &Client<'_>,
    engine: &Arc<Engine>,
    other: &str,
    stations: [(
        rpt_advanced_core::config::resources::candidate::ResourceId,
        Arc<fixture::ReviewOutput>,
    ); 2],
    opens: usize,
) -> (String, String) {
    use rpt_advanced_core::site::ReadingQuality;
    let [(a, output_a), (b, output_b)] = stations;
    let equipment = |scope: &str, collection: &str, key: &str, fields: Value| {
        edit(
            client,
            "POST",
            &format!("{scope}{collection}/"),
            json!({"key":key,"configuration":fields}),
        )
        .headers["location"]
            .clone()
    };
    let source = json!({"type":"interface","node":"selected radio","property":"/gpio/pins/8"});
    let calibration = |low, high| {
        json!({"mode":"two_point","input":{"low":0,"high":1},
        "value":{"low":low,"high":high}})
    };
    let sensor = equipment(
        SITE_PATH,
        "sensors",
        "sense",
        json!({"enabled":true,"source":source,
        "label":"Site gauge","calibration":calibration(5,20)}),
    );
    equipment(
        SITE_PATH,
        "inputs",
        "door",
        json!({"enabled":true,"source":source,"active_level":"low"}),
    );
    let override_path = equipment(
        other,
        "sensors",
        "sense",
        json!({"label":"Port gauge",
        "calibration":calibration(10,30),"timing":{"poll_interval_ms":10,"stale_after_ms":100}}),
    );
    equipment(other, "inputs", "door", json!({"active_level":"high"}));
    equipment(
        SITE_PATH,
        "outputs",
        "blower",
        json!({"enabled":true,"source":{
        "type":"interface","node":"selected radio","property":"/gpio/pins/7"}}),
    );
    equipment(
        SITE_PATH,
        "fans",
        "cooling",
        json!({"enabled":true,"mode":"ptt","output":"blower",
        "ptt_nodes":["selected radio"],"timing":{"run_on_ms":0}}),
    );
    let view = client.get(&effective(&override_path));
    assert_eq!(view.body["effective"]["label"], "Port gauge");
    assert_eq!(
        view.body["meta"]["sources"]["/source/property"]["href"],
        sensor
    );
    assert_eq!(
        view.body["meta"]["sources"]["/label"]["href"],
        override_path
    );
    let first_view = client.get(&effective(&sensor));
    assert_eq!(first_view.body["effective"]["label"], "Site gauge");

    let inspect = |advance, sample_time, quality| {
        let mut clock = engine.clock();
        clock.now_ms += advance;
        engine
            .run(move |host| {
                let owner = a.to_string();
                let before = crate::lifecycle::site_bank_sample(&owner, sample_time, quality);
                host.pump(clock)?;
                let after = crate::lifecycle::site_bank_sample(&owner, sample_time, quality);
                assert_eq!(
                    after.0,
                    before.0 + 1,
                    "one physical bank snapshot per serialized tick"
                );
                let mut snapshots = [a, b].map(|id| {
                    host.runtime
                        .node(&id.to_string())
                        .unwrap()
                        .site()
                        .snapshot(clock.now_ms)
                });
                Ok(snapshots.each_mut().map(|s| {
                    (
                        s.sensors.remove("sense").unwrap().current,
                        s.inputs.remove("door").unwrap(),
                    )
                }))
            })
            .unwrap()
    };
    let [(sensor_a, door_a), (sensor_b, door_b)] = inspect(0, 20_000, 1);
    assert_eq!((sensor_a.value, sensor_b.value), (Some(5.0), Some(10.0)));
    assert_eq!((door_a.value, door_b.value), (Some(true), Some(false)));
    assert_eq!(sensor_a.quality, ReadingQuality::Valid);
    assert_eq!(sensor_b.quality, ReadingQuality::Valid);
    let original = sensor_a.observed_at_ms.expect("acquired radio sample");
    assert_eq!(sensor_b.observed_at_ms, Some(original));
    let [(old_a, _), (old_b, _)] = inspect(110, 20_000, 1);
    assert_eq!(
        (old_a.observed_at_ms, old_b.observed_at_ms),
        (Some(original), Some(original))
    );
    assert_eq!(
        (old_a.quality, old_b.quality),
        (ReadingQuality::Valid, ReadingQuality::Stale)
    );
    let [(fault_a, fault_door_a), (fault_b, fault_door_b)] = inspect(115, 20_010, 2);
    for quality in [
        fault_a.quality,
        fault_b.quality,
        fault_door_a.quality,
        fault_door_b.quality,
    ] {
        assert_eq!(quality, ReadingQuality::Fault);
    }
    let fan = |target: String, duty: Value| {
        let rev = client.get("/api/v1/health/").body["status"]["active_revision"].clone();
        operation(
            client,
            json!({"submitted":true,"targets":[target],"expected_active_revision":rev,
            "action":{"action":"set_fan","name":"cooling","duty":duty}}),
        )
    };
    let status = |scope: String| engine.run(move |host| host.status_text(&scope)).unwrap();
    let focus = std::env::var("RPTADV_SITE_FOCUS").unwrap_or_default();
    if focus != "ptt" {
        for target in [b.to_string(), a.to_string()] {
            let rejected = fan(target, json!(0.5));
            assert!(
                matches!(
                    rejected.body["receipt"]["outcomes"][0]["outcome"].as_str(),
                    Some("rejected" | "indeterminate")
                ),
                "failed digital fan command was represented as admitted: {}",
                rejected.body
            );
            assert!(status(SITE.into()).contains("site-action cooling fault=Unavailable"));
            assert!(!status(b.to_string()).contains("site-action cooling"));
            assert_eq!(crate::lifecycle::site_bank_delay(0), 0);
        }
        assert_eq!(
            fan(SITE.into(), json!("auto")).body["receipt"]["outcomes"][0]["outcome"],
            "admitted"
        );
        assert!(!status(SITE.into()).contains("site-action cooling fault="));
    }
    if focus == "fractional" {
        return (sensor, override_path);
    }
    let inhibit_a = |enabled| {
        let revision = client.get("/api/v1/health/").body["status"]["active_revision"].clone();
        let effect = operation(
            client,
            json!({"submitted":true,"targets":[a.to_string()],
            "expected_active_revision":revision,"action":{"action":"set_transmit_inhibit","enabled":enabled}}),
        );
        assert_eq!(
            effect.body["receipt"]["outcomes"],
            json!([{"target":a.to_string(),"outcome":"admitted"}])
        );
    };
    inhibit_a(true);
    fixture::REVIEW_CARRIER.store(true, Ordering::Release);
    wait_until(|| {
        output_a.value.load(Ordering::Acquire) == 0 && output_b.value.load(Ordering::Acquire) == 3
    });
    inspect(120, 20_020, 1);
    assert_eq!(
        crate::lifecycle::site_bank_delay(0),
        0,
        "unselected radio keyed"
    );
    let writes = crate::lifecycle::site_bank_sample(&a.to_string(), 20_020, 1).1;
    inhibit_a(false);
    wait_until(|| output_a.value.load(Ordering::Acquire) == 3);
    wait_until(|| {
        inspect(120, 20_020, 1);
        crate::lifecycle::site_bank_delay(0) == 0x40
    });
    inspect(121, 20_020, 1);
    assert_eq!(
        crate::lifecycle::site_bank_sample(&a.to_string(), 20_020, 1).1,
        writes + 1
    );
    let healthy = output_a.dekeyed.load(Ordering::Acquire);
    output_a.missing_tx_frame.store(true, Ordering::Release);
    wait_until(|| output_a.dekeyed.load(Ordering::Acquire) >= healthy + 4);
    inspect(121, 20_020, 1);
    let missing = status(SITE.into());
    assert!(
        missing.contains("fan cooling requested-duty=1 fault=true"),
        "missing selected PTT reported as a confirmed release: {missing}"
    );
    assert_eq!(crate::lifecycle::site_bank_delay(0), 0x40);
    output_a.missing_tx_frame.store(false, Ordering::Release);
    wait_until(|| status(SITE.into()).contains("fan cooling requested-duty=1 fault=false"));
    assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens + 2);
    (sensor, override_path)
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
    let offline_first = std::env::var(CASE).as_deref() == Ok("managed-offline-first");
    if offline_first || std::env::var(CASE).as_deref() == Ok("managed-maintenance") {
        let bytes = serde_json::to_vec(&provision).unwrap();
        let root = &scenario.fixture.0;
        let owner = WorkingFiles::open(root).unwrap();
        use crate::configuration_transaction::maintenance::execute_test_request as maintenance;
        let mut request = json!({"store_root":root,"store_uuid":STORE});
        request["expected_state_sha256"] =
            maintenance(1, &request, None).unwrap()["state_sha256"].clone();
        let checkpoint = |point| {
            if point == Boundary::IntentBarrier {
                Err(TransactionError::StorageFault)
            } else {
                Ok(())
            }
        };
        assert!(maintenance(2, &request, Some(&checkpoint)).is_err());
        {
            let guard = owner.try_edit().unwrap();
            assert_eq!(
                Transaction::new(&guard, LIMITS).ordinary_writer_admission(STORE.parse().unwrap()),
                Err(TransactionError::Conflict)
            );
        }
        let selector = fs::read(root.join("state.json")).unwrap();
        let opens = fixture::RADIO_OPENS.load(Ordering::Acquire);
        let callbacks = fixture::REVIEW_CALLBACKS.load(Ordering::Acquire);
        // Keep this observed denial until after offline completion but before its worker starts.
        let blocked_store = offline_first.then(|| scenario.fixture.managed_store().unwrap());
        let offline = offline_first.then(|| owner.try_writer_lease().unwrap());
        assert_eq!(
            start(&bytes),
            0,
            "pending offline owner prevented read-only management startup"
        );
        let health = client.get("/api/v1/health/");
        assert_eq!(health.code, 200);
        assert_eq!(client.get("/api/v1/").code, 200);
        if offline.is_some() {
            assert!(matches!(owner.try_writer_lease(), Err(CaptureError::Busy)));
        } else {
            assert!(
                owner.try_writer_lease().is_ok(),
                "ownerless pending startup retained the writer lease"
            );
        }
        assert!(health.body["status"]["active_revision"].is_null());
        assert_eq!(client.get(SITE_PATH).code, 200);
        assert_eq!(health.body["status"]["storage"], "storage_fault");
        let apply = || {
            operation(
                &client,
                json!({"submitted":true,
            "action":{"action":"apply_configuration"}, "desired_etag":client.etag(DESIRED)}),
            )
        };
        for request in [
            client.edit(
                "PUT",
                SITE_PATH,
                Some(&json!({"configuration":{"name":"Blocked"}})),
            ),
            client.edit(
                "PATCH",
                SITE_PATH,
                Some(&json!([{"op":"replace","path":"/configuration/name","value":"Blocked"}])),
            ),
            client.edit(
                "POST",
                PORTS,
                Some(&json!({"key":"blocked","configuration":{"enabled":false}})),
            ),
            operation(&client, json!({"action":{"action":"apply_configuration"}})),
            apply(),
        ] {
            assert!(matches!(request.code, 409 | 503), "{}", request.body);
        }
        assert_eq!(fs::read(root.join("state.json")).unwrap(), selector);
        assert!(!root.join("operations.json").exists());
        assert_eq!(fixture::RADIO_OPENS.load(Ordering::Acquire), opens);
        assert_eq!(fixture::REVIEW_CALLBACKS.load(Ordering::Acquire), callbacks);
        assert_eq!(fixture::REVIEW_OUTPUT.load(Ordering::Acquire), 0);
        drop(offline);
        let lease = owner
            .try_writer_lease()
            .expect("offline owner can finish pending maintenance");
        drop(lease);
        maintenance(2, &request, None).unwrap();
        if let Some(store) = blocked_store {
            let runtime = Arc::new(RuntimeFixture::default());
            let coordinator = Coordinator::for_service(
                store,
                KeyId::new("example").unwrap(),
                Arc::clone(&runtime),
                Arc::new(inputs()),
            )
            .unwrap();
            drop(coordinator);
            assert_eq!(runtime.adoptions.load(Ordering::Acquire), 0);
            assert!(runtime.observed().active_revision.is_none());
            assert!(!root.join("operations.json").exists());
        }
        let stale = operation(
            &client,
            json!({"submitted":true,
            "action":{"action":"apply_configuration"}, "desired_etag":"stale"}),
        );
        assert_eq!(stale.code, 412);
        assert!(!root.join("operations.json").exists());
        let lease = owner.try_writer_lease().unwrap();
        assert!(matches!(apply().code, 409 | 503));
        assert!(!root.join("operations.json").exists());
        drop(lease);
        assert_eq!(apply().body["receipt"]["state"], "completed");
        client.ready();
        assert!(matches!(owner.try_writer_lease(), Err(CaptureError::Busy)));
        put(
            &client,
            SITE_PATH,
            json!({"name":"After maintenance","inherit_from":null}),
        );
        assert_ne!(client.get(DESIRED).body["desired_revision"], "first");
        assert_eq!(crate::lifecycle::rptadv_product_stop(), 0);
        assert_eq!(start(&bytes), 0);
        client.ready();
        assert_eq!(
            client.get(SITE_PATH).body["configuration"]["name"],
            "After maintenance"
        );
        let active = client.get(DESIRED).body["desired_revision"].clone();
        let ledger = fs::read(root.join("operations.json")).unwrap();
        scenario.fixture.put("maintenance.json", b"{}");
        wait_until(|| client.get(STORAGE).body["storage"] == "storage_fault");
        let health = client.get("/api/v1/health/").body;
        assert_eq!(health["status"]["storage"], "storage_fault");
        assert_eq!(health["status"]["active_revision"], active);
        assert!(matches!(owner.try_writer_lease(), Err(CaptureError::Busy)));
        assert_eq!(apply().code, 503);
        assert_eq!(fs::read(root.join("operations.json")).unwrap(), ledger);
        assert_eq!(crate::lifecycle::rptadv_product_stop(), 0);
        assert_eq!(start(&bytes), 0);
        let health = client.get("/api/v1/health/").body;
        assert!(health["status"]["active_revision"].is_null());
        assert_eq!(health["status"]["storage"], "storage_fault");
        assert!(owner.try_writer_lease().is_ok());
        assert_eq!(fs::read(root.join("operations.json")).unwrap(), ledger);
        assert_eq!(crate::lifecycle::rptadv_product_stop(), 0);
        if offline_first {
            // A live offline transaction can hold both locks during bootstrap.
            let lease = owner.try_writer_lease().unwrap();
            let edit = owner.try_edit().unwrap();
            assert_eq!(start(&bytes), 0);
            assert_eq!(client.get("/api/v1/health/").code, 200);
            assert_eq!(client.get("/api/v1/").code, 200);
            assert_eq!(crate::lifecycle::rptadv_product_stop(), 0);
            drop(edit);
            drop(lease);
        }
        assert!(owner.try_writer_lease().is_ok());
        return;
    }
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
