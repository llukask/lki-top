//! GPU readings straight from the kernel's sysfs files, which `amdgpu`
//! fills with usage, VRAM, temperature and power. No vendor library is
//! needed for those.
//!
//! TODO: NVIDIA cards report through NVML (`libnvidia-ml`) only, and
//! Intel's i915 only through perf counters that need `CAP_PERFMON`;
//! neither shows up here yet.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct GpuSample {
    /// Share of time the GPU was busy, 0..=1.
    pub usage: f32,
    pub vram_used: u64,
    pub vram_total: u64,
    /// Edge temperature, and the critical one that tops its color scale.
    pub temperature_c: Option<f32>,
    pub critical_c: Option<f32>,
    pub power_w: Option<f32>,
    /// The board's power limit, the top of the power color scale.
    pub power_cap_w: Option<f32>,
}

/// One `amdgpu` card, found once at startup.
#[derive(Debug)]
pub struct AmdGpu {
    /// The card's `device` directory, e.g. `/sys/class/drm/card1/device`.
    device: PathBuf,
    /// Its `hwmon/hwmonN` directory, for temperature and power.
    hwmon: Option<PathBuf>,
}

impl AmdGpu {
    /// Every card under `drm` (normally `/sys/class/drm`) that reports
    /// its usage, in card order. Connector entries like `card1-DP-1`
    /// are skipped.
    pub fn find_all(drm: &Path) -> Vec<Self> {
        let Ok(entries) = std::fs::read_dir(drm) else {
            return Vec::new();
        };
        let mut cards: Vec<(u32, PathBuf)> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let number = entry
                    .file_name()
                    .to_str()?
                    .strip_prefix("card")?
                    .parse()
                    .ok()?;
                let device = entry.path().join("device");
                device
                    .join("gpu_busy_percent")
                    .exists()
                    .then_some((number, device))
            })
            .collect();
        cards.sort();
        cards
            .into_iter()
            .map(|(_, device)| {
                let hwmon = std::fs::read_dir(device.join("hwmon"))
                    .ok()
                    .and_then(|mut dirs| dirs.next())
                    .and_then(|dir| dir.ok())
                    .map(|dir| dir.path());
                Self { device, hwmon }
            })
            .collect()
    }

    /// `None` when the usage can't be read, e.g. after the card is gone.
    pub fn sample(&self) -> Option<GpuSample> {
        let device = |file: &str| read_number(&self.device.join(file));
        let hwmon = |file: &str| {
            self.hwmon
                .as_ref()
                .and_then(|dir| read_number(&dir.join(file)))
        };
        // hwmon reports millidegrees and microwatts. Newer kernels name
        // the power reading `power1_input` instead of `power1_average`.
        let celsius = |file: &str| hwmon(file).map(|milli| (milli / 1000.0) as f32);
        let watts = |file: &str| hwmon(file).map(|micro| (micro / 1_000_000.0) as f32);
        Some(GpuSample {
            usage: (device("gpu_busy_percent")? / 100.0) as f32,
            vram_used: device("mem_info_vram_used").unwrap_or(0.0) as u64,
            vram_total: device("mem_info_vram_total").unwrap_or(0.0) as u64,
            temperature_c: celsius("temp1_input"),
            critical_c: celsius("temp1_crit"),
            power_w: watts("power1_average").or_else(|| watts("power1_input")),
            power_cap_w: watts("power1_cap"),
        })
    }
}

fn read_number(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `/sys/class/drm` with one usable card, one connector and
    /// one card without usage reporting, like the i915 iGPU.
    fn fake_drm(name: &str) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-sysfs")
            .join(name);
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: &str, value: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("test paths have a parent"))
                .expect("target/ is writable");
            std::fs::write(path, value).expect("target/ is writable");
        };
        write("card0/device/gt_act_freq_mhz", "300\n");
        write("card1/device/gpu_busy_percent", "37\n");
        write("card1/device/mem_info_vram_used", "1654886400\n");
        write("card1/device/mem_info_vram_total", "17163091968\n");
        write("card1/device/hwmon/hwmon2/temp1_input", "48000\n");
        write("card1/device/hwmon/hwmon2/temp1_crit", "100000\n");
        write("card1/device/hwmon/hwmon2/power1_input", "34000000\n");
        write("card1-DP-1/status", "connected\n");
        root
    }

    #[test]
    fn reads_an_amdgpu_card_in_display_units() {
        let gpus = AmdGpu::find_all(&fake_drm("amdgpu"));
        assert_eq!(gpus.len(), 1, "{gpus:?}");
        let sample = gpus[0].sample().expect("usage is readable");
        assert!((sample.usage - 0.37).abs() < 1e-6);
        assert_eq!(sample.vram_used, 1_654_886_400);
        assert_eq!(sample.vram_total, 17_163_091_968);
        assert_eq!(sample.temperature_c, Some(48.0));
        assert_eq!(sample.critical_c, Some(100.0));
        assert_eq!(sample.power_w, Some(34.0));
        assert_eq!(sample.power_cap_w, None);
    }
}
