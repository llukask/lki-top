//! Reading the system. [`Sampler`] owns the sysinfo handles and turns
//! each refresh into plain data the UI can render without touching
//! sysinfo again: a [`Snapshot`] for the graphs and meters, and the
//! process list, which is costlier and read less often.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind,
    ProcessesToUpdate, RefreshKind, System, ThreadKind, UpdateKind, Users,
};

use crate::gpu::{AmdGpu, GpuSample};
use crate::psu::{CorsairPsu, PsuSample};

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub cpu: CpuSample,
    pub memory: MemorySample,
    /// Sorted by name.
    pub interfaces: Vec<InterfaceSample>,
    pub disks: Vec<DiskSample>,
    pub gpus: Vec<GpuSample>,
    pub psu: Option<PsuSample>,
    pub load_average: [f64; 3],
    pub uptime_secs: u64,
}

#[derive(Debug, Clone)]
pub struct CpuSample {
    pub brand: String,
    /// Usage across all cores, 0..=1.
    pub total: f32,
    /// Usage per logical core, 0..=1.
    pub per_core: Vec<f32>,
    /// Average frequency across cores.
    pub frequency_mhz: u64,
    /// Package temperature, if a sensor reports one.
    pub temperature_c: Option<f32>,
}

#[derive(Debug, Clone, Copy)]
pub struct MemorySample {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub free: u64,
    /// The kernel's page cache. `None` where `/proc/meminfo` can't be
    /// read; sysinfo doesn't report it.
    pub cached: Option<u64>,
    pub swap_total: u64,
    pub swap_used: u64,
}

/// One network interface, loopback aside.
#[derive(Debug, Clone)]
pub struct InterfaceSample {
    pub name: String,
    /// Backed by network hardware, as opposed to a bridge, tunnel or
    /// container veth, whose traffic also crosses a physical interface.
    pub physical: bool,
    pub traffic: Traffic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Traffic {
    /// Bytes per second since the previous sample.
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
    /// Bytes since the interface came up.
    pub rx_total: u64,
    pub tx_total: u64,
}

impl std::ops::Add for Traffic {
    type Output = Traffic;

    fn add(self, other: Traffic) -> Traffic {
        Traffic {
            rx_per_sec: self.rx_per_sec + other.rx_per_sec,
            tx_per_sec: self.tx_per_sec + other.tx_per_sec,
            rx_total: self.rx_total + other.rx_total,
            tx_total: self.tx_total + other.tx_total,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiskSample {
    pub mount_point: String,
    pub total: u64,
    pub used: u64,
    /// Bytes per second since the previous sample.
    pub read_per_sec: f64,
    pub write_per_sec: f64,
}

#[derive(Debug, Clone)]
pub struct ProcessSample {
    pub pid: u32,
    pub parent: Option<u32>,
    pub name: String,
    pub command: String,
    pub user: String,
    pub threads: usize,
    pub memory: u64,
    /// Share of total CPU capacity, 0..=1. sysinfo reports 100% per
    /// fully used core; dividing by the core count makes a process that
    /// saturates the whole machine read 100%, which is btop's default.
    pub cpu: f32,
    /// Average share of total CPU capacity since the process started,
    /// for the "cpu lazy" sort.
    pub lifetime_cpu: f32,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    /// The kernel's run state, e.g. `Sleeping` or `Runnable`.
    pub status: String,
    /// Bytes per second since the previous process refresh. Zero for
    /// other users' processes, whose `/proc/<pid>/io` isn't readable.
    pub disk_read_per_sec: f64,
    pub disk_write_per_sec: f64,
}

/// One refresh of the process list, with the machine totals that put
/// each process's numbers in proportion.
#[derive(Debug, Clone)]
pub struct ProcessList {
    pub processes: Vec<ProcessSample>,
    pub total_memory: u64,
    pub core_count: usize,
}

pub struct Sampler {
    system: System,
    /// Processes live in a `System` of their own. sysinfo divides each
    /// process's CPU time by the CPU time that passed since its
    /// `System` last read `/proc/stat`. The graphs read that more often
    /// than the process list refreshes, which would inflate every
    /// process's CPU share if they shared one.
    process_system: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    users: Users,
    user_names: HashMap<sysinfo::Uid, String>,
    gpus: Vec<AmdGpu>,
    psu: Option<CorsairPsu>,
    /// When network and disk counters were last read, to turn the
    /// bytes since then into rates.
    last_refresh: Instant,
    /// The same for per-process disk counters.
    last_process_refresh: Instant,
}

impl Sampler {
    /// Takes a throwaway first reading, so the first [`Sampler::sample`]
    /// already has a baseline to compute CPU usage against. On a busy
    /// machine that takes tens of milliseconds, so call it off the UI
    /// thread.
    pub fn new() -> Self {
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        let mut sampler = Self {
            system,
            process_system: System::new(),
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            user_names: HashMap::new(),
            gpus: AmdGpu::find_all(Path::new("/sys/class/drm")),
            psu: CorsairPsu::find(Path::new("/sys/class/hwmon")),
            last_refresh: Instant::now(),
            last_process_refresh: Instant::now(),
        };
        sampler.sample();
        sampler.sample_processes();
        sampler
    }

    /// CPU usage is the delta since the previous call, so call this at
    /// least [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`] apart.
    pub fn sample(&mut self) -> Snapshot {
        self.system.refresh_cpu_all();
        self.system.refresh_memory();
        self.networks.refresh(true);
        self.disks.refresh(true);
        self.components.refresh(false);

        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_refresh)
            .as_secs_f64()
            .max(f64::EPSILON);
        self.last_refresh = now;

        let load = System::load_average();
        Snapshot {
            cpu: self.cpu(),
            memory: self.memory(),
            interfaces: self.interfaces(elapsed),
            disks: self.disks(elapsed),
            gpus: self.gpus.iter().filter_map(AmdGpu::sample).collect(),
            psu: self.psu.as_ref().and_then(CorsairPsu::sample),
            load_average: [load.one, load.five, load.fifteen],
            uptime_secs: System::uptime(),
        }
    }

    fn cpu(&self) -> CpuSample {
        let cpus = self.system.cpus();
        let frequency_mhz = if cpus.is_empty() {
            0
        } else {
            cpus.iter().map(|cpu| cpu.frequency()).sum::<u64>() / cpus.len() as u64
        };
        CpuSample {
            brand: cpus
                .first()
                .map(|cpu| cpu.brand().trim().to_owned())
                .unwrap_or_default(),
            total: self.system.global_cpu_usage() / 100.0,
            per_core: cpus.iter().map(|cpu| cpu.cpu_usage() / 100.0).collect(),
            frequency_mhz,
            temperature_c: self.cpu_temperature(),
        }
    }

    /// Sensor labels differ per vendor and driver: `k10temp Tctl` on AMD,
    /// `coretemp Package id 0` on Intel. Take the first that matches one
    /// of the known package sensors.
    fn cpu_temperature(&self) -> Option<f32> {
        const PACKAGE_SENSORS: &[&str] = &["tctl", "package id 0", "tdie", "cpu"];
        PACKAGE_SENSORS.iter().find_map(|wanted| {
            self.components
                .iter()
                .find(|component| component.label().to_lowercase().contains(wanted))
                .and_then(|component| component.temperature())
        })
    }

    fn memory(&self) -> MemorySample {
        MemorySample {
            total: self.system.total_memory(),
            used: self.system.used_memory(),
            available: self.system.available_memory(),
            free: self.system.free_memory(),
            cached: page_cache(),
            swap_total: self.system.total_swap(),
            swap_used: self.system.used_swap(),
        }
    }

    fn interfaces(&self, elapsed_secs: f64) -> Vec<InterfaceSample> {
        let mut interfaces: Vec<InterfaceSample> = self
            .networks
            .iter()
            .filter(|(name, _)| name.as_str() != "lo")
            .map(|(name, data)| InterfaceSample {
                name: name.clone(),
                // Linux links an interface to its device only when there
                // is hardware behind it.
                physical: Path::new("/sys/class/net")
                    .join(name)
                    .join("device")
                    .exists(),
                traffic: Traffic {
                    rx_per_sec: data.received() as f64 / elapsed_secs,
                    tx_per_sec: data.transmitted() as f64 / elapsed_secs,
                    rx_total: data.total_received(),
                    tx_total: data.total_transmitted(),
                },
            })
            .collect();
        interfaces.sort_by(|a, b| a.name.cmp(&b.name));
        interfaces
    }

    /// One row per block device. On NixOS `/nix/store` is a read-only
    /// bind mount of the root filesystem, and btrfs subvolumes show up
    /// once per mount point, so keep only the first mount of each device.
    fn disks(&self, elapsed_secs: f64) -> Vec<DiskSample> {
        let mut seen = std::collections::HashSet::new();
        self.disks
            .iter()
            .filter(|disk| disk.total_space() > 0)
            .filter(|disk| seen.insert(disk.name().to_owned()))
            .map(|disk| DiskSample {
                mount_point: disk.mount_point().to_string_lossy().into_owned(),
                total: disk.total_space(),
                used: disk.total_space().saturating_sub(disk.available_space()),
                read_per_sec: disk.usage().read_bytes as f64 / elapsed_secs,
                write_per_sec: disk.usage().written_bytes as f64 / elapsed_secs,
            })
            .collect()
    }

    /// Process CPU usage is the delta since the previous call, so the
    /// same minimum gap as [`Sampler::sample`] applies.
    pub fn sample_processes(&mut self) -> ProcessList {
        self.process_system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_tasks()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_disk_usage(),
        );
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.last_process_refresh)
            .as_secs_f64()
            .max(f64::EPSILON);
        self.last_process_refresh = now;
        let cores = self.system.cpus().len().max(1);
        let core_count = cores as f32;
        let mut processes = Vec::with_capacity(self.process_system.processes().len());
        for (pid, process) in self.process_system.processes() {
            // sysinfo lists every thread of a process as a process of its
            // own on Linux. The thread count column covers them.
            if process.thread_kind() == Some(ThreadKind::Userland) {
                continue;
            }
            let user = match process.user_id() {
                Some(uid) => user_name(&mut self.users, &mut self.user_names, uid),
                None => String::new(),
            };
            let command = command_line(process.cmd());
            processes.push(ProcessSample {
                pid: pid.as_u32(),
                parent: process.parent().map(|parent| parent.as_u32()),
                name: process.name().to_string_lossy().into_owned(),
                command,
                user,
                threads: process.tasks().map_or(1, |tasks| tasks.len().max(1)),
                memory: process.memory(),
                cpu: process.cpu_usage() / 100.0 / core_count,
                // CPU-milliseconds over wall seconds, a fraction of one
                // core, then of the machine. A process younger than a
                // second counts as one second old.
                lifetime_cpu: process.accumulated_cpu_time() as f32
                    / 1000.0
                    / process.run_time().max(1) as f32
                    / core_count,
                start_time: process.start_time(),
                status: process.status().to_string(),
                disk_read_per_sec: process.disk_usage().read_bytes as f64 / elapsed,
                disk_write_per_sec: process.disk_usage().written_bytes as f64 / elapsed,
            });
        }
        ProcessList {
            processes,
            total_memory: self.system.total_memory(),
            core_count: cores,
        }
    }
}

/// Look up a user name, re-reading `/etc/passwd` only when a uid shows up
/// that the cache hasn't seen. Unknown uids fall back to the number, the
/// way `ps` does.
fn user_name(
    users: &mut Users,
    cache: &mut HashMap<sysinfo::Uid, String>,
    uid: &sysinfo::Uid,
) -> String {
    if let Some(name) = cache.get(uid) {
        return name.clone();
    }
    if users.get_user_by_id(uid).is_none() {
        users.refresh();
    }
    let name = users
        .get_user_by_id(uid)
        .map(|user| user.name().to_owned())
        .unwrap_or_else(|| uid.to_string());
    cache.insert(uid.clone(), name.clone());
    name
}

/// The `Cached:` line of `/proc/meminfo`, in bytes.
fn page_cache() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo_kib(&meminfo, "Cached:").map(|kib| kib * 1024)
}

fn parse_meminfo_kib(meminfo: &str, label: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// The arguments as one line. An argument can hold newlines and tabs,
/// such as the script of an `sh -c`, and the table's rows are one line
/// tall, so control characters become spaces.
fn command_line(args: &[std::ffi::OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(|char| if char.is_control() { ' ' } else { char })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_cache_skips_swap_cached() {
        let meminfo = "MemFree:         7340032 kB\n\
                       SwapCached:          512 kB\n\
                       Cached:         26214400 kB\n";
        assert_eq!(parse_meminfo_kib(meminfo, "Cached:"), Some(26_214_400));
        assert_eq!(parse_meminfo_kib(meminfo, "Shmem:"), None);
    }

    #[test]
    fn a_multi_line_argument_fits_on_one_line() {
        let args = ["sh", "-c", "set -e\n\tcargo build"].map(std::ffi::OsString::from);
        assert_eq!(command_line(&args), "sh -c set -e  cargo build");
    }
}
