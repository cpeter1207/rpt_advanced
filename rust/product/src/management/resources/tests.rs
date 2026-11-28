use super::*;

fn recorded() -> Counters {
    let (host_cpu, online_count) =
        cpu_counters("cpu 100 10 35 330 10 4 5 6 20 3\ncpu0 40 3 10 110\ncpu7 60 7 25 220\n");
    Counters {
        host_cpu,
        online_count,
        process_cpu: process_ticks("123 (test ) process) R 1 1 1 0 0 0 1 0 0 0 50 20 0"),
        ticks_per_second: Some(100),
        host_memory: (
            kibibytes("MemTotal: 2048 kB\nMemAvailable: 768 kB\n", "MemAvailable:"),
            kibibytes("MemTotal: 2048 kB\nMemAvailable: 768 kB\n", "MemTotal:"),
        ),
        resident_bytes: kibibytes("VmRSS:\t256 kB\n", "VmRSS:"),
        network: Some(BTreeMap::from([(
            "eth0".into(),
            InterfaceCounters {
                identity: Some((9, 1, 44)),
                bytes: Some((10_000, 20_000)),
            },
        )])),
        ..Counters::default()
    }
}

fn sample(
    sampler: &mut ResourceSampler,
    milliseconds: u64,
    counters: Counters,
) -> serde_json::Value {
    let seconds = 1_795_586_400 + (milliseconds / 1000) as i64;
    let subsecond = (milliseconds % 1000) as u16;
    serde_json::to_value(sampler.advance(milliseconds, utc_timestamp(seconds, subsecond), counters))
        .unwrap()
}

#[test]
fn recorded_rates_use_elapsed_time_and_report_warming_and_unavailable() {
    let mut sampler = ResourceSampler::default();
    let first = sample(&mut sampler, 1000, recorded());
    assert_eq!(first["monotonic_ms"], "1000");
    assert_eq!(
        first["observed_at"],
        utc_timestamp(1_795_586_401, 0).unwrap()
    );
    assert_eq!(first["cpu"]["online_count"], 2);
    assert_eq!(first["cpu"]["host"]["quality"], "warming");
    assert_eq!(first["cpu"]["process"]["quality"], "warming");
    assert_eq!(first["network"]["interfaces"]["eth0"]["quality"], "warming");
    assert!(first["network"]["interfaces"]["eth0"]["rx_bps"].is_null());
    assert_eq!(first["memory"]["host"]["available_bytes"], "786432");
    assert_eq!(first["memory"]["host"]["total_bytes"], "2097152");
    assert_eq!(first["memory"]["process"]["resident_bytes"], "262144");
    assert_eq!(first["memory"]["service"]["quality"], "unavailable");

    let mut counters = recorded();
    counters.host_cpu = Some((600, 400));
    counters.process_cpu = Some(145);
    counters
        .network
        .as_mut()
        .unwrap()
        .get_mut("eth0")
        .unwrap()
        .bytes = Some((11_500, 20_750));
    let second = sample(&mut sampler, 1500, counters.clone());
    assert_eq!(second["cpu"]["host"]["percent"], 40.0);
    assert_eq!(second["cpu"]["process"]["percent"], 150.0);
    assert_eq!(second["cpu"]["process"]["window_ms"], "500");
    let link = &second["network"]["interfaces"]["eth0"];
    assert_eq!(link["quality"], "valid");
    assert_eq!(link["window_ms"], "500");
    assert_eq!(link["rx_bps"], "24000");
    assert_eq!(link["tx_bps"], "12000");
    assert_eq!(link["observed_at"], second["observed_at"]);

    let iface = counters.network.as_mut().unwrap().get_mut("eth0").unwrap();
    iface.identity = Some((9, 1, 45));
    let replaced = sample(&mut sampler, 2500, counters.clone());
    assert_eq!(
        replaced["network"]["interfaces"]["eth0"]["quality"],
        "warming"
    );

    counters
        .network
        .as_mut()
        .unwrap()
        .get_mut("eth0")
        .unwrap()
        .bytes = Some((1, 2));
    let reset = sample(&mut sampler, 3500, counters.clone());
    assert_eq!(reset["network"]["interfaces"]["eth0"]["quality"], "warming");

    counters
        .network
        .as_mut()
        .unwrap()
        .get_mut("eth0")
        .unwrap()
        .bytes = None;
    counters.process_cpu = None;
    let unavailable = sample(&mut sampler, 4500, counters);
    assert_eq!(
        unavailable["network"]["interfaces"]["eth0"]["quality"],
        "unavailable"
    );
    assert_eq!(unavailable["cpu"]["process"]["quality"], "unavailable");

    let removed = sample(
        &mut sampler,
        5500,
        Counters {
            network: Some(BTreeMap::new()),
            ..Counters::default()
        },
    );
    assert_eq!(removed["network"]["interfaces"], serde_json::json!({}));
    let returned = sample(&mut sampler, 6500, recorded());
    assert_eq!(
        returned["network"]["interfaces"]["eth0"]["quality"],
        "warming"
    );
    assert_eq!(returned["cpu"]["process"]["quality"], "warming");

    let unreadable = sample(&mut sampler, 7500, Counters::default());
    assert_eq!(unreadable["network"]["quality"], "unavailable");
    assert_eq!(
        unreadable["network"]["interfaces"]["eth0"]["quality"],
        "unavailable"
    );
}

#[test]
fn resource_clock_and_cgroup_mount_resolution_preserve_meaning() {
    assert_eq!(utc_timestamp(0, 42).unwrap(), "1970-01-01T00:00:00.042Z");
    assert!(utc_timestamp(-1, 0).is_none());
    assert!(utc_timestamp(0, 1000).is_none());
    let mounts = "31 28 0:28 / /wrong rw - tmpfs tmpfs rw\n\
                  32 28 0:29 /other /wrong2 rw - cgroup2 cgroup rw\n\
                  33 28 0:30 /a /sys/fs/cgroup\\040test rw - cgroup2 cgroup rw\n";
    let (mount, leaf) = cgroup_location("0::/a/service\n", mounts).unwrap();
    assert_eq!(mount, Path::new("/sys/fs/cgroup test"));
    assert_eq!(leaf, Path::new("/sys/fs/cgroup test/service"));
    assert!(cgroup_location("0::/a/../service\n", mounts).is_none());
    let seen = [Some("max"), Some("300"), Some("100"), Some("max")];
    assert_eq!(effective_memory_limit(seen), Some(Some(100)));
    assert_eq!(effective_memory_limit([Some("max")]), Some(None));
    assert_eq!(effective_memory_limit([Some("100"), None]), None);
}

#[test]
fn observer_reads_counters_at_most_once_per_second() {
    let mut sampler = ResourceSampler::default();
    assert!(sampler.poll(20, || None).is_some());
    assert!(sampler.poll(1019, || panic!("sampled too early")).is_none());
    assert!(sampler.poll(1020, || None).is_some());
}
