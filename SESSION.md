# Session notes

- `libc` is pinned to 0.2.186 in `Cargo.lock`. 0.2.191 dropped
  `libc::ENOATTR` on Linux, which breaks `xattr` 0.2.3, pulled in by
  gpui 0.2.2. A plain `cargo update` brings the breakage back.
- sysinfo 0.40 is out; the code targets 0.39.
- After a signal is sent with a filter active, the footer shows the
  filter text, not the "Sent SIGTERM ..." result. The status also never
  expires.
- Column widths in the process table assume ~7.4 px per monospace
  character at `text_xs` (`CH` in `ui.rs`). A font with a
  different advance will make the fixed columns too wide or too narrow.
- Missing compared to btop: per-process detail view,
  per-interface network selection, battery.
- Tree view folds only with `space`. Clicking the `▸`/`▾` marker selects
  the row but doesn't fold it.
- The refresh interval resets to 1 s on every start. Nothing persists
  settings yet.
- The process list re-renders every row on screen once per second, and
  `ProcessTable::update` lower-cases every name and command line for
  each filter keystroke. Fine at ~600 processes; untested at 10k.
- Graphs jump one sample at a time. Smooth scrolling was tried and
  dropped because it didn't look good. It cost 8.5% of a core at
  30 fps with cached boxes, against 2.8% for jumping. The diff is in
  `tmp/smooth-scroll.patch`.
- Graph fill opacity (`FILL_OPACITY`), line widths and the grid color
  were tuned by eye on one dark screen at scale 1.0. Not checked on
  HiDPI.
- Themes use only part of a btop theme: `graph_text`, `proc_misc` and
  `inactive_fg` (beyond the meter fallback) are ignored, and the
  label color is a blend of `main_fg` and `main_bg`, not a theme key.
- GPU readings poll `gpu_busy_percent` every sample. On laptops with a
  runtime-suspended discrete AMD GPU, that read may wake the GPU and
  cost battery. Not checked; this machine is a desktop.
- With the "cpu lazy" sort, short-lived processes (here a `kubectl`
  spawned every ~5 s) briefly rank near the top. Their lifetime average
  is high because they are young, and each one pushes every row below
  it down a line. btop behaves the same. A minimum age in the lifetime
  average would damp it.
- The `Cpu%` column shows current usage while "cpu lazy" sorts by
  lifetime usage, so the column doesn't look sorted. btop does the same.
- Reading per-process disk I/O for the detail panel costs about 3 ms
  per process refresh (18.8 vs 15.7 ms for ~540 processes), for every
  process, although only the one in the panel needs it.
- Once, in a headless test with a filter active and the detail panel
  open, faint text from another row showed through the list header.
  Not reproduced since.
