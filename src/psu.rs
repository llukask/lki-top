//! Power supply readings from the kernel's `corsairpsu` hwmon driver,
//! which talks to Corsair's HX…i and RM…i units over USB.
//!
//! TODO: other PSUs with a USB link (NZXT, Thermaltake) have no kernel
//! driver; liquidctl reads them from userspace.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct PsuSample {
    pub model: Option<&'static str>,
    /// The unit's rated output, the top of its load meter.
    pub rating_w: Option<f32>,
    /// Power delivered to the computer: the sum of the 12 V, 5 V and
    /// 3.3 V rails. What the unit draws from the wall is more, by its
    /// efficiency, and the driver doesn't report it.
    pub output_w: f32,
    /// The VRM temperature, and the critical one that tops its scale.
    pub temperature_c: Option<f32>,
    pub critical_c: Option<f32>,
}

/// USB product ids, from the device table of the kernel's
/// drivers/hwmon/corsair-psu.c, with each unit's rated watts.
const MODELS: [(u16, &str, f32); 15] = [
    (0x1c03, "HX550i", 550.0),
    (0x1c04, "HX650i", 650.0),
    (0x1c05, "HX750i", 750.0),
    (0x1c06, "HX850i", 850.0),
    (0x1c07, "HX1000i", 1000.0),
    (0x1c08, "HX1200i", 1200.0),
    (0x1c09, "RM550i", 550.0),
    (0x1c0a, "RM650i", 650.0),
    (0x1c0b, "RM750i", 750.0),
    (0x1c0c, "RM850i", 850.0),
    (0x1c0d, "RM1000i", 1000.0),
    (0x1c1e, "HX1000i", 1000.0),
    (0x1c1f, "HX1500i", 1500.0),
    (0x1c23, "HX1200i", 1200.0),
    (0x1c27, "HX1200i", 1200.0),
];

/// A Corsair PSU's hwmon directory, found once at startup.
#[derive(Debug)]
pub struct CorsairPsu {
    hwmon: PathBuf,
    model: Option<(&'static str, f32)>,
}

impl CorsairPsu {
    /// The first `corsairpsu` device under `hwmon_root` (normally
    /// `/sys/class/hwmon`).
    pub fn find(hwmon_root: &Path) -> Option<Self> {
        let hwmon = std::fs::read_dir(hwmon_root)
            .ok()?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .find(|dir| {
                std::fs::read_to_string(dir.join("name"))
                    .is_ok_and(|name| name.trim() == "corsairpsu")
            })?;
        let model = product_id(&hwmon).and_then(|id| {
            MODELS
                .iter()
                .find(|(known, _, _)| *known == id)
                .map(|&(_, name, watts)| (name, watts))
        });
        Some(Self { hwmon, model })
    }

    /// `None` when the output can't be read, e.g. after the cable is
    /// pulled.
    pub fn sample(&self) -> Option<PsuSample> {
        let read = |file: &str| read_number(&self.hwmon.join(file));
        // hwmon reports microwatts and millidegrees.
        let celsius = |file: &str| read(file).map(|milli| (milli / 1000.0) as f32);
        Some(PsuSample {
            model: self.model.map(|(name, _)| name),
            rating_w: self.model.map(|(_, watts)| watts),
            output_w: (read("power1_input")? / 1_000_000.0) as f32,
            temperature_c: celsius("temp1_input"),
            critical_c: celsius("temp1_crit"),
        })
    }
}

/// The USB product id from the name of the HID device the hwmon
/// directory belongs to, such as `0003:1B1C:1C0C.000C`: bus, vendor,
/// product, then an instance number. The unit's own USB product string
/// is blank on at least the RM850i, so the id is the only way to the
/// model.
fn product_id(hwmon: &Path) -> Option<u16> {
    let device = std::fs::read_link(hwmon.join("device")).ok()?;
    let name = device.file_name()?.to_str()?;
    let product = name.split(':').nth(2)?.split('.').next()?;
    u16::from_str_radix(product, 16).ok()
}

fn read_number(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `/sys/class/hwmon` with an NVMe sensor and an RM850i, whose
    /// `device` link points at its HID device, as in sysfs.
    fn fake_hwmon(name: &str) -> PathBuf {
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
        write("hwmon0/name", "nvme\n");
        write("hwmon4/name", "corsairpsu\n");
        write("hwmon4/power1_input", "114000000\n");
        write("hwmon4/temp1_input", "40250\n");
        write("hwmon4/temp1_crit", "70000\n");
        let hid = root.join("devices/0003:1B1C:1C0C.000C");
        std::fs::create_dir_all(&hid).expect("target/ is writable");
        std::os::unix::fs::symlink(&hid, root.join("hwmon4/device")).expect("target/ is writable");
        root
    }

    #[test]
    fn reads_an_rm850i_in_display_units() {
        let psu = CorsairPsu::find(&fake_hwmon("corsairpsu")).expect("the fake has one");
        let sample = psu.sample().expect("output is readable");
        assert_eq!(sample.model, Some("RM850i"));
        assert_eq!(sample.rating_w, Some(850.0));
        assert_eq!(sample.output_w, 114.0);
        assert_eq!(sample.temperature_c, Some(40.25));
        assert_eq!(sample.critical_c, Some(70.0));
    }
}
