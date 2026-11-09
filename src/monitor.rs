//! The window's one view: four btop-style boxes over the latest
//! [`Snapshot`], fed by a sampling loop on the background executor.

use std::collections::HashMap;
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

use crate::gpu::GpuSample;
use crate::graph::{GraphStyle, Grow, graph, meter};
use crate::history::History;
use crate::procs::{Fold, Layout, ProcessTable, SortColumn, SortOrder};
use crate::sample::{DiskSample, MemorySample, ProcessSample, Sampler, Snapshot};
use crate::theme::{self, Gradient, Theme};

/// Process list refresh intervals that `+` and `-` step through.
const INTERVALS_MS: [u64; 6] = [250, 500, 1000, 2000, 3000, 5000];
/// Graphs never sample faster than this, which stays above sysinfo's
/// 200 ms minimum between CPU readings.
const MIN_GRAPH_INTERVAL_MS: u64 = 250;
const MAX_GRAPHS_PER_REFRESH: u32 = 8;

/// How often the sampling loop runs. The process list refreshes every
/// [`Rates::process_interval`]; graphs and meters sample
/// [`Rates::graphs_per_refresh`] times in that interval, since reading
/// every process costs far more than everything else together.
#[derive(Debug, Clone, Copy)]
struct Rates {
    /// Index into [`INTERVALS_MS`].
    process_step: usize,
    /// What the user asked for with `[` and `]`. Short process intervals
    /// can't fit that many graph samples, so it is clamped where used
    /// and comes back once the interval is long enough again.
    wanted_graphs_per_refresh: u32,
}

impl Rates {
    const DEFAULT: Self = Self {
        process_step: 2,
        wanted_graphs_per_refresh: 2,
    };

    fn process_interval(self) -> Duration {
        Duration::from_millis(INTERVALS_MS[self.process_step])
    }

    fn max_graphs_per_refresh(self) -> u32 {
        let fits = INTERVALS_MS[self.process_step] / MIN_GRAPH_INTERVAL_MS;
        (fits as u32).clamp(1, MAX_GRAPHS_PER_REFRESH)
    }

    fn graphs_per_refresh(self) -> u32 {
        self.wanted_graphs_per_refresh
            .min(self.max_graphs_per_refresh())
    }

    fn graph_interval(self) -> Duration {
        self.process_interval() / self.graphs_per_refresh()
    }

    fn step_process_interval(self, delta: isize) -> Self {
        Self {
            process_step: self
                .process_step
                .saturating_add_signed(delta)
                .min(INTERVALS_MS.len() - 1),
            ..self
        }
    }

    /// Steps from the count in effect, not the wanted one, so each press
    /// changes what is on screen.
    fn step_graphs_per_refresh(self, delta: i32) -> Self {
        Self {
            wanted_graphs_per_refresh: self
                .graphs_per_refresh()
                .saturating_add_signed(delta)
                .clamp(1, self.max_graphs_per_refresh()),
            ..self
        }
    }
}
/// Ten minutes at one sample a second, wider than any graph on a 4K
/// screen at 3 px per sample.
const HISTORY_LEN: usize = 600;
/// Per-core sparklines are about 60 px wide at 2 px per sample.
const CORE_HISTORY_LEN: usize = 32;
/// Samples the network graphs show, stretched to their width. The
/// graphs scale to the peak of what they show, so the window has to be
/// known up front for the "Top" label to name that same peak.
const NETWORK_WINDOW: usize = 120;
/// Samples each disk's I/O graph shows. The graphs are narrow, so fewer
/// samples than the network graphs keep single bursts readable.
const DISK_IO_WINDOW: usize = 60;
const ROW_HEIGHT: f32 = 18.0;
const DISK_IO_HEIGHT: f32 = 32.0;
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
        MoreGraphSamples,
        FewerGraphSamples,
        ToggleTree,
        ToggleFold,
        NextTheme,
        PreviousTheme,
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
        KeyBinding::new("c", NextTheme, browse),
        KeyBinding::new("shift-c", PreviousTheme, browse),
        // `+` arrives as `shift-=` or as `+` depending on the layout and
        // the platform's keymap handling; `=` saves reaching for shift.
        KeyBinding::new("+", LongerInterval, browse),
        KeyBinding::new("shift-=", LongerInterval, browse),
        KeyBinding::new("=", LongerInterval, browse),
        KeyBinding::new("-", ShorterInterval, browse),
        KeyBinding::new("]", MoreGraphSamples, browse),
        KeyBinding::new("[", FewerGraphSamples, browse),
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
    /// Read and write rates per mount point, for the disks currently
    /// mounted.
    disk_io: HashMap<String, DiskIo>,
    tx_history: History,
    /// Indexed like [`MemoryStat::ALL`].
    memory_histories: [History; MemoryStat::ALL.len()],
    table: ProcessTable,
    mode: Mode,
    rates: Rates,
    /// Tells the sampling loop about new rates, so a change applies to
    /// the wait already in progress.
    rate_changes: mpsc::UnboundedSender<Rates>,
    /// Result of the last signal sent, shown in the process box footer.
    status: Option<SharedString>,
    theme: Theme,
    /// Every theme `c` steps through, built-in ones first.
    themes: Vec<Theme>,
    mono: SharedString,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
}

impl Monitor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (rate_changes, mut new_rates) = mpsc::unbounded();
        let mut rates = Rates::DEFAULT;
        // Counts graph samples; the process list refreshes on every
        // `graphs_per_refresh`-th one, starting with the first.
        let mut tick: u32 = 0;
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
                let with_processes = tick.is_multiple_of(rates.graphs_per_refresh());
                tick = tick.wrapping_add(1);
                let (returned, snapshot, processes) = cx
                    .background_executor()
                    .spawn(async move {
                        let snapshot = sampler.sample();
                        let processes = with_processes.then(|| sampler.sample_processes());
                        (sampler, snapshot, processes)
                    })
                    .await;
                sampler = returned;
                // `update` fails once the window and this view are gone.
                if this
                    .update(cx, |this, cx| this.ingest(snapshot, processes, cx))
                    .is_err()
                {
                    return;
                }

                // Wait one graph interval after this sample. A rate change
                // restarts the wait with the new interval, still counted
                // from the same sample, so going from 5 s to 250 ms takes
                // effect immediately instead of after the old 5 s. It also
                // restarts the count, so the process list follows at once.
                let sampled_at = Instant::now();
                loop {
                    let remaining = rates.graph_interval().saturating_sub(sampled_at.elapsed());
                    if remaining.is_zero() {
                        break;
                    }
                    let timer = cx.background_executor().timer(remaining);
                    match future::select(timer, new_rates.next()).await {
                        Either::Left(_) => break,
                        Either::Right((Some(changed), _)) => {
                            rates = changed;
                            tick = 0;
                        }
                        // The sender lives in the view, so the view is gone.
                        Either::Right((None, _)) => return,
                    }
                }
            }
        })
        .detach();

        let themes = theme::load_all();
        let theme = theme::saved_choice()
            .and_then(|name| themes.iter().find(|theme| theme.name == name))
            .unwrap_or(&themes[0])
            .clone();
        Self {
            snapshot: None,
            cpu_history: History::new(HISTORY_LEN),
            core_histories: Vec::new(),
            rx_history: History::new(HISTORY_LEN),
            disk_io: HashMap::new(),
            tx_history: History::new(HISTORY_LEN),
            memory_histories: std::array::from_fn(|_| History::new(HISTORY_LEN)),
            table: ProcessTable::new(SortOrder::by(SortColumn::Cpu)),
            mode: Mode::Browse,
            rates: Rates::DEFAULT,
            rate_changes,
            status: None,
            theme,
            themes,
            mono: theme::pick_mono_font(cx),
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    /// A process list, when this sample includes one, moves into `table`,
    /// which owns it from here on; the snapshot stays for the other boxes.
    fn ingest(
        &mut self,
        snapshot: Snapshot,
        processes: Option<Vec<ProcessSample>>,
        cx: &mut Context<Self>,
    ) {
        self.cpu_history.push(snapshot.cpu.total);
        let cores = &snapshot.cpu.per_core;
        self.core_histories
            .resize_with(cores.len(), || History::new(CORE_HISTORY_LEN));
        for (history, &usage) in self.core_histories.iter_mut().zip(cores) {
            history.push(usage);
        }
        self.rx_history.push(snapshot.network.rx_per_sec as f32);
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
        self.tx_history.push(snapshot.network.tx_per_sec as f32);
        for (stat, history) in MemoryStat::ALL.iter().zip(&mut self.memory_histories) {
            history.push(stat.share(snapshot.memory));
        }
        if let Some(processes) = processes {
            self.table.replace(processes);
        }
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

    fn set_rates(&mut self, rates: Rates, cx: &mut Context<Self>) {
        self.rates = rates;
        // Fails only once the sampling loop has stopped, and then there
        // is nothing left to retime.
        let _ = self.rate_changes.unbounded_send(rates);
        cx.notify();
    }

    fn longer_interval(&mut self, _: &LongerInterval, _: &mut Window, cx: &mut Context<Self>) {
        self.set_rates(self.rates.step_process_interval(1), cx);
    }

    fn shorter_interval(&mut self, _: &ShorterInterval, _: &mut Window, cx: &mut Context<Self>) {
        self.set_rates(self.rates.step_process_interval(-1), cx);
    }

    fn more_graph_samples(&mut self, _: &MoreGraphSamples, _: &mut Window, cx: &mut Context<Self>) {
        self.set_rates(self.rates.step_graphs_per_refresh(1), cx);
    }

    fn fewer_graph_samples(
        &mut self,
        _: &FewerGraphSamples,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_rates(self.rates.step_graphs_per_refresh(-1), cx);
    }

    fn toggle_tree(&mut self, _: &ToggleTree, _: &mut Window, cx: &mut Context<Self>) {
        self.table.toggle_layout();
        if let Some(index) = self.table.selected_index() {
            self.scroll_handle
                .scroll_to_item(index, ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn step_theme(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.themes.len() as isize;
        let current = self
            .themes
            .iter()
            .position(|theme| theme.name == self.theme.name)
            .unwrap_or(0) as isize;
        self.theme = self.themes[(current + delta).rem_euclid(count) as usize].clone();
        let name = &self.theme.name;
        self.status = Some(match theme::save_choice(name) {
            Ok(()) => format!("Theme: {name}").into(),
            Err(err) => format!("Theme: {name} (couldn't save the choice: {err})").into(),
        });
        cx.notify();
    }

    fn next_theme(&mut self, _: &NextTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.step_theme(1, cx);
    }

    fn previous_theme(&mut self, _: &PreviousTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.step_theme(-1, cx);
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

    // ----- Rendering: the four boxes -----

    fn render_cpu(&self, snapshot: &Snapshot) -> Div {
        let theme = &self.theme;
        let cpu = &snapshot.cpu;
        let clock = jiff::Zoned::now().strftime("%H:%M:%S").to_string();
        let right = format!(
            "up {} · {clock} · procs {} ms · graphs {} ms",
            uptime(snapshot.uptime_secs),
            self.rates.process_interval().as_millis(),
            self.rates.graph_interval().as_millis(),
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
            )))
            .children(self.render_gpus(&snapshot.gpus));

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

    /// The `mem` box, laid out like btop's: memory readings on the left,
    /// each a `label  amount` line over its history graph, and disks on
    /// the right. The readings share the box height evenly, so the graphs
    /// grow with the box.
    /// One line per GPU under the cores, like btop's: usage, VRAM,
    /// temperature and power. Numbered only when there is more than one.
    fn render_gpus<'a>(&'a self, gpus: &'a [GpuSample]) -> impl Iterator<Item = Div> + 'a {
        let theme = &self.theme;
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
                .child(div().flex_1().h_full().child(meter(
                    gpu.usage,
                    theme.cpu,
                    theme.meter_empty,
                )))
                .child(
                    div()
                        .text_color(theme.cpu.at(gpu.usage))
                        .child(format!("{:>3.0}%", gpu.usage * 100.0)),
                )
                .child(div().w(px(34.0)).h(px(12.0)).child(meter(
                    vram,
                    theme.used,
                    theme.meter_empty,
                )))
                .child(format!(
                    "{}/{}",
                    short_bytes(gpu.vram_used),
                    short_bytes(gpu.vram_total)
                ))
                .children(temperature)
                .children(power)
        })
    }

    fn render_memory(&self, snapshot: &Snapshot) -> Div {
        let theme = &self.theme;
        let memory = snapshot.memory;

        let readings = MemoryStat::ALL
            .iter()
            .zip(&self.memory_histories)
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
            .map(|disk| self.render_disk(disk, self.disk_io.get(&disk.mount_point)));

        self.panel(
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

    /// A disk as btop shows it: its size, its I/O as mirrored graphs
    /// with reads growing up and writes down, and how full it is. The
    /// column is narrow, so the meter's percentage stands in for the
    /// used amount.
    fn render_disk(&self, disk: &DiskSample, io: Option<&DiskIo>) -> Div {
        let theme = &self.theme;
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
                            .child(div().flex_1().child(graph(
                                read,
                                theme.download,
                                style(Grow::Up),
                            )))
                            .child(div().flex_1().child(graph(
                                write,
                                theme.upload,
                                style(Grow::Down),
                            ))),
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

    fn render_network(&self, snapshot: &Snapshot) -> Div {
        let theme = &self.theme;
        let network = snapshot.network;

        // The floor keeps an idle link from blowing a few bytes of
        // background chatter up to full height.
        let (rx, rx_peak) = scaled(&self.rx_history, NETWORK_WINDOW, 10_240.0);
        let (tx, tx_peak) = scaled(&self.tx_history, NETWORK_WINDOW, 10_240.0);

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
                            row.bg(theme.selected_bg).text_color(theme.selected_fg)
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
                        "f filter · ←/→ sort · r reverse · e tree · space fold · +/- rate · [/] graph rate · c theme · t term · K kill".into()
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
            .on_action(cx.listener(Self::more_graph_samples))
            .on_action(cx.listener(Self::fewer_graph_samples))
            .on_action(cx.listener(Self::toggle_tree))
            .on_action(cx.listener(Self::toggle_fold))
            .on_action(cx.listener(Self::next_theme))
            .on_action(cx.listener(Self::previous_theme))
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
            .font_family(self.mono.clone());

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

/// A memory reading shown with a history graph in the `mem` box.
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

fn share(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        part as f32 / whole as f32
    }
}

/// The last `window` samples divided by their peak, which never drops
/// below `floor`, and that peak.
fn scaled(history: &History, window: usize, floor: f32) -> (Vec<f32>, f32) {
    let peak = history.tail(window).fold(floor, f32::max);
    (
        history.tail(window).map(|value| value / peak).collect(),
        peak,
    )
}

#[derive(Debug)]
struct DiskIo {
    read: History,
    write: History,
}

/// Sizes in btop's terse style, e.g. `1.5G` or `16G`, for tight rows.
fn short_bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 && unit > 0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.0}{}", UNITS[unit])
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

#[cfg(test)]
mod tests {
    use super::*;

    fn intervals(rates: Rates) -> (u128, u128) {
        (
            rates.process_interval().as_millis(),
            rates.graph_interval().as_millis(),
        )
    }

    #[test]
    fn graphs_sample_several_times_per_process_refresh() {
        assert_eq!(intervals(Rates::DEFAULT), (1000, 500));
        let more = Rates::DEFAULT.step_graphs_per_refresh(1);
        assert_eq!(intervals(more), (1000, 333));
        let slower = Rates::DEFAULT.step_process_interval(1);
        assert_eq!(intervals(slower), (2000, 1000));
    }

    #[test]
    fn graph_interval_never_drops_below_the_minimum() {
        // At 1 s, four graph samples already take 250 ms each.
        let capped = Rates::DEFAULT
            .step_graphs_per_refresh(1)
            .step_graphs_per_refresh(1)
            .step_graphs_per_refresh(1);
        assert_eq!(intervals(capped), (1000, 250));
        let fastest = Rates::DEFAULT.step_process_interval(-2);
        assert_eq!(intervals(fastest), (250, 250));
    }

    #[test]
    fn clamped_choice_comes_back_with_a_longer_interval() {
        let round_trip = Rates::DEFAULT
            .step_process_interval(-2)
            .step_process_interval(2);
        assert_eq!(intervals(round_trip), (1000, 500));
    }
}
