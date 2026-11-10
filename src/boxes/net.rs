//! The `net` box: download and upload as mirrored graphs with a legend.

use gpui::{Div, div, prelude::*, px};

use crate::graph::{GraphStyle, Grow, graph};
use crate::history::History;
use crate::sample::Snapshot;
use crate::theme::{Gradient, Theme};
use crate::ui::{bytes, chars, panel, scaled};

const HISTORY_LEN: usize = 600;
/// Samples the network graphs show, stretched to their width. The
/// graphs scale to the peak of what they show, so the window has to be
/// known up front for the "Top" label to name that same peak.
const NETWORK_WINDOW: usize = 120;
/// Keeps an idle link from blowing a few bytes of background chatter up
/// to full height.
const FLOOR: f32 = 10_240.0;

pub struct NetBox {
    rx: History,
    tx: History,
}

impl NetBox {
    pub fn new() -> Self {
        Self {
            rx: History::new(HISTORY_LEN),
            tx: History::new(HISTORY_LEN),
        }
    }

    pub fn ingest(&mut self, snapshot: &Snapshot) {
        self.rx.push(snapshot.network.rx_per_sec as f32);
        self.tx.push(snapshot.network.tx_per_sec as f32);
    }

    pub fn render(&self, snapshot: &Snapshot, theme: &Theme) -> Div {
        let network = snapshot.network;
        let (rx, rx_peak) = scaled(&self.rx, NETWORK_WINDOW, FLOOR);
        let (tx, tx_peak) = scaled(&self.tx, NETWORK_WINDOW, FLOOR);

        let legend =
            |arrow: &str, label: &str, rate: f64, peak: f32, total: u64, gradient: Gradient| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(
                                div()
                                    .text_color(gradient.at(0.9))
                                    .child(format!("{arrow} {label}")),
                            )
                            .child(format!("{}/s", bytes(rate as u64))),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_color(theme.muted)
                            .child("Top")
                            .child(format!("{}/s", bytes(peak as u64))),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_color(theme.muted)
                            .child("Total")
                            .child(bytes(total)),
                    )
            };

        panel(theme, "net", theme.net_box, None).child(
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
                        .child(div().flex_1().child(graph(
                            rx,
                            theme.download,
                            GraphStyle::large(Grow::Up, theme.grid).fitted(NETWORK_WINDOW),
                        )))
                        .child(div().h(px(1.0)).bg(theme.faint))
                        .child(div().flex_1().child(graph(
                            tx,
                            theme.upload,
                            GraphStyle::large(Grow::Down, theme.grid).fitted(NETWORK_WINDOW),
                        ))),
                )
                .child(
                    div()
                        .w(chars(22.0))
                        .flex_shrink_0()
                        .flex()
                        .flex_col()
                        .justify_between()
                        .child(legend(
                            "▼",
                            "download",
                            network.rx_per_sec,
                            rx_peak,
                            network.rx_total,
                            theme.download,
                        ))
                        .child(legend(
                            "▲",
                            "upload",
                            network.tx_per_sec,
                            tx_peak,
                            network.tx_total,
                            theme.upload,
                        )),
                ),
        )
    }
}
