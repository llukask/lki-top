//! The `proc` box: the process table with its selection, filter, sort
//! and signal prompt. A view of its own, since it holds the keyboard
//! focus and sets the key contexts that decide what each key does.

use std::ops::Range;

use gpui::{
    AnyElement, App, ClickEvent, Context, FocusHandle, Focusable, KeyDownEvent, ScrollStrategy,
    SharedString, UniformListScrollHandle, Window, actions, div, prelude::*, px, uniform_list,
};

use crate::graph::meter;
use crate::procs::{Fold, Layout, ProcessTable, SortColumn, SortOrder};
use crate::sample::ProcessSample;
use crate::theme::Theme;
use crate::ui::{ROW_HEIGHT, bytes, chars, panel};

const PAGE_ROWS: usize = 20;

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
        ToggleTree,
        ToggleFold,
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

pub struct ProcessBox {
    table: ProcessTable,
    mode: Mode,
    /// The last message for the user, such as a signal's result, shown
    /// in the footer.
    status: Option<SharedString>,
    focus_handle: FocusHandle,
    scroll_handle: UniformListScrollHandle,
}

impl ProcessBox {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            table: ProcessTable::new(SortOrder::by(SortColumn::CpuLazy)),
            mode: Mode::Browse,
            status: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: UniformListScrollHandle::new(),
        }
    }

    pub fn replace(&mut self, processes: Vec<ProcessSample>, cx: &mut Context<Self>) {
        self.table.replace(processes);
        cx.notify();
    }

    pub fn show_status(&mut self, status: SharedString, cx: &mut Context<Self>) {
        self.status = Some(status);
        cx.notify();
    }

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
        self.show_status(outcome.into(), cx);
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.mode = Mode::Browse;
        cx.notify();
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

    // ----- Rendering -----

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let sort = self.table.sort();
        let header = |column: SortColumn, label: &'static str| {
            let active = sort.column.header() == column;
            // Clicking the active header flips the sort it shows, so the
            // `Cpu%` header keeps whichever CPU sort is on.
            let target = if active { sort.column } else { column };
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
                    this.set_sort(this.table.sort().toggled(target), cx);
                }))
        };
        columns(
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
            header(SortColumn::CpuLazy, "Cpu%").flex().justify_end(),
        )
    }

    /// Tree view needs room for the guides in front of each name.
    fn name_width(&self) -> gpui::Pixels {
        match self.table.layout() {
            Layout::Flat => chars(15.0),
            Layout::Tree => chars(34.0),
        }
    }

    fn render_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.global::<Theme>().clone();
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
                columns(
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

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
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
}

/// The one place that knows the column widths, shared by the header and
/// every row so they can't drift apart.
#[allow(clippy::too_many_arguments)]
fn columns<Row: ParentElement + Styled>(
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

fn direction(sort: SortOrder) -> &'static str {
    if sort.descending { "desc" } else { "asc" }
}

impl Focusable for ProcessBox {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ProcessBox {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.global::<Theme>().clone();
        let sort = self.table.sort();
        let layout = match self.table.layout() {
            Layout::Flat => "",
            Layout::Tree => "tree · ",
        };
        panel(
            &theme,
            "proc",
            theme.proc_box,
            Some(format!("{layout}{} by {}", direction(sort), sort.column.label()).into()),
        )
        .flex_1()
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
        .on_action(cx.listener(Self::toggle_tree))
        .on_action(cx.listener(Self::toggle_fold))
        .on_key_down(cx.listener(Self::filter_key_down))
        .child(self.render_header(&theme, cx))
        .child(
            uniform_list(
                "processes",
                self.table.len(),
                cx.processor(|this, range, _window, cx| this.render_rows(range, cx)),
            )
            .track_scroll(self.scroll_handle.clone())
            .flex_1(),
        )
        .child(self.render_footer(&theme))
    }
}
