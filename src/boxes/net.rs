//! The `net` box: download and upload as mirrored graphs with a legend,
//! for the physical interfaces together or for one interface.

use std::collections::HashMap;

use gpui::{Div, div, prelude::*, px};

use crate::graph::{GraphStyle, Grow, graph};
use crate::history::History;
use crate::sample::{Snapshot, Traffic};
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

/// Which traffic the box shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NetView {
    /// Every interface with network hardware behind it. Bridges, tunnels
    /// and container veths are left out, since their traffic crosses a
    /// physical interface too and would count twice.
    Physical,
    Interface(String),
}

impl NetView {
    pub fn label(&self) -> &str {
        match self {
            NetView::Physical => "physical",
            NetView::Interface(name) => name,
        }
    }

    /// The traffic this view covers in `snapshot`, or `None` for an
    /// interface that isn't there.
    fn traffic(&self, snapshot: &Snapshot) -> Option<Traffic> {
        match self {
            NetView::Physical => Some(
                snapshot
                    .interfaces
                    .iter()
                    .filter(|interface| interface.physical)
                    .map(|interface| interface.traffic)
                    .fold(Traffic::default(), |sum, traffic| sum + traffic),
            ),
            NetView::Interface(name) => snapshot
                .interfaces
                .iter()
                .find(|interface| &interface.name == name)
                .map(|interface| interface.traffic),
        }
    }
}

#[derive(Debug)]
struct Histories {
    rx: History,
    tx: History,
}

pub struct NetBox {
    /// What the user picked. An interface that has gone away, such as a
    /// container's veth, stays picked, and the box shows the physical
    /// traffic until it is back.
    chosen: NetView,
    /// Every view of the latest sample keeps its history, so switching
    /// shows the past right away.
    histories: HashMap<NetView, Histories>,
    /// The order `n` and `b` step through: physical, then each interface
    /// by name.
    views: Vec<NetView>,
}

impl NetBox {
    pub fn new(chosen: NetView) -> Self {
        Self {
            chosen,
            histories: HashMap::new(),
            views: vec![NetView::Physical],
        }
    }

    pub fn chosen(&self) -> &NetView {
        &self.chosen
    }

    fn shown(&self) -> &NetView {
        if self.views.contains(&self.chosen) {
            &self.chosen
        } else {
            &NetView::Physical
        }
    }

    pub fn step(&mut self, delta: isize) {
        let current = self
            .views
            .iter()
            .position(|view| view == self.shown())
            .unwrap_or(0) as isize;
        let count = self.views.len() as isize;
        self.chosen = self.views[(current + delta).rem_euclid(count) as usize].clone();
    }

    pub fn ingest(&mut self, snapshot: &Snapshot) {
        self.views = std::iter::once(NetView::Physical)
            .chain(
                snapshot
                    .interfaces
                    .iter()
                    .map(|interface| NetView::Interface(interface.name.clone())),
            )
            .collect();
        self.histories.retain(|view, _| self.views.contains(view));
        for view in &self.views {
            let traffic = view
                .traffic(snapshot)
                .expect("views come from this snapshot");
            let histories = self
                .histories
                .entry(view.clone())
                .or_insert_with(|| Histories {
                    rx: History::new(HISTORY_LEN),
                    tx: History::new(HISTORY_LEN),
                });
            histories.rx.push(traffic.rx_per_sec as f32);
            histories.tx.push(traffic.tx_per_sec as f32);
        }
    }

    pub fn render(&self, snapshot: &Snapshot, theme: &Theme) -> Div {
        let shown = self.shown();
        let traffic = shown.traffic(snapshot).unwrap_or_default();
        let (rx, rx_peak, tx, tx_peak) = match self.histories.get(shown) {
            Some(histories) => {
                let (rx, rx_peak) = scaled(&histories.rx, NETWORK_WINDOW, FLOOR);
                let (tx, tx_peak) = scaled(&histories.tx, NETWORK_WINDOW, FLOOR);
                (rx, rx_peak, tx, tx_peak)
            }
            None => (Vec::new(), FLOOR, Vec::new(), FLOOR),
        };

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

        panel(
            theme,
            "net",
            theme.net_box,
            Some(format!("{} · b n", shown.label()).into()),
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
                            traffic.rx_per_sec,
                            rx_peak,
                            traffic.rx_total,
                            theme.download,
                        ))
                        .child(legend(
                            "▲",
                            "upload",
                            traffic.tx_per_sec,
                            tx_peak,
                            traffic.tx_total,
                            theme.upload,
                        )),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::{CpuSample, InterfaceSample, MemorySample};

    fn snapshot(interfaces: &[(&str, bool, f64)]) -> Snapshot {
        Snapshot {
            cpu: CpuSample {
                brand: String::new(),
                total: 0.0,
                per_core: Vec::new(),
                frequency_mhz: 0,
                temperature_c: None,
            },
            memory: MemorySample {
                total: 0,
                used: 0,
                available: 0,
                free: 0,
                cached: None,
                swap_total: 0,
                swap_used: 0,
            },
            interfaces: interfaces
                .iter()
                .map(|&(name, physical, rx)| InterfaceSample {
                    name: name.into(),
                    physical,
                    traffic: Traffic {
                        rx_per_sec: rx,
                        ..Traffic::default()
                    },
                })
                .collect(),
            disks: Vec::new(),
            gpus: Vec::new(),
            load_average: [0.0; 3],
            uptime_secs: 0,
        }
    }

    #[test]
    fn physical_traffic_leaves_out_bridges_and_tunnels() {
        let sample = snapshot(&[
            ("enp7s0", true, 100.0),
            ("wg0", false, 40.0),
            ("wlan0", true, 5.0),
        ]);
        let traffic = NetView::Physical.traffic(&sample).expect("always there");
        assert_eq!(traffic.rx_per_sec, 105.0);
    }

    #[test]
    fn stepping_cycles_through_physical_then_each_interface() {
        let mut net = NetBox::new(NetView::Physical);
        net.ingest(&snapshot(&[("enp7s0", true, 0.0), ("wg0", false, 0.0)]));
        let mut seen = Vec::new();
        for _ in 0..3 {
            net.step(1);
            seen.push(net.chosen().label().to_owned());
        }
        assert_eq!(seen, ["enp7s0", "wg0", "physical"]);
        net.step(-1);
        assert_eq!(net.chosen().label(), "wg0");
    }

    #[test]
    fn a_vanished_interface_stays_chosen_but_shows_physical() {
        let mut net = NetBox::new(NetView::Interface("veth1".into()));
        net.ingest(&snapshot(&[("enp7s0", true, 0.0)]));
        assert_eq!(net.shown(), &NetView::Physical);
        assert_eq!(net.chosen(), &NetView::Interface("veth1".into()));
        net.ingest(&snapshot(&[("enp7s0", true, 0.0), ("veth1", false, 0.0)]));
        assert_eq!(net.shown(), &NetView::Interface("veth1".into()));
    }
}
