//! The window's root view: the four btop boxes over the latest
//! [`Snapshot`], fed by a sampling loop on the background executor.

use std::time::Instant;

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::{self, Either};

use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, KeyBinding, SharedString,
    Window, actions, div, prelude::*, relative,
};

use crate::boxes::cpu::CpuBox;
use crate::boxes::mem::MemBox;
use crate::boxes::net::NetBox;
use crate::boxes::proc::{
    Cancel, ClearFilter, CommitFilter, Confirm, Kill, PageDown, PageUp, ProcessBox, ReverseSort,
    SelectFirst, SelectLast, SelectNext, SelectPrevious, SortNext, SortPrevious, StartFilter,
    Terminate, ToggleDetail, ToggleFold, ToggleTree,
};
use crate::rates::Rates;
use crate::sample::{ProcessList, Sampler, Snapshot};
use crate::theme::{self, Theme};

actions!(
    lki_top,
    [
        LongerInterval,
        ShorterInterval,
        MoreGraphSamples,
        FewerGraphSamples,
        NextTheme,
        PreviousTheme,
        Quit,
    ]
);

/// The process box sets the key contexts, since it holds the focus.
/// This view's own actions use the `Browse` context too, and reach it as
/// they bubble up from the focused box.
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
        KeyBinding::new("enter", ToggleDetail, browse),
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
    /// The latest reading, for the boxes that show its current values.
    snapshot: Option<Snapshot>,
    cpu: CpuBox,
    mem: MemBox,
    net: NetBox,
    processes: Entity<ProcessBox>,
    rates: Rates,
    /// Tells the sampling loop about new rates, so a change applies to
    /// the wait already in progress.
    rate_changes: mpsc::UnboundedSender<Rates>,
    /// Every theme `c` steps through, built-in ones first. The one in
    /// use is the [`Theme`] global.
    themes: Vec<Theme>,
    mono: SharedString,
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
        let chosen = theme::saved_choice()
            .and_then(|name| themes.iter().find(|theme| theme.name == name))
            .unwrap_or(&themes[0])
            .clone();
        cx.set_global(chosen);
        Self {
            snapshot: None,
            cpu: CpuBox::new(),
            mem: MemBox::new(),
            net: NetBox::new(),
            processes: cx.new(ProcessBox::new),
            rates: Rates::DEFAULT,
            rate_changes,
            themes,
            mono: theme::pick_mono_font(cx),
        }
    }

    /// The process list, when this sample includes one, goes to the
    /// process box; the snapshot stays for the current values.
    fn ingest(
        &mut self,
        snapshot: Snapshot,
        processes: Option<ProcessList>,
        cx: &mut Context<Self>,
    ) {
        self.cpu.ingest(&snapshot);
        self.mem.ingest(&snapshot);
        self.net.ingest(&snapshot);
        if let Some(processes) = processes {
            self.processes
                .update(cx, |processes_box, cx| processes_box.replace(processes, cx));
        }
        self.snapshot = Some(snapshot);
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

    fn step_theme(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.themes.len() as isize;
        let current_name = &cx.global::<Theme>().name;
        let current = self
            .themes
            .iter()
            .position(|theme| &theme.name == current_name)
            .unwrap_or(0) as isize;
        let next = self.themes[(current + delta).rem_euclid(count) as usize].clone();
        let name = next.name.clone();
        cx.set_global(next);
        let status = match theme::save_choice(&name) {
            Ok(()) => format!("Theme: {name}"),
            Err(err) => format!("Theme: {name} (couldn't save the choice: {err})"),
        };
        self.processes
            .update(cx, |processes, cx| processes.show_status(status.into(), cx));
        cx.notify();
    }

    fn next_theme(&mut self, _: &NextTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.step_theme(1, cx);
    }

    fn previous_theme(&mut self, _: &PreviousTheme, _: &mut Window, cx: &mut Context<Self>) {
        self.step_theme(-1, cx);
    }

    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }
}

impl Focusable for Monitor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.processes.focus_handle(cx)
    }
}

impl Render for Monitor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let root = div()
            .on_action(cx.listener(Self::longer_interval))
            .on_action(cx.listener(Self::shorter_interval))
            .on_action(cx.listener(Self::more_graph_samples))
            .on_action(cx.listener(Self::fewer_graph_samples))
            .on_action(cx.listener(Self::next_theme))
            .on_action(cx.listener(Self::previous_theme))
            .on_action(cx.listener(Self::quit))
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

        root.child(
            self.cpu
                .render(snapshot, self.rates, &theme)
                .h(relative(0.36))
                .flex_shrink_0(),
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
                        .gap_1()
                        .w(relative(0.38))
                        .flex_shrink_0()
                        .child(self.mem.render(snapshot, &theme).flex_1())
                        .child(self.net.render(snapshot, &theme).flex_1()),
                )
                .child(self.processes.clone()),
        )
    }
}
