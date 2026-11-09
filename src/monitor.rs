//! The window's one view: four btop-style boxes over the latest
//! [`Snapshot`], fed by a sampling loop on the background executor.

use std::ops::Range;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::{self, Either};

use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FocusHandle, Focusable, Hsla, KeyBinding,
    KeyDownEvent, ScrollStrategy, SharedString, UniformListScrollHandle, Window, actions, div,
    prelude::*, px, relative, uniform_list,
};

use crate::graph::{GraphStyle, Grow, graph, meter};
use crate::history::History;
use crate::procs::{Fold, Layout, ProcessTable, SortColumn, SortOrder};
use crate::sample::{Sampler, Snapshot};
use crate::theme::{Gradient, Theme};

/// Sampling intervals that `+` and `-` step through. The shortest stays
/// above sysinfo's 200 ms minimum between CPU readings.
const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 3000, 5000];
const DEFAULT_INTERVAL: usize = 2;
/// Ten minutes at one sample a second, wider than any graph on a 4K
/// screen at 3 px per sample.
const HISTORY_LEN: usize = 600;
/// Per-core sparklines are about 60 px wide at 2 px per sample.
const CORE_HISTORY_LEN: usize = 32;
/// Samples the network graphs show, stretched to their width. The
/// graphs scale to the peak of what they show, so the window has to be
/// known up front for the "Top" label to name that same peak.
const NETWORK_WINDOW: usize = 120;
const ROW_HEIGHT: f32 = 18.0;
const PAGE_ROWS: usize = 20;
/// Approximate advance of one monospace character at `text_xs`. Column
/// widths are given in characters so they line up with btop's grid.
const CH: f32 = 7.4;

fn chars(count: f32) -> gpui::Pixels {
    px(count * CH)
}

actions!(
    lki_top,
    [
        SelectNext,
        SelectPrevious,
        PageDown,
        PageUp,
        SelectFirst,
        SelectLast,
        SortNext,
        SortPrevious,
        ReverseSort,
        StartFilter,
        ClearFilter,
        CommitFilter,
        Terminate,
        Kill,
        Confirm,
        Cancel,
        LongerInterval,
        ShorterInterval,
        ToggleTree,
        ToggleFold,
        Quit,
    ]
);

/// What keys do right now. Each mode is its own key context, so a
/// binding like `k` (select previous) can't fire while typing a filter.
#[derive(Debug, Clone)]
enum Mode {
    Browse,
    Filter,
    Confirm(PendingSignal),
}

impl Mode {
    fn key_context(&self) -> &'static str {
        match self {
            Mode::Browse => "Browse",
            Mode::Filter => "Filter",
            Mode::Confirm(_) => "Confirm",
        }
    }
}

/// A signal the user asked for but hasn't confirmed yet. Holds the name
/// as it was when asked, so the prompt stays readable if the process
/// exits before the answer.
#[derive(Debug, Clone)]
struct PendingSignal {
    pid: u32,
    name: String,
    signal: Signal,
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

impl Signal {
    fn name(self) -> &'static str {
        match self {
            Signal::Term => "SIGTERM",
            Signal::Kill => "SIGKILL",
        }
    }

    fn send(self, pid: u32) -> std::io::Result<()> {
        let pid = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| std::io::Error::other(format!("{pid} is not a valid pid")))?;
        let signal = match self {
            Signal::Term => rustix::process::Signal::TERM,
            Signal::Kill => rustix::process::Signal::KILL,
        };
        rustix::process::kill_process(pid, signal)?;
        Ok(())
    }
}

pub fn bind_keys(cx: &mut App) {
    let browse = Some("Browse");
    let filter = Some("Filter");
    let confirm = Some("Confirm");
    cx.bind_keys([
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("q", Quit, browse),
        KeyBinding::new("down", SelectNext, browse),
        KeyBinding::new("j", SelectNext, browse),
        KeyBinding::new("up", SelectPrevious, browse),
        KeyBinding::new("k", SelectPrevious, browse),
        KeyBinding::new("pagedown", PageDown, browse),
        KeyBinding::new("pageup", PageUp, browse),
        KeyBinding::new("home", SelectFirst, browse),
        KeyBinding::new("end", SelectLast, browse),
        KeyBinding::new("right", SortNext, browse),
        KeyBinding::new("left", SortPrevious, browse),
        KeyBinding::new("r", ReverseSort, browse),
        KeyBinding::new("f", StartFilter, browse),
        KeyBinding::new("/", StartFilter, browse),
        KeyBinding::new("escape", ClearFilter, browse),
        KeyBinding::new("t", Terminate, browse),
        KeyBinding::new("shift-k", Kill, browse),
        KeyBinding::new("e", ToggleTree, browse),
        KeyBinding::new("space", ToggleFold, browse),
        // `+` arrives as `shift-=` or as `+` depending on the layout and
        // the platform's keymap handling; `=` saves reaching for shift.
        KeyBinding::new("+", LongerInterval, browse),
        KeyBinding::new("shift-=", LongerInterval, browse),
        KeyBinding::new("=", LongerInterval, browse),
        KeyBinding::new("-", ShorterInterval, browse),
        KeyBinding::new("down", SelectNext, filter),
        KeyBinding::new("up", SelectPrevious, filter),
        KeyBinding::new("enter", CommitFilter, filter),
        KeyBinding::new("escape", ClearFilter, filter),
        KeyBinding::new("y", Confirm, confirm),
        KeyBinding::new("enter", Confirm, confirm),
        KeyBinding::new("n", Cancel, confirm),
        KeyBinding::new("escape", Cancel, confirm),
    ]);
}

pub struct Monitor {
    snapshot: Option<Snapshot>,
    cpu_history: History,
    /// One per logical core, created with the first snapshot.
    core_histories: Vec<History>,
    rx_history: History,
    tx_history: History,
    table: ProcessTable,
    mode: Mode,
    /// Index into [`INTERVALS_MS`].
    interval_step: usize,
    /// Tells the sampling loop about a new interval, so a change applies
    /// to the wait already in progress.
    interval_changes: mpsc::UnboundedSender<Duration>,
    /// Result of the last signal sent, shown in the process box footer.
    status: Option<SharedString>,
    theme: Theme,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
}

impl Monitor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (interval_changes, mut new_intervals) = mpsc::unbounded();
        let mut interval = Duration::from_millis(INTERVALS_MS[DEFAULT_INTERVAL]);
        cx.spawn(async move |this, cx| {
            let mut sampler = cx
                .background_executor()
                .spawn(async { Sampler::new() })
                .await;
            // The first sample needs a gap after the baseline `new` took.
            cx.background_executor()
                .timer(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL)
                .await;
            loop {
                let (returned, snapshot) = cx
                    .background_executor()
                    .spawn(async move {
                        let snapshot = sampler.sample();
                        (sampler, snapshot)
                    })
                    .await;
                sampler = returned;
                // `update` fails once the window and this view are gone.
                if this
                    .update(cx, |this, cx| this.ingest(snapshot, cx))
                    .is_err()
                {
                    return;
                }

                // Wait until `interval` after this sample. A rate change
                // restarts the wait with the new interval, still counted
                // from the same sample, so going from 5 s to 250 ms takes
                // effect immediately instead of after the old 5 s.
                let sampled_at = Instant::now();
                loop {
                    let remaining = interval.saturating_sub(sampled_at.elapsed());
                    if remaining.is_zero() {
                        break;
                    }
                    let timer = cx.background_executor().timer(remaining);
                    match future::select(timer, new_intervals.next()).await {
                        Either::Left(_) => break,
                        Either::Right((Some(changed), _)) => interval = changed,
                        // The sender lives in the view, so the view is gone.
                        Either::Right((None, _)) => return,
                    }
                }
            }
        })
        .detach();

        Self {
            snapshot: None,
            cpu_history: History::new(HISTORY_LEN),
            core_histories: Vec::new(),
            rx_history: History::new(HISTORY_LEN),
            tx_history: History::new(HISTORY_LEN),
            table: ProcessTable::new(SortOrder::by(SortColumn::Cpu)),
            mode: Mode::Browse,
            interval_step: DEFAULT_INTERVAL,
            interval_changes,
            status: None,
            theme: Theme::new(cx),
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    /// The process list moves into `table`, which owns it from here on;
    /// the rest of the snapshot stays for the other boxes.
    fn ingest(&mut self, mut snapshot: Snapshot, cx: &mut Context<Self>) {
        self.cpu_history.push(snapshot.cpu.total);
        let cores = &snapshot.cpu.per_core;
        self.core_histories
            .resize_with(cores.len(), || History::new(CORE_HISTORY_LEN));
        for (history, &usage) in self.core_histories.iter_mut().zip(cores) {
            history.push(usage);
        }
        self.rx_history.push(snapshot.network.rx_per_sec as f32);
        self.tx_history.push(snapshot.network.tx_per_sec as f32);
        self.table.replace(std::mem::take(&mut snapshot.processes));
        self.snapshot = Some(snapshot);
        cx.notify();
    }

    // ----- Process table state -----

    fn select_row(&mut self, index: usize, strategy: ScrollStrategy, cx: &mut Context<Self>) {
        if self.table.select_index(index) {
            self.scroll_handle.scroll_to_item(index, strategy);
            cx.notify();
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.table.len() == 0 {
            return;
        }
        let last = self.table.len() as isize - 1;
        let target = match self.table.selected_index() {
            Some(index) => (index as isize + delta).clamp(0, last),
            None => 0,
        };
        let strategy = if delta < 0 {
            ScrollStrategy::Top
        } else {
            ScrollStrategy::Bottom
        };
        self.select_row(target as usize, strategy, cx);
    }

    fn set_sort(&mut self, sort: SortOrder, cx: &mut Context<Self>) {
        self.table.set_sort(sort);
        if let Some(index) = self.table.selected_index() {
            self.scroll_handle
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn request_signal(&mut self, signal: Signal, cx: &mut Context<Self>) {
        let Some(process) = self.table.selected() else {
            return;
        };
        self.mode = Mode::Confirm(PendingSignal {
            pid: process.pid,
            name: process.name.clone(),
            signal,
        });
        cx.notify();
    }

    // ----- Actions -----

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(PAGE_ROWS as isize, cx);
    }

    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-(PAGE_ROWS as isize), cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.select_row(0, ScrollStrategy::Top, cx);
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.select_row(
            self.table.len().saturating_sub(1),
            ScrollStrategy::Bottom,
            cx,
        );
    }

    fn sort_next(&mut self, _: &SortNext, _: &mut Window, cx: &mut Context<Self>) {
        self.set_sort(SortOrder::by(self.table.sort().column.step(1)), cx);
    }

    fn sort_previous(&mut self, _: &SortPrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.set_sort(SortOrder::by(self.table.sort().column.step(-1)), cx);
    }

    fn reverse_sort(&mut self, _: &ReverseSort, _: &mut Window, cx: &mut Context<Self>) {
        let sort = self.table.sort();
        self.set_sort(sort.toggled(sort.column), cx);
    }

    fn start_filter(&mut self, _: &StartFilter, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Filter;
        cx.notify();
    }

    fn clear_filter(&mut self, _: &ClearFilter, _: &mut Window, cx: &mut Context<Self>) {
        self.table.clear_filter();
        self.mode = Mode::Browse;
        cx.notify();
    }

    fn commit_filter(&mut self, _: &CommitFilter, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Browse;
        cx.notify();
    }

    fn terminate(&mut self, _: &Terminate, _: &mut Window, cx: &mut Context<Self>) {
        self.request_signal(Signal::Term, cx);
    }

    fn kill(&mut self, _: &Kill, _: &mut Window, cx: &mut Context<Self>) {
        self.request_signal(Signal::Kill, cx);
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        let Mode::Confirm(pending) = std::mem::replace(&mut self.mode, Mode::Browse) else {
            return;
        };
        let outcome = match pending.signal.send(pending.pid) {
            Ok(()) => format!(
                "Sent {} to {} ({})",
                pending.signal.name(),
                pending.name,
                pending.pid
            ),
            Err(err) => format!("Couldn't signal {} ({}): {err}", pending.name, pending.pid),
        };
        self.status = Some(outcome.into());
        cx.notify();
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Browse;
        cx.notify();
    }

    fn interval(&self) -> u64 {
        INTERVALS_MS[self.interval_step]
    }

    fn step_interval(&mut self, delta: isize, cx: &mut Context<Self>) {
        let step = self
            .interval_step
            .saturating_add_signed(delta)
            .min(INTERVALS_MS.len() - 1);
        if step == self.interval_step {
            return;
        }
        self.interval_step = step;
        // Fails only once the sampling loop has stopped, and then there
        // is nothing left to retime.
        let _ = self
            .interval_changes
            .unbounded_send(Duration::from_millis(self.interval()));
        cx.notify();
    }

    fn longer_interval(&mut self, _: &LongerInterval, _: &mut Window, cx: &mut Context<Self>) {
        self.step_interval(1, cx);
    }

    fn shorter_interval(&mut self, _: &ShorterInterval, _: &mut Window, cx: &mut Context<Self>) {
        self.step_interval(-1, cx);
    }

    fn toggle_tree(&mut self, _: &ToggleTree, _: &mut Window, cx: &mut Context<Self>) {
        self.table.toggle_layout();
        if let Some(index) = self.table.selected_index() {
            self.scroll_handle
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn toggle_fold(&mut self, _: &ToggleFold, _: &mut Window, cx: &mut Context<Self>) {
        if self.table.toggle_fold() {
            cx.notify();
        }
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    /// Typing into the filter. Bound keys never get here: gpui runs a
    /// matching action first and stops the event, and the `Filter` key
    /// context only binds navigation, enter and escape.
    fn filter_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.mode, Mode::Filter) {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.key == "backspace" {
            self.table.pop_filter();
        } else if let Some(text) = keystroke
            .key_char
            .as_ref()
            .filter(|_| !keystroke.modifiers.control && !keystroke.modifiers.platform)
        {
            self.table.push_filter(text);
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    // ----- Rendering: shared pieces -----

    /// A rounded box with its title cut into the top border, the way
    /// btop draws `┌ cpu ┐`.
    fn panel(
        &self,
        title: impl Into<SharedString>,
        accent: Hsla,
        right: Option<SharedString>,
    ) -> Div {
        let theme = &self.theme;
        // gpui paints a div's border after its children, so a label
        // inside the bordered div can't cover the line. The border lives
        // on a separate frame instead, and the labels come after it as
        // siblings, which paints them on top. The frame starts half a
        // title line down so the line runs through the titles' middle.
        let title_height = ROW_HEIGHT;
        let frame = div()
            .absolute()
            .top(px(title_height / 2.0))
            .bottom_0()
            .left_0()
            .right_0()
            .border_1()
            .border_color(accent)
            .rounded_md();
        let label = |text: SharedString| {
            div()
                .absolute()
                .top_0()
                .h(px(title_height))
                .flex()
                .items_center()
                .px_1()
                .bg(theme.background)
                .text_color(theme.title)
                .child(text)
        };
        div()
            .relative()
            .flex()
            .flex_col()
            .min_h_0()
            .min_w_0()
            .pt(px(title_height + 2.0))
            .px_2()
            .pb_1()
            .child(frame)
            .child(label(title.into()).left(px(10.0)))
            .when_some(right, |panel, right| {
                panel.child(label(right).right(px(10.0)).text_color(theme.muted))
            })
    }

    /// One `label  value` line over a full-width meter with a percentage.
    fn meter_row(&self, label: &str, value: String, fraction: f32, gradient: Gradient) -> Div {
        let theme = &self.theme;
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(div().text_color(theme.muted).child(label.to_owned()))
                    .child(value),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(ROW_HEIGHT - 4.0))
                    .child(div().flex_1().h_full().child(meter(
                        fraction,
                        gradient,
                        theme.meter_empty,
                    )))
                    .child(
                        div()
                            .w(chars(4.0))
                            .flex()
                            .justify_end()
                            .text_color(gradient.at(fraction))
                            .child(format!("{:.0}%", fraction * 100.0)),
                    ),
            )
    }

    // ----- Rendering: the four boxes -----

    fn render_cpu(&self, snapshot: &Snapshot) -> Div {
        let theme = &self.theme;
        let cpu = &snapshot.cpu;
        let clock = jiff::Zoned::now().strftime("%H:%M:%S").to_string();
        let right = format!(
            "up {} · {clock} · every {} ms",
            uptime(snapshot.uptime_secs),
            self.interval()
        );

        // Cores fill columns of at most eight, like btop's core grid.
        const CORES_PER_COLUMN: usize = 8;
        let core_histories = &self.core_histories;
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
                                    core_histories[index].tail(CORE_HISTORY_LEN).collect(),
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
            )));

        let history = self.cpu_history.tail(HISTORY_LEN).collect();
        self.panel("cpu", theme.cpu_box, Some(right.into())).child(
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
                            history,
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

    fn render_memory(&self, snapshot: &Snapshot) -> Div {
        let theme = &self.theme;
        let memory = snapshot.memory;
        let share = |part: u64, whole: u64| {
            if whole == 0 {
                0.0
            } else {
                part as f32 / whole as f32
            }
        };

        let disks = snapshot.disks.iter().map(|disk| {
            self.meter_row(
                &disk.mount_point,
                format!("{} / {}", bytes(disk.used), bytes(disk.total)),
                share(disk.used, disk.total),
                theme.used,
            )
        });

        self.panel(
            "mem",
            theme.mem_box,
            Some(format!("total {}", bytes(memory.total)).into()),
        )
        .child(
            div()
                .id("memory-scroll")
                .flex()
                .flex_col()
                .gap_1()
                .flex_1()
                .overflow_y_scroll()
                .child(self.meter_row(
                    "Used",
                    bytes(memory.used),
                    share(memory.used, memory.total),
                    theme.used,
                ))
                .child(self.meter_row(
                    "Available",
                    bytes(memory.available),
                    share(memory.available, memory.total),
                    theme.available,
                ))
                .child(self.meter_row(
                    "Free",
                    bytes(memory.free),
                    share(memory.free, memory.total),
                    theme.free,
                ))
                .when(memory.swap_total > 0, |column| {
                    column.child(self.meter_row(
                        "Swap",
                        format!("{} / {}", bytes(memory.swap_used), bytes(memory.swap_total)),
                        share(memory.swap_used, memory.swap_total),
                        theme.swap,
                    ))
                })
                .child(
                    div()
                        .mt_1()
                        .border_t_1()
                        .border_color(theme.faint)
                        .text_color(theme.title)
                        .child("disks"),
                )
                .children(disks),
        )
    }

    fn render_network(&self, snapshot: &Snapshot) -> Div {
        let theme = &self.theme;
        let network = snapshot.network;

        // Each direction scales to its own peak over the samples shown.
        // The floor keeps an idle link from blowing a few bytes of
        // background chatter up to full height.
        let scaled = |history: &History| -> (Vec<f32>, f32) {
            let peak = history.tail(NETWORK_WINDOW).fold(10_240.0_f32, f32::max);
            (
                history
                    .tail(NETWORK_WINDOW)
                    .map(|value| value / peak)
                    .collect(),
                peak,
            )
        };
        let (rx, rx_peak) = scaled(&self.rx_history);
        let (tx, tx_peak) = scaled(&self.tx_history);

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

        self.panel("net", theme.net_box, None).child(
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

    fn render_process_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let sort = self.table.sort();
        let header = |column: SortColumn, label: &'static str| {
            let active = sort.column == column;
            let arrow = match (active, sort.descending) {
                (false, _) => "",
                (true, true) => "↓",
                (true, false) => "↑",
            };
            div()
                .id(label)
                .cursor_pointer()
                .text_color(if active { theme.highlight } else { theme.title })
                .child(format!("{label}{arrow}"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_sort(this.table.sort().toggled(column), cx);
                }))
        };
        Self::process_columns(
            self.name_width(),
            div()
                .h(px(ROW_HEIGHT))
                .border_b_1()
                .border_color(theme.faint),
            header(SortColumn::Pid, "Pid").flex().justify_end(),
            header(SortColumn::Name, "Program"),
            div().text_color(theme.title).child("Command"),
            header(SortColumn::Threads, "Thr").flex().justify_end(),
            header(SortColumn::User, "User"),
            header(SortColumn::Memory, "Mem").flex().justify_end(),
            header(SortColumn::Cpu, "Cpu%").flex().justify_end(),
        )
    }

    /// Tree view needs room for the guides in front of each name.
    fn name_width(&self) -> gpui::Pixels {
        match self.table.layout() {
            Layout::Flat => chars(15.0),
            Layout::Tree => chars(34.0),
        }
    }

    /// The one place that knows the column widths, shared by the header
    /// and every row so they can't drift apart.
    #[allow(clippy::too_many_arguments)]
    fn process_columns<Row: ParentElement + Styled>(
        name_width: gpui::Pixels,
        row: Row,
        pid: impl IntoElement + Styled,
        name: impl IntoElement + Styled,
        command: impl IntoElement + Styled,
        threads: impl IntoElement + Styled,
        user: impl IntoElement + Styled,
        memory: impl IntoElement + Styled,
        cpu: impl IntoElement + Styled,
    ) -> Row {
        row.w_full()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_1()
            .child(pid.w(chars(7.0)).flex_shrink_0())
            .child(name.w(name_width).flex_shrink_0().truncate())
            .child(command.flex_1().min_w_0().truncate())
            .child(threads.w(chars(4.0)).flex_shrink_0())
            .child(user.w(chars(9.0)).flex_shrink_0().truncate())
            .child(memory.w(chars(9.0)).flex_shrink_0())
            .child(cpu.w(chars(12.0)).flex_shrink_0())
    }

    fn render_process_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        range
            .filter_map(|index| self.table.row(index))
            .map(|row| {
                let process = row.process;
                let pid = process.pid;
                let is_selected = self.table.is_selected(pid);
                let right = || div().flex().justify_end();
                // Show tiny loads instead of rounding them to an empty
                // meter, so "uses some CPU" stays visible.
                let cpu_fraction = if process.cpu > 0.0 {
                    process.cpu.max(0.02)
                } else {
                    0.0
                };
                let cpu = div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(ROW_HEIGHT))
                    .child(div().w(px(34.0)).h(px(12.0)).child(meter(
                        cpu_fraction,
                        theme.process,
                        theme.meter_empty,
                    )))
                    .child(
                        right()
                            .flex_1()
                            .text_color(theme.process.at(process.cpu))
                            .child(format!("{:.1}", process.cpu * 100.0)),
                    );
                let marker = match row.fold {
                    Fold::Leaf => "",
                    Fold::Open => "▾ ",
                    Fold::Closed => "▸ ",
                };
                let name = div()
                    .flex()
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme.faint)
                            .child(row.guides.to_owned()),
                    )
                    .child(div().flex_shrink_0().text_color(theme.muted).child(marker))
                    .child(div().min_w_0().truncate().child(process.name.clone()));
                Self::process_columns(
                    self.name_width(),
                    div()
                        .id(("process", pid as usize))
                        .h(px(ROW_HEIGHT))
                        .cursor_pointer()
                        .when(is_selected, |row| {
                            row.bg(theme.selected_bg).text_color(theme.title)
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.table.select_pid(pid);
                            window.focus(&this.focus_handle);
                            cx.notify();
                        })),
                    right().child(pid.to_string()),
                    name,
                    div()
                        .when(!is_selected, |cell| cell.text_color(theme.muted))
                        .child(process.command.clone()),
                    right().child(process.threads.to_string()),
                    div().child(process.user.clone()),
                    right().child(bytes(process.memory)),
                    cpu,
                )
                .into_any_element()
            })
            .collect()
    }

    fn render_process_footer(&self) -> impl IntoElement {
        let theme = &self.theme;
        let shown = format!("{}/{}", self.table.len(), self.table.total());
        let left: AnyElement =
            match &self.mode {
                Mode::Confirm(pending) => div()
                    .text_color(theme.highlight)
                    .child(format!(
                        "Send {} to {} ({})?  y / n",
                        pending.signal.name(),
                        pending.name,
                        pending.pid
                    ))
                    .into_any_element(),
                Mode::Filter => div()
                    .text_color(theme.title)
                    .child(format!("filter: {}█", self.table.filter()))
                    .into_any_element(),
                Mode::Browse if !self.table.filter().is_empty() => div()
                    .child(format!("filter: {}  (esc clears)", self.table.filter()))
                    .into_any_element(),
                Mode::Browse => div()
                    .text_color(theme.muted)
                    .child(self.status.clone().unwrap_or_else(|| {
                        "f filter · ←/→ sort · r reverse · e tree · space fold · +/- rate · t term · K kill".into()
                    }))
                    .into_any_element(),
            };
        div()
            .flex()
            .flex_shrink_0()
            .justify_between()
            .gap_2()
            .pt_1()
            .border_t_1()
            .border_color(theme.faint)
            .child(div().flex_1().min_w_0().truncate().child(left))
            .child(div().text_color(theme.muted).child(shown))
    }

    fn render_processes(&self, cx: &mut Context<Self>) -> Div {
        let theme = &self.theme;
        self.panel(
            "proc",
            theme.proc_box,
            Some(
                format!(
                    "{}{} by {:?}",
                    match self.table.layout() {
                        Layout::Flat => "",
                        Layout::Tree => "tree · ",
                    },
                    sort_label(self.table.sort()),
                    self.table.sort().column
                )
                .into(),
            ),
        )
        .child(self.render_process_header(cx))
        .child(
            uniform_list(
                "processes",
                self.table.len(),
                cx.processor(|this, range, _window, cx| this.render_process_rows(range, cx)),
            )
            .track_scroll(self.scroll_handle.clone())
            .flex_1(),
        )
        .child(self.render_process_footer())
    }
}

fn sort_label(sort: SortOrder) -> &'static str {
    if sort.descending { "desc" } else { "asc" }
}

impl Focusable for Monitor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Monitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let root = div()
            .key_context(self.mode.key_context())
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::sort_next))
            .on_action(cx.listener(Self::sort_previous))
            .on_action(cx.listener(Self::reverse_sort))
            .on_action(cx.listener(Self::start_filter))
            .on_action(cx.listener(Self::clear_filter))
            .on_action(cx.listener(Self::commit_filter))
            .on_action(cx.listener(Self::terminate))
            .on_action(cx.listener(Self::kill))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::longer_interval))
            .on_action(cx.listener(Self::shorter_interval))
            .on_action(cx.listener(Self::toggle_tree))
            .on_action(cx.listener(Self::toggle_fold))
            .on_action(cx.listener(Self::quit))
            .on_key_down(cx.listener(Self::filter_key_down))
            .size_full()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .bg(theme.background)
            .text_color(theme.text)
            .text_xs()
            .font_family(theme.mono.clone());

        // The first snapshot arrives a moment after the window opens.
        let Some(snapshot) = &self.snapshot else {
            return root
                .items_center()
                .justify_center()
                .text_color(theme.muted)
                .child("reading system…");
        };

        root.child(self.render_cpu(snapshot).h(relative(0.36)).flex_shrink_0())
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
                            .gap_1()
                            .w(relative(0.38))
                            .flex_shrink_0()
                            .child(self.render_memory(snapshot).flex_1())
                            .child(self.render_network(snapshot).flex_1()),
                    )
                    .child(self.render_processes(cx).flex_1()),
            )
    }
}

fn bytes(count: u64) -> String {
    humansize::format_size(count, humansize::BINARY.decimal_places(1))
}

fn uptime(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60);
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else {
        format!("{hours}h {minutes}m")
    }
}
