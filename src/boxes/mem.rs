//! The `mem` box, laid out like btop's: memory readings on the left,
//! each a `label  amount` line over its history graph, and disks with
//! their I/O on the right.

use std::collections::HashMap;

use gpui::{Div, div, prelude::*, px, relative};

use crate::graph::{GraphStyle, Grow, graph, meter};
use crate::history::History;
use crate::sample::{DiskSample, MemorySample, Snapshot};
use crate::theme::{Gradient, Theme};
use crate::ui::{ROW_HEIGHT, bytes, chars, panel, scaled, share, short_bytes};

const HISTORY_LEN: usize = 600;
/// Samples each disk's I/O graph shows. The graphs are narrow, so fewer
/// samples than the network graphs keep single bursts readable.
const DISK_IO_WINDOW: usize = 60;
const DISK_IO_HEIGHT: f32 = 32.0;

pub struct MemBox {
    /// Indexed like [`MemoryStat::ALL`].
    readings: [History; MemoryStat::ALL.len()],
    /// Read and write rates per mount point, for the disks currently
    /// mounted.
    disk_io: HashMap<String, DiskIo>,
}

#[derive(Debug)]
struct DiskIo {
    read: History,
    write: History,
}

impl MemBox {
    pub fn new() -> Self {
        Self {
            readings: std::array::from_fn(|_| History::new(HISTORY_LEN)),
            disk_io: HashMap::new(),
        }
    }

    pub fn ingest(&mut self, snapshot: &Snapshot) {
        for (stat, history) in MemoryStat::ALL.iter().zip(&mut self.readings) {
            history.push(stat.share(snapshot.memory));
        }
        for disk in &snapshot.disks {
            let io = self
                .disk_io
                .entry(disk.mount_point.clone())
                .or_insert_with(|| DiskIo {
                    read: History::new(DISK_IO_WINDOW),
                    write: History::new(DISK_IO_WINDOW),
                });
            io.read.push(disk.read_per_sec as f32);
            io.write.push(disk.write_per_sec as f32);
        }
        self.disk_io.retain(|mount_point, _| {
            snapshot
                .disks
                .iter()
                .any(|disk| &disk.mount_point == mount_point)
        });
    }

    /// The readings share the box height evenly, so the graphs grow
    /// with the box.
    pub fn render(&self, snapshot: &Snapshot, theme: &Theme) -> Div {
        let memory = snapshot.memory;

        let readings = MemoryStat::ALL
            .iter()
            .zip(&self.readings)
            .filter(|(stat, _)| stat.applies(memory))
            .map(|(&stat, history)| {
                let fraction = stat.share(memory);
                let gradient = stat.gradient(theme);
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(div().text_color(theme.muted).child(stat.label()))
                            .child(stat.amount(memory)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h(px(ROW_HEIGHT - 4.0))
                            .gap_2()
                            .child(
                                div()
                                    .w(chars(4.0))
                                    .flex_shrink_0()
                                    .text_color(gradient.at(fraction))
                                    .child(format!("{:.0}%", fraction * 100.0)),
                            )
                            // Stacked graphs with no backdrop run into each
                            // other; the outline shows where each one starts.
                            .child(
                                div()
                                    .flex_1()
                                    .h_full()
                                    .border_1()
                                    .border_color(theme.faint)
                                    .rounded_sm()
                                    .child(graph(
                                        history.tail(HISTORY_LEN).collect(),
                                        gradient,
                                        GraphStyle::large(Grow::Up, theme.grid),
                                    )),
                            ),
                    )
            });

        let disks = snapshot
            .disks
            .iter()
            .map(|disk| render_disk(disk, self.disk_io.get(&disk.mount_point), theme));

        panel(
            theme,
            "mem",
            theme.mem_box,
            Some(format!("total {}", bytes(memory.total)).into()),
        )
        .child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .children(readings),
                )
                .child(
                    div()
                        .id("disks")
                        .flex()
                        .flex_col()
                        .w(relative(0.45))
                        .flex_shrink_0()
                        .gap_1()
                        .pl_2()
                        .border_l_1()
                        .border_color(theme.faint)
                        .overflow_y_scroll()
                        .child(div().text_color(theme.title).child("disks"))
                        .children(disks),
                ),
        )
    }
}

/// A disk as btop shows it: its size, its I/O as mirrored graphs with
/// reads growing up and writes down, and how full it is. The column is
/// narrow, so the meter's percentage stands in for the used amount.
fn render_disk(disk: &DiskSample, io: Option<&DiskIo>, theme: &Theme) -> Div {
    // Each direction scales to its own peak. The floor keeps a disk
    // that only sees the odd log write from looking busy.
    const FLOOR: f32 = 1_048_576.0;
    let (read, write) = io.map_or_else(Default::default, |io| {
        (
            scaled(&io.read, DISK_IO_WINDOW, FLOOR).0,
            scaled(&io.write, DISK_IO_WINDOW, FLOOR).0,
        )
    });
    let style = |grow| GraphStyle::small(grow).fitted(DISK_IO_WINDOW);
    let rate = |label: &str, value: f64, gradient: Gradient| {
        div()
            .flex()
            .justify_between()
            .child(div().text_color(gradient.at(0.9)).child(label.to_owned()))
            .child(format!("{}/s", short_bytes(value as u64)))
    };
    let fraction = share(disk.used, disk.total);
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .child(
            div()
                .flex()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted)
                        .child(disk.mount_point.clone()),
                )
                .child(div().flex_shrink_0().child(bytes(disk.total))),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .h(px(DISK_IO_HEIGHT))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .border_1()
                        .border_color(theme.faint)
                        .rounded_sm()
                        .child(
                            div()
                                .flex_1()
                                .child(graph(read, theme.download, style(Grow::Up))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .child(graph(write, theme.upload, style(Grow::Down))),
                        ),
                )
                .child(
                    div()
                        .w(chars(8.0))
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .child(rate("R", disk.read_per_sec, theme.download))
                        .child(rate("W", disk.write_per_sec, theme.upload)),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(ROW_HEIGHT - 4.0))
                .child(div().flex_1().h_full().child(meter(
                    fraction,
                    theme.used,
                    theme.meter_empty,
                )))
                .child(
                    div()
                        .w(chars(8.0))
                        .flex()
                        .justify_end()
                        .text_color(theme.used.at(fraction))
                        .child(format!("{:.0}%", fraction * 100.0)),
                ),
        )
}

/// A memory reading shown with a history graph.
#[derive(Debug, Clone, Copy)]
enum MemoryStat {
    Used,
    Available,
    Cached,
    Free,
    Swap,
}

impl MemoryStat {
    const ALL: [Self; 5] = [
        Self::Used,
        Self::Available,
        Self::Cached,
        Self::Free,
        Self::Swap,
    ];

    /// Cached needs `/proc/meminfo` and swap needs a swap device.
    fn applies(self, memory: MemorySample) -> bool {
        match self {
            Self::Cached => memory.cached.is_some(),
            Self::Swap => memory.swap_total > 0,
            Self::Used | Self::Available | Self::Free => true,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Used => "Used",
            Self::Available => "Available",
            Self::Cached => "Cached",
            Self::Free => "Free",
            Self::Swap => "Swap",
        }
    }

    /// Share of RAM, or of swap for [`MemoryStat::Swap`].
    fn share(self, memory: MemorySample) -> f32 {
        match self {
            Self::Used => share(memory.used, memory.total),
            Self::Available => share(memory.available, memory.total),
            Self::Cached => share(memory.cached.unwrap_or(0), memory.total),
            Self::Free => share(memory.free, memory.total),
            Self::Swap => share(memory.swap_used, memory.swap_total),
        }
    }

    fn amount(self, memory: MemorySample) -> String {
        match self {
            Self::Used => bytes(memory.used),
            Self::Available => bytes(memory.available),
            Self::Cached => bytes(memory.cached.unwrap_or(0)),
            Self::Free => bytes(memory.free),
            Self::Swap => format!("{} / {}", bytes(memory.swap_used), bytes(memory.swap_total)),
        }
    }

    fn gradient(self, theme: &Theme) -> Gradient {
        match self {
            Self::Used => theme.used,
            Self::Available => theme.available,
            Self::Cached => theme.cached,
            Self::Free => theme.free,
            // btop themes have no swap colors; btop draws swap as used.
            Self::Swap => theme.used,
        }
    }
}
