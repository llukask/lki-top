//! Reading the system. [`Sampler`] owns the sysinfo handles and turns
//! each refresh into a plain [`Snapshot`] the UI can render without
//! touching sysinfo again.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use sysinfo::{
    Components, CpuRefreshKind, Disks, MemoryRefreshKind, Networks, ProcessRefreshKind,
    ProcessesToUpdate, RefreshKind, System, ThreadKind, UpdateKind, Users,
};

use crate::gpu::{AmdGpu, GpuSample};

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub cpu: CpuSample,
    pub memory: MemorySample,
    pub network: NetworkSample,
    pub disks: Vec<DiskSample>,
    pub gpus: Vec<GpuSample>,
    pub processes: Vec<ProcessSample>,
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

#[derive(Debug, Clone, Copy, Default)]
pub struct NetworkSample {
    /// Bytes per second since the previous sample, summed over all
    /// interfaces except loopback.
    pub rx_per_sec: f64,
    pub tx_per_sec: f64,
    /// Bytes since each interface came up.
    pub rx_total: u64,
    pub tx_total: u64,
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
}

pub struct Sampler {
    system: System,
    networks: Networks,
    disks: Disks,
    components: Components,
    users: Users,
    user_names: HashMap<sysinfo::Uid, String>,
    gpus: Vec<AmdGpu>,
    /// When network and disk counters were last read, to turn the
    /// bytes since then into rates.
    last_refresh: Instant,
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
            networks: Networks::new_with_refreshed_list(),
            disks: Disks::new_with_refreshed_list(),
            components: Components::new_with_refreshed_list(),
            users: Users::new_with_refreshed_list(),
            user_names: HashMap::new(),
            gpus: AmdGpu::find_all(Path::new("/sys/class/drm")),
            last_refresh: Instant::now(),
        };
        sampler.sample();
        sampler
    }

    /// CPU usage is the delta since the previous call, so call this at
    /// least [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`] apart.
    pub fn sample(&mut self) -> Snapshot {
        self.system.refresh_cpu_all();
        self.system.refresh_memory();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_tasks()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .with_user(UpdateKind::OnlyIfNotSet),
        );
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
            network: self.network(elapsed),
            disks: self.disks(elapsed),
            gpus: self.gpus.iter().filter_map(AmdGpu::sample).collect(),
            processes: self.processes(),
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

    fn network(&self, elapsed_secs: f64) -> NetworkSample {
        let mut sample = NetworkSample::default();
        for (name, data) in &self.networks {
            if name == "lo" {
                continue;
            }
            sample.rx_per_sec += data.received() as f64 / elapsed_secs;
            sample.tx_per_sec += data.transmitted() as f64 / elapsed_secs;
            sample.rx_total += data.total_received();
            sample.tx_total += data.total_transmitted();
        }
        sample
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

    fn processes(&mut self) -> Vec<ProcessSample> {
        let core_count = self.system.cpus().len().max(1) as f32;
        let mut processes = Vec::with_capacity(self.system.processes().len());
        for (pid, process) in self.system.processes() {
            // sysinfo lists every thread of a process as a process of its
            // own on Linux. The thread count column covers them.
            if process.thread_kind() == Some(ThreadKind::Userland) {
                continue;
            }
            let user = match process.user_id() {
                Some(uid) => user_name(&mut self.users, &mut self.user_names, uid),
                None => String::new(),
            };
            let command = process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            processes.push(ProcessSample {
                pid: pid.as_u32(),
                parent: process.parent().map(|parent| parent.as_u32()),
                name: process.name().to_string_lossy().into_owned(),
                command,
                user,
                threads: process.tasks().map_or(1, |tasks| tasks.len().max(1)),
                memory: process.memory(),
                cpu: process.cpu_usage() / 100.0 / core_count,
            });
        }
        processes
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
}
