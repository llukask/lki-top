//! The `cpu` box: total usage history, a per-core grid with
//! sparklines, load average and one line per GPU.

use gpui::{Div, div, prelude::*, px};

use crate::gpu::GpuSample;
use crate::graph::{GraphStyle, Grow, graph, meter};
use crate::history::History;
use crate::psu::PsuSample;
use crate::rates::Rates;
use crate::sample::Snapshot;
use crate::theme::Theme;
use crate::ui::{ROW_HEIGHT, chars, duration, panel, share, short_bytes};

/// Ten minutes at one sample a second, wider than any graph on a 4K
/// screen at 3 px per sample.
const HISTORY_LEN: usize = 600;
/// Per-core sparklines are about 60 px wide at 2 px per sample.
const CORE_HISTORY_LEN: usize = 32;
/// Cores fill columns of at most eight, like btop's core grid.
const CORES_PER_COLUMN: usize = 8;

pub struct CpuBox {
    total: History,
    /// One per logical core, created with the first snapshot.
    cores: Vec<History>,
}

impl CpuBox {
    pub fn new() -> Self {
        Self {
            total: History::new(HISTORY_LEN),
            cores: Vec::new(),
        }
    }

    pub fn ingest(&mut self, snapshot: &Snapshot) {
        self.total.push(snapshot.cpu.total);
        let cores = &snapshot.cpu.per_core;
        self.cores
            .resize_with(cores.len(), || History::new(CORE_HISTORY_LEN));
        for (history, &usage) in self.cores.iter_mut().zip(cores) {
            history.push(usage);
        }
    }

    pub fn render(&self, snapshot: &Snapshot, rates: Rates, theme: &Theme) -> Div {
        let cpu = &snapshot.cpu;
        let clock = jiff::Zoned::now().strftime("%H:%M:%S").to_string();
        let right = format!(
            "up {} · {clock} · procs {} ms · graphs {} ms",
            duration(snapshot.uptime_secs),
            rates.process_interval().as_millis(),
            rates.graph_interval().as_millis(),
        );

        let core_columns =
            cpu.per_core
                .chunks(CORES_PER_COLUMN)
                .enumerate()
                .map(|(column, cores)| {
                    div()
                        .flex()
                        .flex_col()
                        .children(cores.iter().enumerate().map(move |(row, &usage)| {
                            let index = column * CORES_PER_COLUMN + row;
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .h(px(ROW_HEIGHT - 2.0))
                                .child(
                                    div()
                                        .w(chars(3.0))
                                        .text_color(theme.muted)
                                        .child(format!("C{index}")),
                                )
                                .child(div().w(px(62.0)).h(px(12.0)).child(graph(
                                    self.cores[index].tail(CORE_HISTORY_LEN).collect(),
                                    theme.cpu,
                                    GraphStyle::sparkline(theme.meter_empty),
                                )))
                                .child(
                                    div()
                                        .w(chars(4.0))
                                        .flex()
                                        .justify_end()
                                        .text_color(theme.cpu.at(usage))
                                        .child(format!("{:.0}%", usage * 100.0)),
                                )
                        }))
                });

        let temperature = cpu.temperature_c.map(|celsius| {
            div()
                .text_color(theme.temperature.at(celsius / 100.0))
                .child(format!("{celsius:.0}°C"))
        });

        let cores_box = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .gap_1()
            .px_2()
            .py_1()
            .border_1()
            .border_color(theme.faint)
            .rounded_md()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_color(theme.title)
                            .truncate()
                            .child(cpu.brand.clone()),
                    )
                    .child(format!("{:.1} GHz", cpu.frequency_mhz as f32 / 1000.0)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(ROW_HEIGHT))
                    .child(div().text_color(theme.muted).child("CPU"))
                    .child(div().flex_1().h_full().child(meter(
                        cpu.total,
                        theme.cpu,
                        theme.meter_empty,
                    )))
                    .child(
                        div()
                            .text_color(theme.cpu.at(cpu.total))
                            .child(format!("{:>3.0}%", cpu.total * 100.0)),
                    )
                    .children(temperature),
            )
            .child(div().flex().gap_3().children(core_columns))
            .child(div().text_color(theme.muted).child(format!(
                "Load avg {:.2} {:.2} {:.2}",
                snapshot.load_average[0], snapshot.load_average[1], snapshot.load_average[2]
            )))
            .children(gpu_lines(&snapshot.gpus, theme))
            .children(snapshot.psu.as_ref().map(|psu| psu_line(psu, theme)));

        panel(theme, "cpu", theme.cpu_box, Some(right.into())).child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .gap_2()
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .h_full()
                        .child(graph(
                            self.total.tail(HISTORY_LEN).collect(),
                            theme.cpu,
                            GraphStyle::large(Grow::Up, theme.grid),
                        ))
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .text_color(theme.cpu.at(cpu.total))
                                .child(format!("{:.0}%", cpu.total * 100.0)),
                        ),
                )
                .child(cores_box),
        )
    }
}

/// The power supply's output, under the GPUs: its model, a load meter
/// against its rating when the model is known, the watts, and its
/// temperature. "out" because the unit draws more from the wall.
fn psu_line(psu: &PsuSample, theme: &Theme) -> Div {
    let load = psu.rating_w.map(|rating| psu.output_w / rating);
    let temperature = psu.temperature_c.map(|celsius| {
        let level = celsius / psu.critical_c.unwrap_or(100.0);
        div()
            .text_color(theme.temperature.at(level))
            .child(format!("{celsius:.0}°C"))
    });
    div()
        .flex()
        .items_center()
        .gap_2()
        .h(px(ROW_HEIGHT))
        .child(
            div()
                .text_color(theme.muted)
                .child(psu.model.unwrap_or("PSU")),
        )
        .child(
            div()
                .flex_1()
                .h_full()
                .children(load.map(|load| meter(load, theme.cached, theme.meter_empty))),
        )
        .child(
            div()
                .text_color(theme.cached.at(load.unwrap_or(0.0)))
                .child(format!("{:.0}W out", psu.output_w)),
        )
        .children(temperature)
}

/// One line per GPU under the cores, like btop's: usage, VRAM,
/// temperature and power. Numbered only when there is more than one.
fn gpu_lines<'a>(gpus: &'a [GpuSample], theme: &'a Theme) -> impl Iterator<Item = Div> + 'a {
    gpus.iter().enumerate().map(move |(index, gpu)| {
        let label = if gpus.len() > 1 {
            format!("GPU{index}")
        } else {
            "GPU".to_owned()
        };
        let vram = share(gpu.vram_used, gpu.vram_total);
        let temperature = gpu.temperature_c.map(|celsius| {
            let level = celsius / gpu.critical_c.unwrap_or(100.0);
            div()
                .text_color(theme.temperature.at(level))
                .child(format!("{celsius:.0}°C"))
        });
        let power = gpu.power_w.map(|watts| {
            let level = gpu.power_cap_w.map_or(0.0, |cap| watts / cap);
            div()
                .text_color(theme.cached.at(level))
                .child(format!("{watts:.0}W"))
        });
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(ROW_HEIGHT))
            .child(div().text_color(theme.muted).child(label))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .child(meter(gpu.usage, theme.cpu, theme.meter_empty)),
            )
            .child(
                div()
                    .text_color(theme.cpu.at(gpu.usage))
                    .child(format!("{:>3.0}%", gpu.usage * 100.0)),
            )
            .child(
                div()
                    .w(px(34.0))
                    .h(px(12.0))
                    .child(meter(vram, theme.used, theme.meter_empty)),
            )
            .child(format!(
                "{}/{}",
                short_bytes(gpu.vram_used),
                short_bytes(gpu.vram_total)
            ))
            .children(temperature)
            .children(power)
    })
}
