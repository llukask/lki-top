//! Colors, read from btop `.theme` files. btop's default theme and the
//! Catppuccin flavors are built in; more themes come from the same
//! directories btop reads, so any btop theme works unchanged.
//!
//! A theme file is lines of `theme[key]="value"`. A value is `#rrggbb`,
//! `#gg` for a grey, or three decimals `r g b`. Missing keys fall back
//! the way btop does it: mostly to the default theme, but a few to
//! other keys of the same file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use gpui::{App, Global, Hsla, Rgba, SharedString};

/// One linear stretch of a [`Gradient`]: levels `from..=to`, with the
/// colors at both ends.
#[derive(Debug, Clone, Copy)]
pub struct GradientPiece {
    pub from: f32,
    pub to: f32,
    pub from_color: Hsla,
    pub to_color: Hsla,
}

#[derive(Debug, Clone, Copy)]
pub struct Gradient {
    stops: [Rgba; 3],
}

impl Gradient {
    /// btop's rules: no end color means one solid color, and no middle
    /// color means a straight blend from start to end.
    fn new(start: Rgba, mid: Option<Rgba>, end: Option<Rgba>) -> Self {
        let Some(end) = end else {
            return Self { stops: [start; 3] };
        };
        let mid = mid.unwrap_or_else(|| mix(start, end, 0.5));
        Self {
            stops: [start, mid, end],
        }
    }

    #[cfg(test)]
    pub(crate) fn from_hex(low: u32, mid: u32, high: u32) -> Self {
        let rgb = gpui::rgb;
        Self::new(rgb(low), Some(rgb(mid)), Some(rgb(high)))
    }

    /// Color at `t` in 0..=1, linear in RGB between the stops.
    pub fn at(&self, t: f32) -> Hsla {
        let t = t.clamp(0.0, 1.0) * 2.0;
        let (from, to, t) = if t <= 1.0 {
            (self.stops[0], self.stops[1], t)
        } else {
            (self.stops[1], self.stops[2], t - 1.0)
        };
        mix(from, to, t).into()
    }

    /// The gradient's linear pieces between stops. gpui gradients have
    /// two stops, so painting each piece as its own shape is how a
    /// three-stop gradient renders exactly like [`Gradient::at`].
    pub fn pieces(&self) -> [GradientPiece; 2] {
        let color = |index: usize| -> Hsla { self.stops[index].into() };
        [
            GradientPiece {
                from: 0.0,
                to: 0.5,
                from_color: color(0),
                to_color: color(1),
            },
            GradientPiece {
                from: 0.5,
                to: 1.0,
                from_color: color(1),
                to_color: color(2),
            },
        ]
    }
}

fn mix(from: Rgba, to: Rgba, t: f32) -> Rgba {
    let lerp = |a: f32, b: f32| a + (b - a) * t;
    Rgba {
        r: lerp(from.r, to.r),
        g: lerp(from.g, to.g),
        b: lerp(from.b, to.b),
        a: 1.0,
    }
}

const DEFAULT_NAME: &str = "Default";
const DEFAULT_THEME: &str = include_str!("../themes/btop_default.theme");
const BUILT_IN: [(&str, &str); 5] = [
    (DEFAULT_NAME, DEFAULT_THEME),
    (
        "catppuccin_latte",
        include_str!("../themes/catppuccin_latte.theme"),
    ),
    (
        "catppuccin_frappe",
        include_str!("../themes/catppuccin_frappe.theme"),
    ),
    (
        "catppuccin_macchiato",
        include_str!("../themes/catppuccin_macchiato.theme"),
    ),
    (
        "catppuccin_mocha",
        include_str!("../themes/catppuccin_mocha.theme"),
    ),
];

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: SharedString,
    pub background: Hsla,
    pub text: Hsla,
    /// Labels and secondary text. btop themes have no color for this, so
    /// it sits between the text and background colors, which keeps it
    /// readable in light and dark themes alike.
    pub muted: Hsla,
    pub faint: Hsla,
    pub title: Hsla,
    pub highlight: Hsla,
    pub selected_bg: Hsla,
    pub selected_fg: Hsla,
    pub meter_empty: Hsla,
    /// Graph grid lines, halfway between the background and dividers.
    pub grid: Hsla,
    pub cpu_box: Hsla,
    pub mem_box: Hsla,
    pub net_box: Hsla,
    pub proc_box: Hsla,
    pub cpu: Gradient,
    pub temperature: Gradient,
    pub used: Gradient,
    pub available: Gradient,
    pub cached: Gradient,
    pub free: Gradient,
    pub download: Gradient,
    pub upload: Gradient,
    pub process: Gradient,
}

/// The theme in use, read by every box when it renders.
impl Global for Theme {}

impl Theme {
    #[cfg(test)]
    fn default_theme() -> Self {
        Self::parse(DEFAULT_NAME, DEFAULT_THEME)
    }

    pub fn parse(name: impl Into<SharedString>, text: &str) -> Self {
        let file = entries(text);
        let defaults = entries(DEFAULT_THEME);
        let default_color = |key: &str| {
            defaults
                .get(key)
                .and_then(|value| parse_color(value))
                .expect("the built-in default theme defines every key")
        };
        // `None` only for a middle or end gradient color the file leaves
        // empty on purpose; anything else unusable falls back.
        let lookup = |key: &str| -> Option<Rgba> {
            match file.get(key) {
                Some(value)
                    if value.is_empty() && (key.ends_with("_mid") || key.ends_with("_end")) =>
                {
                    None
                }
                Some(value) => Some(parse_color(value).unwrap_or_else(|| default_color(key))),
                None => Some(default_color(key)),
            }
        };
        let color = |key: &str| lookup(key).unwrap_or_else(|| default_color(key));
        let gradient = |prefix: &str| {
            Gradient::new(
                color(&format!("{prefix}_start")),
                lookup(&format!("{prefix}_mid")),
                lookup(&format!("{prefix}_end")),
            )
        };

        let background = color("main_bg");
        let text = color("main_fg");
        let divider = color("div_line");
        // Optional keys that btop fills from the same theme, not from
        // the default one.
        let meter_empty = if file.contains_key("meter_bg") {
            color("meter_bg")
        } else {
            color("inactive_fg")
        };
        let cpu = gradient("cpu");
        let process = if file.contains_key("process_start") {
            gradient("process")
        } else {
            cpu
        };

        Self {
            name: name.into(),
            background: background.into(),
            text: text.into(),
            muted: mix(text, background, 0.35).into(),
            faint: divider.into(),
            title: color("title").into(),
            highlight: color("hi_fg").into(),
            selected_bg: color("selected_bg").into(),
            selected_fg: color("selected_fg").into(),
            meter_empty: meter_empty.into(),
            grid: mix(background, divider, 0.5).into(),
            cpu_box: color("cpu_box").into(),
            mem_box: color("mem_box").into(),
            net_box: color("net_box").into(),
            proc_box: color("proc_box").into(),
            cpu,
            temperature: gradient("temp"),
            used: gradient("used"),
            available: gradient("available"),
            cached: gradient("cached"),
            free: gradient("free"),
            download: gradient("download"),
            upload: gradient("upload"),
            process,
        }
    }
}

/// `key → value` for each `theme[key]="value"` line. Values may be
/// unquoted, and anything after the closing quote is a comment.
fn entries(text: &str) -> HashMap<&str, &str> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("theme[")?;
            let (key, rest) = rest.split_once(']')?;
            let value = rest.trim_start().strip_prefix('=')?.trim_start();
            let value = match value.strip_prefix('"') {
                Some(quoted) => quoted.split_once('"').map_or(quoted, |(inside, _)| inside),
                None => value,
            };
            Some((key, value.trim()))
        })
        .collect()
}

fn parse_color(value: &str) -> Option<Rgba> {
    let channel = |byte: u8| f32::from(byte) / 255.0;
    if let Some(hex) = value.strip_prefix('#') {
        let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
        let (r, g, b) = match hex.len() {
            2 => {
                let grey = byte(0)?;
                (grey, grey, grey)
            }
            6 => (byte(0)?, byte(2)?, byte(4)?),
            _ => return None,
        };
        return Some(Rgba {
            r: channel(r),
            g: channel(g),
            b: channel(b),
            a: 1.0,
        });
    }
    let decimals: Vec<u8> = value
        .split_whitespace()
        .map(|part| part.parse().ok())
        .collect::<Option<_>>()?;
    let [r, g, b] = decimals[..] else {
        return None;
    };
    Some(Rgba {
        r: channel(r),
        g: channel(g),
        b: channel(b),
        a: 1.0,
    })
}

/// The built-in themes, then every `.theme` file from the directories
/// btop reads, each group sorted by name. A file named like a built-in
/// theme replaces it, so a user can adjust one.
pub fn load_all() -> Vec<Theme> {
    let mut themes: Vec<Theme> = BUILT_IN
        .iter()
        .map(|(name, text)| Theme::parse(*name, text))
        .collect();
    let mut found: Vec<(String, PathBuf)> = theme_dirs()
        .iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "theme" {
                return None;
            }
            let name = path.file_stem()?.to_str()?.to_owned();
            Some((name, path))
        })
        .collect();
    found.sort();
    for (name, path) in found {
        // An unreadable file is skipped like a missing one; there is no
        // place to report it before the window is up.
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let theme = Theme::parse(name, &text);
        match themes.iter_mut().find(|known| known.name == theme.name) {
            Some(known) => *known = theme,
            None => themes.push(theme),
        }
    }
    themes
}

/// Where btop looks, lowest priority first: the system data dirs, then
/// btop's user config, then lki-top's own.
fn theme_dirs() -> Vec<PathBuf> {
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    let mut dirs: Vec<PathBuf> = data_dirs
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(|dir| Path::new(dir).join("btop/themes"))
        .collect();
    if let Some(config) = config_home() {
        dirs.push(config.join("btop/themes"));
        dirs.push(config.join("lki-top/themes"));
    }
    dirs
}

pub fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
}

/// gpui's `.ZedMono` alias maps to Lilex, which most systems lack, and a
/// missing family renders as nothing useful. Pick the first installed one.
pub fn pick_mono_font(cx: &App) -> SharedString {
    const MONO_FONTS: &[&str] = &[
        "JetBrains Mono",
        "Fira Code",
        "Iosevka",
        "Noto Sans Mono",
        "DejaVu Sans Mono",
        "Liberation Mono",
        "Ubuntu Mono",
    ];
    let installed = cx.text_system().all_font_names();
    MONO_FONTS
        .iter()
        .find(|name| installed.iter().any(|have| have == *name))
        .copied()
        .unwrap_or(".ZedMono")
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        }
    }

    #[test]
    fn reads_btop_color_notations() {
        assert_eq!(parse_color("#1e66f5"), Some(rgb(0x1e, 0x66, 0xf5)));
        assert_eq!(parse_color("#cc"), Some(rgb(0xcc, 0xcc, 0xcc)));
        assert_eq!(parse_color("30 102 245"), Some(rgb(30, 102, 245)));
        assert_eq!(parse_color(""), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("1 2"), None);
    }

    #[test]
    fn reads_quoted_unquoted_and_commented_lines() {
        let text = "# comment\n\
                    theme[main_bg]=\"#1e1e2e\"\n\
                    theme[cpu_box]=\"#cba6f7\" #Mauve\n\
                    theme[title] = #cdd6f4\n\
                    theme[used_mid]=\"\"\n";
        let entries = entries(text);
        assert_eq!(entries["main_bg"], "#1e1e2e");
        assert_eq!(entries["cpu_box"], "#cba6f7");
        assert_eq!(entries["title"], "#cdd6f4");
        assert_eq!(entries["used_mid"], "");
        assert_eq!(entries.len(), 4);
    }

    #[test]
    fn every_built_in_theme_sets_its_own_background() {
        let default = Theme::default_theme();
        for (name, text) in BUILT_IN.iter().skip(1) {
            let theme = Theme::parse(*name, text);
            assert_ne!(theme.background, default.background, "{name}");
        }
    }

    #[test]
    fn missing_keys_fall_back_like_btop() {
        let default = Theme::default_theme();
        let theme = Theme::parse(
            "partial",
            "theme[main_bg]=\"#ffffff\"\n\
             theme[inactive_fg]=\"#123456\"\n\
             theme[cpu_start]=\"#000000\"\n",
        );
        // Missing keys come from the default theme...
        assert_eq!(theme.title, default.title);
        assert_eq!(theme.cpu.at(1.0), default.cpu.at(1.0));
        // ...but the meter track follows this theme's inactive color,
        // and processes follow its cpu colors.
        assert_eq!(theme.meter_empty, Hsla::from(rgb(0x12, 0x34, 0x56)));
        assert_eq!(theme.process.at(0.0), Hsla::from(rgb(0, 0, 0)));
    }

    #[test]
    fn empty_gradient_ends_mean_fewer_stops() {
        let solid = Theme::parse(
            "solid",
            "theme[used_start]=\"#ff0000\"\ntheme[used_mid]=\"\"\ntheme[used_end]=\"\"\n",
        );
        assert_eq!(solid.used.at(1.0), Hsla::from(rgb(255, 0, 0)));

        let blend = Theme::parse(
            "blend",
            "theme[used_start]=\"#000000\"\ntheme[used_mid]=\"\"\ntheme[used_end]=\"#ffffff\"\n",
        );
        let middle = blend.used.at(0.5).to_rgb();
        assert!((middle.r - 0.5).abs() < 1e-3, "{middle:?}");
    }
}
