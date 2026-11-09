//! Which processes the table shows, and in what order.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::sample::ProcessSample;

/// Linux's kernel thread parent. Its few hundred children would bury
/// everything else in tree view, so it starts folded.
const KTHREADD_PID: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Flat,
    Tree,
}

/// Whether a tree row has visible children, and whether they're shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Leaf,
    Open,
    Closed,
}

/// One line of the table: which process, and in tree view the guide
/// characters (`│  ├─ `) that go in front of its name.
#[derive(Debug, Clone)]
struct Row {
    index: usize,
    guides: String,
    fold: Fold,
}

#[derive(Debug, Clone, Copy)]
pub struct RowView<'a> {
    pub process: &'a ProcessSample,
    pub guides: &'a str,
    pub fold: Fold,
}

/// The process list with its filter, sort, layout and selection.
///
/// `rows` holds indices into `processes`, so the two only change
/// together. Every mutator reads the selected row *before* touching
/// either, while they still agree, then rebuilds `rows` and repairs the
/// selection in one step.
#[derive(Debug, Clone)]
pub struct ProcessTable {
    processes: Vec<ProcessSample>,
    rows: Vec<Row>,
    filter: String,
    sort: SortOrder,
    layout: Layout,
    /// Pids whose children are hidden in tree view. Kept across
    /// samples, and across switching to flat view and back.
    folded: HashSet<u32>,
    /// Follows the pid, not the row, so the selection stays on the same
    /// process while the list re-sorts under it.
    selected: Option<u32>,
}

impl ProcessTable {
    pub fn new(sort: SortOrder) -> Self {
        Self {
            processes: Vec::new(),
            rows: Vec::new(),
            filter: String::new(),
            sort,
            layout: Layout::Flat,
            folded: HashSet::from([KTHREADD_PID]),
            selected: None,
        }
    }

    pub fn replace(&mut self, processes: Vec<ProcessSample>) {
        self.update(|table| table.processes = processes);
    }

    pub fn set_sort(&mut self, sort: SortOrder) {
        self.update(|table| table.sort = sort);
    }

    pub fn push_filter(&mut self, text: &str) {
        self.update(|table| table.filter.push_str(text));
    }

    pub fn pop_filter(&mut self) {
        self.update(|table| {
            table.filter.pop();
        });
    }

    pub fn clear_filter(&mut self) {
        self.update(|table| table.filter.clear());
    }

    pub fn toggle_layout(&mut self) {
        self.update(|table| {
            table.layout = match table.layout {
                Layout::Flat => Layout::Tree,
                Layout::Tree => Layout::Flat,
            }
        });
    }

    /// Fold or unfold the selected process's children. Returns whether
    /// anything changed: leaves and flat view have nothing to fold.
    pub fn toggle_fold(&mut self) -> bool {
        let Some(row) = self.selected_index().and_then(|index| self.row(index)) else {
            return false;
        };
        if row.fold == Fold::Leaf {
            return false;
        }
        let pid = row.process.pid;
        self.update(|table| {
            if !table.folded.remove(&pid) {
                table.folded.insert(pid);
            }
        });
        true
    }

    /// Apply `change`, then rebuild `rows`. If the selected process is
    /// gone afterwards (it exited, or the filter hides it), select the
    /// row that now sits where it was, so killing a process leaves the
    /// cursor in place instead of dropping it.
    fn update(&mut self, change: impl FnOnce(&mut Self)) {
        let previous_index = self.selected_index();
        change(self);
        self.rows = match self.layout {
            Layout::Flat => arrange(&self.processes, &self.filter, self.sort),
            Layout::Tree => arrange_tree(&self.processes, &self.filter, self.sort, &self.folded),
        };
        if self.selected_index().is_none() {
            self.selected = previous_index
                .map(|index| index.min(self.rows.len().saturating_sub(1)))
                .and_then(|index| self.row(index))
                .map(|row| row.process.pid);
        }
    }

    pub fn select_index(&mut self, index: usize) -> bool {
        let Some(pid) = self.row(index).map(|row| row.process.pid) else {
            return false;
        };
        self.selected = Some(pid);
        true
    }

    pub fn select_pid(&mut self, pid: u32) {
        self.selected = Some(pid);
    }

    pub fn selected_index(&self) -> Option<usize> {
        let pid = self.selected?;
        self.rows
            .iter()
            .position(|row| self.processes[row.index].pid == pid)
    }

    pub fn selected(&self) -> Option<&ProcessSample> {
        self.selected_index()
            .and_then(|index| self.row(index))
            .map(|row| row.process)
    }

    pub fn is_selected(&self, pid: u32) -> bool {
        self.selected == Some(pid)
    }

    /// The row shown at `index`, after filter, sort and folding.
    pub fn row(&self, index: usize) -> Option<RowView<'_>> {
        self.rows.get(index).map(|row| RowView {
            process: &self.processes[row.index],
            guides: &row.guides,
            fold: row.fold,
        })
    }

    /// Rows shown after the filter.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Every process, including the ones the filter hides.
    pub fn total(&self) -> usize {
        self.processes.len()
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn sort(&self) -> SortOrder {
        self.sort
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Pid,
    Name,
    User,
    Threads,
    Memory,
    Cpu,
}

impl SortColumn {
    /// Left-to-right order of the table's columns, which is also the
    /// order the sort-cycling keys walk through.
    pub const ALL: [SortColumn; 6] = [
        SortColumn::Pid,
        SortColumn::Name,
        SortColumn::User,
        SortColumn::Threads,
        SortColumn::Memory,
        SortColumn::Cpu,
    ];

    /// Numbers read best largest-first, text alphabetically.
    fn natural_descending(self) -> bool {
        matches!(
            self,
            SortColumn::Threads | SortColumn::Memory | SortColumn::Cpu
        )
    }

    pub fn step(self, delta: isize) -> SortColumn {
        let index = Self::ALL
            .iter()
            .position(|column| *column == self)
            .expect("ALL lists every column");
        let len = Self::ALL.len() as isize;
        Self::ALL[(index as isize + delta).rem_euclid(len) as usize]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortOrder {
    pub column: SortColumn,
    pub descending: bool,
}

impl SortOrder {
    pub fn by(column: SortColumn) -> Self {
        Self {
            column,
            descending: column.natural_descending(),
        }
    }

    /// Clicking the active column flips direction. Clicking another one
    /// switches to it in that column's natural direction.
    pub fn toggled(self, column: SortColumn) -> Self {
        if column == self.column {
            Self {
                descending: !self.descending,
                ..self
            }
        } else {
            Self::by(column)
        }
    }

    /// Ties break on pid so rows don't swap places between refreshes
    /// when their sort keys are equal.
    fn compare(self, a: &ProcessSample, b: &ProcessSample) -> Ordering {
        let by_key = match self.column {
            SortColumn::Pid => a.pid.cmp(&b.pid),
            SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortColumn::User => a.user.cmp(&b.user),
            SortColumn::Threads => a.threads.cmp(&b.threads),
            SortColumn::Memory => a.memory.cmp(&b.memory),
            SortColumn::Cpu => a.cpu.total_cmp(&b.cpu),
        };
        let by_key = if self.descending {
            by_key.reverse()
        } else {
            by_key
        };
        by_key.then(a.pid.cmp(&b.pid))
    }
}

/// Processes that match `filter`, sorted, as flat rows.
///
/// The filter is a case-insensitive substring match on the name, the
/// command line, the user, or the exact pid.
fn arrange(processes: &[ProcessSample], filter: &str, order: SortOrder) -> Vec<Row> {
    let needle = filter.trim().to_lowercase();
    let mut indices: Vec<usize> = (0..processes.len())
        .filter(|&index| needle.is_empty() || matches(&processes[index], &needle))
        .collect();
    indices.sort_by(|&a, &b| order.compare(&processes[a], &processes[b]));
    indices
        .into_iter()
        .map(|index| Row {
            index,
            guides: String::new(),
            fold: Fold::Leaf,
        })
        .collect()
}

/// Processes as a parent/child tree, depth first, siblings sorted by
/// `order`.
///
/// A filter keeps each matching process plus every ancestor up to its
/// root, so a match never shows up detached from where it lives. While
/// a filter is active nothing is folded, otherwise a match inside a
/// folded branch would be filtered in but still invisible.
fn arrange_tree(
    processes: &[ProcessSample],
    filter: &str,
    order: SortOrder,
    folded: &HashSet<u32>,
) -> Vec<Row> {
    let index_of: HashMap<u32, usize> = processes
        .iter()
        .enumerate()
        .map(|(index, process)| (process.pid, index))
        .collect();
    let parent_of: Vec<Option<usize>> = processes
        .iter()
        .map(|process| {
            process
                .parent
                .filter(|&parent| parent != process.pid)
                .and_then(|parent| index_of.get(&parent).copied())
        })
        .collect();

    let mut children = vec![Vec::new(); processes.len()];
    let mut roots = Vec::new();
    for (index, parent) in parent_of.iter().enumerate() {
        match parent {
            Some(parent) => children[*parent].push(index),
            // The parent exited before this sample, or is pid 0.
            None => roots.push(index),
        }
    }

    // Parent pids come from separate /proc reads, so pid reuse between
    // two reads can in principle link processes into a loop with no
    // root. Promote one member of each unreachable group to a root;
    // `TreeWalk::visit` skips anything already emitted, which cuts the
    // loop there.
    let mut reachable = vec![false; processes.len()];
    let mut stack = roots.clone();
    while let Some(index) = stack.pop() {
        if !std::mem::replace(&mut reachable[index], true) {
            stack.extend(&children[index]);
        }
    }
    for index in 0..processes.len() {
        if !reachable[index] {
            roots.push(index);
            let mut stack = vec![index];
            while let Some(index) = stack.pop() {
                if !std::mem::replace(&mut reachable[index], true) {
                    stack.extend(&children[index]);
                }
            }
        }
    }

    let by_order = |a: &usize, b: &usize| order.compare(&processes[*a], &processes[*b]);
    roots.sort_by(by_order);
    for siblings in &mut children {
        siblings.sort_by(by_order);
    }

    let needle = filter.trim().to_lowercase();
    let mut keep = vec![needle.is_empty(); processes.len()];
    if !needle.is_empty() {
        for (index, process) in processes.iter().enumerate() {
            if !matches(process, &needle) {
                continue;
            }
            // Stop at the first ancestor already kept: everything above
            // it is kept too. That also ends the walk on a loop.
            let mut current = Some(index);
            while let Some(at) = current {
                if std::mem::replace(&mut keep[at], true) {
                    break;
                }
                current = parent_of[at];
            }
        }
    }

    let mut walk = TreeWalk {
        processes,
        children: &children,
        keep: &keep,
        folded,
        folding: needle.is_empty(),
        visited: vec![false; processes.len()],
        rows: Vec::new(),
    };
    for root in roots.into_iter().filter(|&root| keep[root]) {
        walk.visit(root, String::new(), String::new());
    }
    walk.rows
}

struct TreeWalk<'a> {
    processes: &'a [ProcessSample],
    children: &'a [Vec<usize>],
    keep: &'a [bool],
    folded: &'a HashSet<u32>,
    folding: bool,
    visited: Vec<bool>,
    rows: Vec<Row>,
}

impl TreeWalk<'_> {
    /// Emit `index` with `guides` in front of its name, then its kept
    /// children. `continuation` is what the children's guides start
    /// with: a `│` for every ancestor that still has siblings below.
    fn visit(&mut self, index: usize, guides: String, continuation: String) {
        if std::mem::replace(&mut self.visited[index], true) {
            return;
        }
        let kids: Vec<usize> = self.children[index]
            .iter()
            .copied()
            .filter(|&kid| self.keep[kid])
            .collect();
        let fold = if kids.is_empty() {
            Fold::Leaf
        } else if self.folding && self.folded.contains(&self.processes[index].pid) {
            Fold::Closed
        } else {
            Fold::Open
        };
        self.rows.push(Row {
            index,
            guides,
            fold,
        });
        if fold != Fold::Open {
            return;
        }
        let last = kids.len() - 1;
        for (position, kid) in kids.into_iter().enumerate() {
            let (branch, trunk) = if position == last {
                ("└─ ", "   ")
            } else {
                ("├─ ", "│  ")
            };
            self.visit(
                kid,
                format!("{continuation}{branch}"),
                format!("{continuation}{trunk}"),
            );
        }
    }
}

fn matches(process: &ProcessSample, needle: &str) -> bool {
    process.name.to_lowercase().contains(needle)
        || process.command.to_lowercase().contains(needle)
        || process.user.to_lowercase().contains(needle)
        || process.pid.to_string() == needle
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str, cpu: f32) -> ProcessSample {
        ProcessSample {
            pid,
            parent: None,
            name: name.into(),
            command: format!("/usr/bin/{name} --flag"),
            user: "lukas".into(),
            threads: 1,
            memory: u64::from(pid) * 1024,
            cpu,
        }
    }

    fn child(pid: u32, parent: u32, name: &str) -> ProcessSample {
        ProcessSample {
            parent: Some(parent),
            ..process(pid, name, 0.0)
        }
    }

    fn table(processes: Vec<ProcessSample>, sort: SortColumn) -> ProcessTable {
        let mut table = ProcessTable::new(SortOrder::by(sort));
        table.replace(processes);
        table
    }

    fn pids(table: &ProcessTable) -> Vec<u32> {
        (0..table.len())
            .map(|index| table.row(index).expect("index < len").process.pid)
            .collect()
    }

    fn tree_lines(table: &ProcessTable) -> Vec<String> {
        (0..table.len())
            .map(|index| {
                let row = table.row(index).expect("index < len");
                format!("{}{}", row.guides, row.process.name)
            })
            .collect()
    }

    /// init ─┬─ shell ── editor
    ///       └─ daemon
    fn family() -> ProcessTable {
        let mut table = table(
            vec![
                process(1, "init", 0.0),
                child(10, 1, "shell"),
                child(11, 1, "daemon"),
                child(20, 10, "editor"),
            ],
            SortColumn::Pid,
        );
        table.toggle_layout();
        table
    }

    #[test]
    fn cpu_sorts_busiest_first_and_breaks_ties_on_pid() {
        let table = table(
            vec![
                process(3, "c", 0.1),
                process(1, "a", 0.5),
                process(2, "b", 0.1),
            ],
            SortColumn::Cpu,
        );
        assert_eq!(pids(&table), [1, 2, 3]);
    }

    #[test]
    fn toggling_the_active_column_flips_direction() {
        let mut table = table(
            vec![process(1, "beta", 0.0), process(2, "Alpha", 0.0)],
            SortColumn::Name,
        );
        assert_eq!(pids(&table), [2, 1]);
        table.set_sort(table.sort().toggled(SortColumn::Name));
        assert_eq!(pids(&table), [1, 2]);
    }

    #[test]
    fn filter_matches_command_and_exact_pid() {
        let mut table = table(
            vec![process(12, "firefox", 0.0), process(123, "bash", 0.0)],
            SortColumn::Pid,
        );
        table.push_filter("FLAG");
        assert_eq!(pids(&table), [12, 123]);
        table.clear_filter();
        table.push_filter("fire");
        assert_eq!(pids(&table), [12]);
        table.clear_filter();
        table.push_filter("12");
        assert_eq!(pids(&table), [12]);
    }

    #[test]
    fn selection_survives_a_shorter_process_list() {
        let mut table = table(
            (1..=5).map(|pid| process(pid, "p", 0.0)).collect(),
            SortColumn::Pid,
        );
        assert!(table.select_index(4));

        table.replace(vec![process(1, "p", 0.0), process(2, "p", 0.0)]);
        assert_eq!(table.selected().map(|p| p.pid), Some(2));
    }

    #[test]
    fn selection_follows_pid_across_resort() {
        let mut table = table(
            vec![process(1, "a", 0.1), process(2, "b", 0.9)],
            SortColumn::Pid,
        );
        table.select_pid(1);

        table.set_sort(SortOrder::by(SortColumn::Cpu));
        assert_eq!(table.selected_index(), Some(1));
    }

    #[test]
    fn tree_nests_children_under_parents() {
        assert_eq!(
            tree_lines(&family()),
            ["init", "├─ shell", "│  └─ editor", "└─ daemon"]
        );
    }

    #[test]
    fn folding_hides_descendants_and_unfolding_restores_them() {
        let mut table = family();
        table.select_pid(10);
        assert!(table.toggle_fold());
        assert_eq!(tree_lines(&table), ["init", "├─ shell", "└─ daemon"]);
        assert_eq!(table.row(1).map(|row| row.fold), Some(Fold::Closed));

        assert!(table.toggle_fold());
        assert_eq!(table.len(), 4);
    }

    #[test]
    fn leaves_cannot_fold() {
        let mut table = family();
        table.select_pid(20);
        assert!(!table.toggle_fold());
    }

    #[test]
    fn tree_filter_keeps_the_path_to_each_match() {
        let mut table = family();
        table.push_filter("editor");
        assert_eq!(tree_lines(&table), ["init", "└─ shell", "   └─ editor"]);
    }

    #[test]
    fn tree_filter_reaches_into_folded_branches() {
        let mut table = family();
        table.select_pid(10);
        table.toggle_fold();
        table.push_filter("editor");
        assert_eq!(pids(&table), [1, 10, 20]);
    }

    #[test]
    fn orphans_and_parent_loops_become_roots() {
        let mut table = table(
            vec![child(5, 6, "a"), child(6, 5, "b"), child(7, 99, "orphan")],
            SortColumn::Pid,
        );
        table.toggle_layout();
        let mut shown = pids(&table);
        shown.sort();
        assert_eq!(shown, [5, 6, 7]);
    }

    #[test]
    fn step_wraps_around() {
        assert_eq!(SortColumn::Pid.step(-1), SortColumn::Cpu);
        assert_eq!(SortColumn::Cpu.step(1), SortColumn::Pid);
    }
}
