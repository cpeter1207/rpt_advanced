//! Host resource observations. Only the non-realtime management observer reads Linux files.
use serde::{Serialize, Serializer};
use std::{
    collections::BTreeMap,
    fs,
    mem::MaybeUninit,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

type CpuTicks = (u64, u64);
type Bytes = (u64, u64);
type InterfaceIdentity = (u32, u64, u64);
type Interfaces = BTreeMap<String, InterfaceCounters>;

/// The status protocol represents new 64-bit values as decimal strings.
#[derive(Clone, Copy, PartialEq)]
struct Decimal(u64);

impl Serialize for Decimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Quality {
    Valid,
    Warming,
    Unavailable,
}

impl Quality {
    fn available(available: bool) -> Self {
        if available {
            Self::Valid
        } else {
            Self::Unavailable
        }
    }

    fn rate(source: bool, window: bool) -> Self {
        if !source {
            Self::Unavailable
        } else if window {
            Self::Valid
        } else {
            Self::Warming
        }
    }
}

/// One site-wide capture, shared by the status surfaces.
#[derive(PartialEq, Serialize)]
pub(crate) struct Resources {
    observed_at: Option<String>,
    monotonic_ms: Decimal,
    cpu: Cpu,
    memory: Memory,
    network: Network,
}

#[derive(PartialEq, Serialize)]
struct Cpu {
    online_count: Option<u32>,
    host: CpuRate,
    process: CpuRate,
}

#[derive(PartialEq, Serialize)]
struct CpuRate {
    percent: Option<f64>,
    window_ms: Option<Decimal>,
    quality: Quality,
}

impl CpuRate {
    fn new(source: bool, measurement: Option<(f64, u64)>) -> Self {
        Self {
            percent: measurement.map(|(percent, _)| percent),
            window_ms: measurement.map(|(_, elapsed)| Decimal(elapsed)),
            quality: Quality::rate(source, measurement.is_some()),
        }
    }
}

#[derive(PartialEq, Serialize)]
struct Memory {
    host: HostMemory,
    process: ProcessMemory,
    service: ServiceMemory,
}

#[derive(PartialEq, Serialize)]
struct HostMemory {
    available_bytes: Option<Decimal>,
    total_bytes: Option<Decimal>,
    quality: Quality,
}

#[derive(PartialEq, Serialize)]
struct ProcessMemory {
    resident_bytes: Option<Decimal>,
    quality: Quality,
}

#[derive(PartialEq, Serialize)]
struct ServiceMemory {
    current_bytes: Option<Decimal>,
    limit_bytes: Option<Decimal>,
    quality: Quality,
    limit_quality: Quality,
}

#[derive(PartialEq, Serialize)]
struct Network {
    quality: Quality,
    interfaces: BTreeMap<String, InterfaceRate>,
}

#[derive(PartialEq, Serialize)]
struct InterfaceRate {
    ifindex: Option<u32>,
    rx_bps: Option<Decimal>,
    tx_bps: Option<Decimal>,
    window_ms: Option<Decimal>,
    observed_at: Option<String>,
    quality: Quality,
}

#[derive(Clone, Copy)]
struct InterfaceCounters {
    // Linux interface index plus the actual sysfs directory identity.
    identity: Option<InterfaceIdentity>,
    bytes: Option<Bytes>,
}

#[derive(Clone, Default)]
struct Counters {
    host_cpu: Option<CpuTicks>,
    online_count: Option<u32>,
    process_cpu: Option<u64>,
    ticks_per_second: Option<u64>,
    host_memory: (Option<u64>, Option<u64>),
    resident_bytes: Option<u64>,
    service_current: Option<u64>,
    // An observed unlimited hierarchy is distinct from an unavailable limit.
    service_limit: Option<Option<u64>>,
    network: Option<Interfaces>,
}

/// The Linux sampler is owned by the status observer, not a product worker.
#[derive(Default)]
pub(crate) struct ResourceSampler {
    previous: Option<(u64, Counters)>,
}

impl ResourceSampler {
    /// Read a fresh set of counters at most once per second of service time.
    pub(crate) fn poll(
        &mut self,
        now_ms: u64,
        observed_at: impl FnOnce() -> Option<String>,
    ) -> Option<Resources> {
        if self
            .previous
            .as_ref()
            .is_some_and(|(last, _)| now_ms.saturating_sub(*last) < 1000)
        {
            return None;
        }
        Some(self.advance(now_ms, observed_at(), Counters::read()))
    }

    fn advance(
        &mut self,
        now_ms: u64,
        observed_at: Option<String>,
        current: Counters,
    ) -> Resources {
        let previous = self.previous.as_ref().map(|(_, counters)| counters);
        let elapsed = self
            .previous
            .as_ref()
            .and_then(|(last, _)| now_ms.checked_sub(*last).filter(|elapsed| *elapsed != 0));

        let host_rate = previous.zip(elapsed).and_then(|(old, elapsed)| {
            if current.online_count != old.online_count {
                return None;
            }
            let (total, idle) = current.host_cpu?;
            let (old_total, old_idle) = old.host_cpu?;
            let total = total.checked_sub(old_total)?;
            let idle = idle.checked_sub(old_idle)?;
            (total != 0).then_some((
                total.checked_sub(idle)? as f64 * 100.0 / total as f64,
                elapsed,
            ))
        });
        let process_rate = previous.zip(elapsed).and_then(|(old, elapsed)| {
            let ticks = current.process_cpu?.checked_sub(old.process_cpu?)?;
            let ticks_per_second = current.ticks_per_second?;
            Some((
                ticks as f64 * 100_000.0 / (ticks_per_second as f64 * elapsed as f64),
                elapsed,
            ))
        });
        let cpu = Cpu {
            online_count: current.online_count,
            host: CpuRate::new(current.host_cpu.is_some(), host_rate),
            process: CpuRate::new(
                current.process_cpu.zip(current.ticks_per_second).is_some(),
                process_rate,
            ),
        };

        let (available, total) = current.host_memory;
        let memory = Memory {
            host: HostMemory {
                available_bytes: available.map(Decimal),
                total_bytes: total.map(Decimal),
                quality: Quality::available(available.zip(total).is_some()),
            },
            process: ProcessMemory {
                resident_bytes: current.resident_bytes.map(Decimal),
                quality: Quality::available(current.resident_bytes.is_some()),
            },
            service: ServiceMemory {
                current_bytes: current.service_current.map(Decimal),
                limit_bytes: current.service_limit.flatten().map(Decimal),
                quality: Quality::available(current.service_current.is_some()),
                limit_quality: Quality::available(current.service_limit.is_some()),
            },
        };
        let old_interfaces = previous.and_then(|old| old.network.as_ref());
        let network = Network {
            quality: Quality::available(current.network.is_some()),
            interfaces: current
                .network
                .as_ref()
                .or(old_interfaces)
                .into_iter()
                .flatten()
                .map(|(name, counters)| {
                    let valid_source = current.network.is_some()
                        && counters.identity.zip(counters.bytes).is_some();
                    let rate = elapsed.filter(|_| valid_source).and_then(|window| {
                        let old = old_interfaces?.get(name)?;
                        if old.identity != counters.identity {
                            return None;
                        }
                        let (rx, tx) = counters.bytes?;
                        let (old_rx, old_tx) = old.bytes?;
                        Some((
                            rate(rx.checked_sub(old_rx)?, window)?,
                            rate(tx.checked_sub(old_tx)?, window)?,
                            window,
                        ))
                    });
                    let interface = InterfaceRate {
                        ifindex: counters.identity.map(|(index, _, _)| index),
                        rx_bps: rate.map(|(rx, _, _)| Decimal(rx)),
                        tx_bps: rate.map(|(_, tx, _)| Decimal(tx)),
                        window_ms: rate.map(|(_, _, elapsed)| Decimal(elapsed)),
                        observed_at: observed_at.clone(),
                        quality: Quality::rate(valid_source, rate.is_some()),
                    };
                    (name.clone(), interface)
                })
                .collect(),
        };

        self.previous = Some((now_ms, current));
        Resources {
            observed_at,
            monotonic_ms: Decimal(now_ms),
            cpu,
            memory,
            network,
        }
    }
}

/// Format one captured wall time without relying on the machine's local timezone.
pub(crate) fn utc_timestamp(wall_seconds: i64, wall_subsecond_ms: u16) -> Option<String> {
    if wall_seconds < 0 || wall_subsecond_ms >= 1000 {
        return None;
    }
    let mut converted = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: the two pointers address valid distinct objects of the requested C types.
    let result = unsafe { libc::gmtime_r(&wall_seconds, converted.as_mut_ptr()) };
    if result.is_null() {
        return None;
    }
    // SAFETY: a non-null return initialized the caller-provided UTC breakdown.
    let converted = unsafe { converted.assume_init() };
    let year = converted.tm_year.checked_add(1900)?;
    (0..=9999).contains(&year).then(|| {
        format!(
            "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{wall_subsecond_ms:03}Z",
            converted.tm_mon + 1,
            converted.tm_mday,
            converted.tm_hour,
            converted.tm_min,
            converted.tm_sec,
        )
    })
}

fn rate(bytes: u64, elapsed_ms: u64) -> Option<u64> {
    u64::try_from(u128::from(bytes) * 8000 / u128::from(elapsed_ms)).ok()
}

impl Counters {
    fn read() -> Self {
        let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
        let (host_cpu, online_count) = cpu_counters(&stat);
        let mem = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let process = fs::read_to_string("/proc/self/stat").unwrap_or_default();
        let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
        // SAFETY: sysconf reads a numeric system constant without retaining memory.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let (service_current, service_limit) = cgroup_memory().unwrap_or_default();
        Self {
            host_cpu,
            online_count,
            process_cpu: process_ticks(&process),
            ticks_per_second: u64::try_from(ticks).ok().filter(|ticks| *ticks != 0),
            host_memory: (
                kibibytes(&mem, "MemAvailable:"),
                kibibytes(&mem, "MemTotal:"),
            ),
            resident_bytes: kibibytes(&status, "VmRSS:"),
            service_current,
            service_limit,
            network: interfaces(),
        }
    }
}

fn cpu_counters(stat: &str) -> (Option<CpuTicks>, Option<u32>) {
    let mut lines = stat.lines();
    let aggregate = lines.next().and_then(|line| {
        let values: Vec<u64> = line
            .strip_prefix("cpu ")?
            .split_whitespace()
            .take(8)
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        // Guest ticks are already accounted for by user and nice.
        let [_, _, _, idle, iowait, ..] = values.as_slice() else {
            return None;
        };
        let idle = idle.checked_add(*iowait)?;
        let total = values.into_iter().try_fold(0u64, u64::checked_add)?;
        Some((total, idle))
    });
    let online_count = lines
        .take_while(|line| {
            line.split_whitespace()
                .next()
                .and_then(|label| label.strip_prefix("cpu"))
                .is_some_and(|id| id.parse::<u32>().is_ok())
        })
        .count();
    (
        aggregate,
        u32::try_from(online_count).ok().filter(|count| *count != 0),
    )
}

fn process_ticks(stat: &str) -> Option<u64> {
    // The comm field is parenthesized and can contain spaces and close parentheses.
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    let user = fields.nth(11)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    user.checked_add(system)
}

fn kibibytes(text: &str, label: &str) -> Option<u64> {
    let mut fields = text
        .lines()
        .find_map(|line| line.strip_prefix(label))?
        .split_whitespace();
    let value = fields.next()?.parse::<u64>().ok()?;
    (fields.next()? == "kB").then_some(value.checked_mul(1024)?)
}

fn mount_path(text: &str) -> PathBuf {
    PathBuf::from(
        text.replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\"),
    )
}

fn cgroup_location(membership: &str, mounts: &str) -> Option<(PathBuf, PathBuf)> {
    let group = Path::new(
        membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))?,
    );
    if !group.is_absolute() || group.components().any(|part| part == Component::ParentDir) {
        return None;
    }
    mounts
        .lines()
        .filter_map(|line| {
            let (mount_info, filesystem) = line.split_once(" - ")?;
            if !filesystem.starts_with("cgroup2 ") {
                return None;
            }
            let mut fields = mount_info.split_whitespace();
            let root = mount_path(fields.nth(3)?);
            let mount = mount_path(fields.next()?);
            let relative = group.strip_prefix(&root).ok()?;
            let leaf = mount.join(relative);
            Some((root.components().count(), mount, leaf))
        })
        .min_by_key(|(root_depth, _, _)| *root_depth)
        .map(|(_, mount, leaf)| (mount, leaf))
}

fn effective_memory_limit<'a>(
    limits: impl IntoIterator<Item = Option<&'a str>>,
) -> Option<Option<u64>> {
    limits
        .into_iter()
        .try_fold(None, |limit, value| match value?.trim() {
            "max" => Some(limit),
            bytes => {
                let bytes = bytes.parse::<u64>().ok()?;
                Some(Some(limit.map_or(bytes, |current: u64| current.min(bytes))))
            }
        })
}

type CgroupMemory = (Option<u64>, Option<Option<u64>>);

fn cgroup_memory() -> Option<CgroupMemory> {
    let membership = fs::read_to_string("/proc/self/cgroup").ok()?;
    let mounts = fs::read_to_string("/proc/self/mountinfo").ok()?;
    let (mount, leaf) = cgroup_location(&membership, &mounts)?;
    let current = number(&leaf.join("memory.current"));
    let limits: Vec<_> = leaf
        .ancestors()
        .take_while(|group| group.starts_with(&mount))
        .map(|group| match fs::read_to_string(group.join("memory.max")) {
            Ok(value) => Some(value),
            Err(error) if group == mount && error.kind() == std::io::ErrorKind::NotFound => {
                Some("max".to_owned())
            }
            Err(_) => None,
        })
        .collect();
    Some((
        current,
        effective_memory_limit(limits.iter().map(Option::as_deref)),
    ))
}

fn number<T: std::str::FromStr>(path: &Path) -> Option<T> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn interface_identity(path: &Path) -> Option<InterfaceIdentity> {
    let inode = fs::metadata(path).ok()?;
    Some((number(&path.join("ifindex"))?, inode.dev(), inode.ino()))
}

fn interface(path: &Path, name: &str) -> Option<InterfaceCounters> {
    let flags = fs::read_to_string(path.join("flags"))
        .ok()
        .and_then(|flags| u32::from_str_radix(flags.trim().strip_prefix("0x")?, 16).ok());
    if flags.map_or(name == "lo", |bits| bits & libc::IFF_LOOPBACK as u32 != 0) {
        return None;
    }
    let identity = interface_identity(path);
    let bytes = flags.zip(identity).and_then(|(_, identity)| {
        let rx = number(&path.join("statistics/rx_bytes"))?;
        let tx = number(&path.join("statistics/tx_bytes"))?;
        // Never pair counters from a replaced interface with the retired identity.
        (interface_identity(path) == Some(identity)).then_some((rx, tx))
    });
    Some(InterfaceCounters { identity, bytes })
}

fn interfaces() -> Option<Interfaces> {
    Some(
        fs::read_dir("/sys/class/net")
            .ok()?
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let name = entry.file_name().into_string().ok()?;
                let observation = interface(&entry.path(), &name)?;
                Some((name, observation))
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "resources/tests.rs"]
mod tests;
