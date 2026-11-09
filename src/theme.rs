//! Colors, loosely after btop's default theme: a black background, a
//! muted border color per box, and three-stop gradients that run from
//! calm at 0% to alarming at 100%.

use gpui::{App, Hsla, Rgba, SharedString, rgb};

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
    pub(crate) const fn new(low: u32, mid: u32, high: u32) -> Self {
        Self {
            stops: [hex(low), hex(mid), hex(high)],
        }
    }

    /// Color at `t` in 0..=1, linear in RGB between the stops.
    pub fn at(&self, t: f32) -> Hsla {
        let t = t.clamp(0.0, 1.0) * 2.0;
        let (from, to, t) = if t <= 1.0 {
            (self.stops[0], self.stops[1], t)
        } else {
            (self.stops[1], self.stops[2], t - 1.0)
        };
        let mix = |a: f32, b: f32| a + (b - a) * t;
        Rgba {
            r: mix(from.r, to.r),
            g: mix(from.g, to.g),
            b: mix(from.b, to.b),
            a: 1.0,
        }
        .into()
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

/// `gpui::rgb` isn't `const`, and the gradients are.
const fn hex(value: u32) -> Rgba {
    Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.0,
        g: ((value >> 8) & 0xff) as f32 / 255.0,
        b: (value & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub background: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub title: Hsla,
    pub highlight: Hsla,
    pub selected_bg: Hsla,
    pub meter_empty: Hsla,
    pub grid: Hsla,
    pub cpu_box: Hsla,
    pub mem_box: Hsla,
    pub net_box: Hsla,
    pub proc_box: Hsla,
    pub cpu: Gradient,
    pub temperature: Gradient,
    pub used: Gradient,
    pub available: Gradient,
    pub free: Gradient,
    pub swap: Gradient,
    pub download: Gradient,
    pub upload: Gradient,
    pub process: Gradient,
    pub mono: SharedString,
}

impl Theme {
    pub fn new(cx: &App) -> Self {
        Self {
            background: rgb(0x0b0b0b).into(),
            text: rgb(0xcccccc).into(),
            muted: rgb(0x8a8a8a).into(),
            faint: rgb(0x404040).into(),
            title: rgb(0xeeeeee).into(),
            highlight: rgb(0xb54040).into(),
            selected_bg: rgb(0x6a2f2f).into(),
            meter_empty: rgb(0x262626).into(),
            grid: rgb(0x1c1c1c).into(),
            cpu_box: rgb(0x556d59).into(),
            mem_box: rgb(0x6c6c4b).into(),
            net_box: rgb(0x5c588d).into(),
            proc_box: rgb(0x805252).into(),
            cpu: Gradient::new(0x77ca9b, 0xcbc06c, 0xdc4c4c),
            temperature: Gradient::new(0x4897d4, 0x5474e8, 0xff40b6),
            used: Gradient::new(0x592b26, 0xd9626d, 0xff4769),
            available: Gradient::new(0x4e5900, 0xc4b754, 0xfff066),
            free: Gradient::new(0x384f21, 0x88c66b, 0xb5e685),
            swap: Gradient::new(0x5a2b4a, 0xa95e8f, 0xe68ac4),
            download: Gradient::new(0x291f75, 0x4f43a3, 0xb0a9de),
            upload: Gradient::new(0x620665, 0x7d4180, 0xdcafde),
            process: Gradient::new(0x80d0a3, 0xdcd179, 0xd45454),
            mono: pick_mono_font(cx),
        }
    }
}

/// gpui's `.ZedMono` alias maps to Lilex, which most systems lack, and a
/// missing family renders as nothing useful. Pick the first installed one.
fn pick_mono_font(cx: &App) -> SharedString {
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
